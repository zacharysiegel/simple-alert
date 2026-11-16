use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use ureq::Agent;
use ureq::Body;
use ureq::http::Response;

use crate::error::AppError;

const TELEGRAM_API_BASE_URL: &str = "https://api.telegram.org";
const REDACTED_TOKEN: &str = "<redacted>";
const MAX_ATTEMPTS: u32 = 3;
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_HONORED_RETRY_AFTER: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_TOO_MANY_REQUESTS: u16 = 429;
// Telegram answers 401 for an unknown bot token and 404 for a malformed one.
const HTTP_UNAUTHORIZED: u16 = 401;
const HTTP_NOT_FOUND: u16 = 404;
const GET_ME_METHOD: &str = "getMe";
const GROUP_CHAT_TYPES: [&str; 3] = ["channel", "group", "supergroup"];

#[derive(Clone)]
pub struct BotToken(String);

impl BotToken {
    pub fn parse(raw: &str) -> Result<BotToken, AppError> {
        let trimmed: &str = raw.trim();
        let valid: bool = match trimmed.split_once(':') {
            Some((bot_id, secret)) => {
                !bot_id.is_empty()
                    && bot_id.chars().all(|character| character.is_ascii_digit())
                    && !secret.is_empty()
                    && secret.chars().all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
            }
            None => false,
        };

        if !valid {
            return Err(AppError::new(
                "the bot token is not in the form <digits>:<letters, digits, '-', '_'> that @BotFather issues",
            ));
        }

        Ok(BotToken(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn redact(&self, text: &str) -> String {
        text.replace(&self.0, REDACTED_TOKEN)
    }
}

pub struct TelegramClient {
    agent: Agent,
    token: BotToken,
}

impl TelegramClient {
    pub fn send_message(&self, chat_id: i64, text: &str) -> Result<(), AppError> {
        let body: Value = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });

        self.call("sendMessage", &body)?;

        Ok(())
    }

    pub fn check_token(&self) -> Result<TokenCheck, AppError> {
        let settled_call: SettledCall = self.settle(GET_ME_METHOD, &serde_json::json!({}));
        let get_me_outcome: GetMeOutcome = classify_get_me_outcome(&settled_call.outcome);

        if get_me_outcome == GetMeOutcome::TokenRejected {
            return Ok(TokenCheck::Rejected);
        }

        let result: Value = self.finish(GET_ME_METHOD, settled_call)?;
        let bot_user: BotUser = serde_json::from_value(result)?;

        Ok(TokenCheck::Accepted(bot_user))
    }

    pub fn get_updates(&self) -> Result<Vec<Update>, AppError> {
        let result: Value = self.call("getUpdates", &serde_json::json!({}))?;

        parse_updates(result)
    }

    fn call(&self, method: &str, body: &Value) -> Result<Value, AppError> {
        let settled_call: SettledCall = self.settle(method, body);

        self.finish(method, settled_call)
    }

    fn settle(&self, method: &str, body: &Value) -> SettledCall {
        let mut attempt_number: u32 = 1;

        loop {
            let outcome: AttemptOutcome = self.attempt(method, body);
            let decision: RetryDecision = decide_retry(&outcome);

            let delay: Duration = match decision {
                RetryDecision::Stop => {
                    return SettledCall {
                        outcome,
                        attempt_count: attempt_number,
                    };
                }
                RetryDecision::RetryAfter(_) if attempt_number >= MAX_ATTEMPTS => {
                    return SettledCall {
                        outcome,
                        attempt_count: attempt_number,
                    };
                }
                RetryDecision::RetryAfter(delay) => delay,
            };

            std::thread::sleep(delay);
            attempt_number += 1;
        }
    }

    fn attempt(&self, method: &str, body: &Value) -> AttemptOutcome {
        let url: String = format!("{}/bot{}/{}", TELEGRAM_API_BASE_URL, self.token.as_str(), method);

        let sent: Result<Response<Body>, ureq::Error> = self.agent.post(&url).send_json(body);
        let mut response: Response<Body> = match sent {
            Ok(response) => response,
            Err(error) => return AttemptOutcome::NetworkFailure(error.to_string()),
        };

        let status: u16 = response.status().as_u16();
        let read: Result<String, ureq::Error> = response.body_mut().read_to_string();

        match read {
            Ok(response_text) => classify_response(status, &response_text),
            Err(error) => AttemptOutcome::NetworkFailure(error.to_string()),
        }
    }

    fn finish(&self, method: &str, settled_call: SettledCall) -> Result<Value, AppError> {
        let failure_message: String = match settled_call.outcome {
            AttemptOutcome::Delivered(result) => return Ok(result),
            AttemptOutcome::NetworkFailure(detail) => format!("could not reach Telegram for {}: {}", method, detail),
            AttemptOutcome::Rejected(rejection) => render_rejection(method, &rejection),
        };

        let attempts_note: String = match settled_call.attempt_count {
            1 => String::new(),
            count => format!(" (after {} attempts)", count),
        };

        Err(AppError::new(&self.token.redact(&format!("{}{}", failure_message, attempts_note))))
    }
}

pub fn create_client(token: BotToken) -> TelegramClient {
    let agent: Agent = Agent::new_with_config(
        Agent::config_builder()
            .http_status_as_error(false)
            .https_only(true)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build(),
    );

    TelegramClient {
        agent,
        token,
    }
}

#[derive(Debug, PartialEq)]
pub enum AttemptOutcome {
    Delivered(Value),
    NetworkFailure(String),
    Rejected(TelegramRejection),
}

struct SettledCall {
    outcome: AttemptOutcome,
    attempt_count: u32,
}

pub enum TokenCheck {
    Accepted(BotUser),
    Rejected,
}

#[derive(Debug, PartialEq)]
pub enum GetMeOutcome {
    BotFound,
    TokenRejected,
    Failed,
}

pub fn classify_get_me_outcome(outcome: &AttemptOutcome) -> GetMeOutcome {
    match outcome {
        AttemptOutcome::Delivered(_) => GetMeOutcome::BotFound,
        AttemptOutcome::Rejected(rejection) if [HTTP_UNAUTHORIZED, HTTP_NOT_FOUND].contains(&rejection.status) => {
            GetMeOutcome::TokenRejected
        }
        AttemptOutcome::Rejected(_) | AttemptOutcome::NetworkFailure(_) => GetMeOutcome::Failed,
    }
}

#[derive(Debug, PartialEq)]
pub struct TelegramRejection {
    pub status: u16,
    pub description: String,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, PartialEq)]
pub enum RetryDecision {
    Stop,
    RetryAfter(Duration),
}

pub fn decide_retry(outcome: &AttemptOutcome) -> RetryDecision {
    match outcome {
        AttemptOutcome::Delivered(_) => RetryDecision::Stop,
        AttemptOutcome::NetworkFailure(_) => RetryDecision::RetryAfter(DEFAULT_RETRY_DELAY),
        AttemptOutcome::Rejected(rejection) if rejection.status == HTTP_TOO_MANY_REQUESTS => {
            decide_rate_limited_retry(rejection.retry_after_seconds)
        }
        AttemptOutcome::Rejected(rejection) if rejection.status >= 500 => RetryDecision::RetryAfter(DEFAULT_RETRY_DELAY),
        AttemptOutcome::Rejected(_) => RetryDecision::Stop,
    }
}

fn decide_rate_limited_retry(retry_after_seconds: Option<u64>) -> RetryDecision {
    let Some(seconds) = retry_after_seconds else {
        return RetryDecision::RetryAfter(DEFAULT_RETRY_DELAY);
    };

    let retry_after: Duration = Duration::from_secs(seconds);

    if retry_after > MAX_HONORED_RETRY_AFTER {
        return RetryDecision::Stop;
    }

    RetryDecision::RetryAfter(retry_after)
}

#[derive(Deserialize)]
struct TelegramResponse {
    ok: bool,
    result: Option<Value>,
    description: Option<String>,
    parameters: Option<ResponseParameters>,
}

#[derive(Deserialize)]
struct ResponseParameters {
    retry_after: Option<u64>,
}

pub fn classify_response(status: u16, response_text: &str) -> AttemptOutcome {
    let parsed: Result<TelegramResponse, serde_json::Error> = serde_json::from_str(response_text);

    let Ok(response) = parsed else {
        return AttemptOutcome::Rejected(TelegramRejection {
            status,
            description: "the response body is not a Telegram Bot API response".to_string(),
            retry_after_seconds: None,
        });
    };

    let succeeded: bool = response.ok && (200..300).contains(&status);

    if succeeded {
        return AttemptOutcome::Delivered(response.result.unwrap_or(Value::Null));
    }

    AttemptOutcome::Rejected(TelegramRejection {
        status,
        description: response.description.unwrap_or_else(|| "no description".to_string()),
        retry_after_seconds: response.parameters.and_then(|parameters| parameters.retry_after),
    })
}

fn render_rejection(method: &str, rejection: &TelegramRejection) -> String {
    let retry_after_note: String = match rejection.retry_after_seconds {
        Some(seconds) => format!("; Telegram asked to retry after {} seconds", seconds),
        None => String::new(),
    };

    format!(
        "Telegram rejected {} with HTTP {}: {}{}",
        method, rejection.status, rejection.description, retry_after_note,
    )
}

#[derive(Debug, Deserialize)]
pub struct BotUser {
    pub id: i64,
    pub first_name: String,
    pub username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<UpdateMessage>,
    pub channel_post: Option<UpdateMessage>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMessage {
    pub chat: Chat,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    pub title: Option<String>,
}

pub fn parse_updates(result: Value) -> Result<Vec<Update>, AppError> {
    let updates: Vec<Update> = serde_json::from_value(result)?;

    Ok(updates)
}

/// Channel and group chats in first-seen order, each once.
pub fn get_distinct_group_chats(updates: &[Update]) -> Vec<Chat> {
    let mut chats: Vec<Chat> = Vec::new();

    let seen_chats: Vec<&Chat> = updates
        .iter()
        .flat_map(|update| [update.channel_post.as_ref(), update.message.as_ref()])
        .flatten()
        .map(|update_message| &update_message.chat)
        .collect();

    for chat in seen_chats {
        let is_group_chat: bool = GROUP_CHAT_TYPES.contains(&chat.chat_type.as_str());
        let already_listed: bool = chats.iter().any(|listed_chat| listed_chat.id == chat.id);

        if is_group_chat && !already_listed {
            chats.push(chat.clone());
        }
    }

    chats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_rejection(status: u16, retry_after_seconds: Option<u64>) -> AttemptOutcome {
        AttemptOutcome::Rejected(TelegramRejection {
            status,
            description: "description".to_string(),
            retry_after_seconds,
        })
    }

    #[test]
    fn decide_retry_stops_after_a_delivery() {
        assert_eq!(decide_retry(&AttemptOutcome::Delivered(Value::Bool(true))), RetryDecision::Stop);
    }

    #[test]
    fn decide_retry_retries_a_network_failure_after_the_default_delay() {
        let outcome: AttemptOutcome = AttemptOutcome::NetworkFailure("connection failed".to_string());

        assert_eq!(decide_retry(&outcome), RetryDecision::RetryAfter(DEFAULT_RETRY_DELAY));
    }

    #[test]
    fn decide_retry_waits_the_retry_after_telegram_gives_for_a_429() {
        assert_eq!(decide_retry(&get_rejection(429, Some(5))), RetryDecision::RetryAfter(Duration::from_secs(5)));
    }

    #[test]
    fn decide_retry_stops_when_a_429_asks_for_a_longer_wait_than_is_honored() {
        let retry_after_seconds: u64 = MAX_HONORED_RETRY_AFTER.as_secs() + 1;

        assert_eq!(decide_retry(&get_rejection(429, Some(retry_after_seconds))), RetryDecision::Stop);
    }

    #[test]
    fn decide_retry_retries_a_server_error() {
        assert_eq!(decide_retry(&get_rejection(502, None)), RetryDecision::RetryAfter(DEFAULT_RETRY_DELAY));
    }

    #[test]
    fn decide_retry_stops_after_another_client_error() {
        assert_eq!(decide_retry(&get_rejection(400, None)), RetryDecision::Stop);
    }

    #[test]
    fn classify_get_me_outcome_treats_unauthorized_and_not_found_as_a_rejected_token() {
        assert_eq!(classify_get_me_outcome(&get_rejection(401, None)), GetMeOutcome::TokenRejected);
        assert_eq!(classify_get_me_outcome(&get_rejection(404, None)), GetMeOutcome::TokenRejected);
    }

    #[test]
    fn classify_get_me_outcome_does_not_treat_a_network_failure_as_a_rejected_token() {
        let outcome: AttemptOutcome = AttemptOutcome::NetworkFailure("connection failed".to_string());

        assert_eq!(classify_get_me_outcome(&outcome), GetMeOutcome::Failed);
    }

    #[test]
    fn classify_response_reads_the_retry_after_parameter() {
        let response_text: &str =
            r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#;

        let outcome: AttemptOutcome = classify_response(429, response_text);

        let expected: AttemptOutcome = AttemptOutcome::Rejected(TelegramRejection {
            status: 429,
            description: "Too Many Requests: retry after 7".to_string(),
            retry_after_seconds: Some(7),
        });
        assert_eq!(outcome, expected);
    }

    #[test]
    fn classify_response_treats_an_unparseable_body_as_a_rejection_with_its_status() {
        let outcome: AttemptOutcome = classify_response(502, "<html>Bad Gateway</html>");

        assert!(matches!(outcome, AttemptOutcome::Rejected(TelegramRejection { status: 502, .. })));
    }

    #[test]
    fn redact_replaces_every_occurrence_of_the_token() {
        let token: BotToken = BotToken::parse("123456789:AAFakeTokenForTests_-x").unwrap();
        let text: String = format!(
            "bad uri: https://api.telegram.org/bot{}/sendMessage ({})",
            token.as_str(),
            token.as_str(),
        );

        let redacted: String = token.redact(&text);

        assert_eq!(redacted, "bad uri: https://api.telegram.org/bot<redacted>/sendMessage (<redacted>)");
    }

    #[test]
    fn parse_rejects_a_token_with_characters_outside_the_issued_alphabet() {
        assert!(BotToken::parse("123456789:AA/../getMe").is_err());
    }

    #[test]
    fn get_distinct_group_chats_lists_each_channel_and_group_once_and_skips_private_chats() {
        let response_text: &str = r#"[
            {"update_id": 1, "channel_post": {"message_id": 10, "date": 1, "chat": {"id": -1001111111111, "title": "Alerts", "type": "channel"}, "text": "hello"}},
            {"update_id": 2, "message": {"message_id": 11, "date": 2, "chat": {"id": 42, "first_name": "Zachary", "type": "private"}, "text": "hi"}},
            {"update_id": 3, "channel_post": {"message_id": 12, "date": 3, "chat": {"id": -1001111111111, "title": "Alerts", "type": "channel"}, "text": "again"}},
            {"update_id": 4, "message": {"message_id": 13, "date": 4, "chat": {"id": -1002222222222, "title": "Ops", "type": "supergroup"}, "text": "x"}},
            {"update_id": 5, "my_chat_member": {"chat": {"id": -1003333333333, "title": "Other", "type": "channel"}}}
        ]"#;
        let updates: Vec<Update> = parse_updates(serde_json::from_str(response_text).unwrap()).unwrap();

        let chats: Vec<Chat> = get_distinct_group_chats(&updates);

        let expected: Vec<Chat> = vec![
            Chat {
                id: -1001111111111,
                chat_type: "channel".to_string(),
                title: Some("Alerts".to_string()),
            },
            Chat {
                id: -1002222222222,
                chat_type: "supergroup".to_string(),
                title: Some("Ops".to_string()),
            },
        ];
        assert_eq!(chats, expected);
    }
}
