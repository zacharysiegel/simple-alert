use std::path::Path;
use std::path::PathBuf;

use simple_alert::alert::Alert;
use simple_alert::config;
use simple_alert::config::Config;
use simple_alert::config::SecretsLocation;
use simple_alert::error::AppError;
use simple_alert::private_file;
use simple_alert::secrets;
use simple_alert::secrets::StoreContents;
use simple_alert::send;
use simple_alert::telegram;
use simple_alert::telegram::BotToken;
use simple_alert::telegram::BotUser;
use simple_alert::telegram::Chat;
use simple_alert::telegram::TelegramClient;
use simple_alert::telegram::Update;
use toml::Table;

use crate::prompt;
use crate::prompt::Confirmation;

const DEFAULT_SECRETS_STORE: &str = "~/.config/simple-alert/secrets.yaml";

#[derive(Debug, PartialEq)]
pub enum KeySource {
    ExistingConfig,
    Generated,
}

#[derive(Debug, PartialEq)]
pub struct SecretsPlan {
    pub location: SecretsLocation,
    pub key_source: KeySource,
}

#[derive(Debug, PartialEq)]
pub enum TokenStorage {
    Add,
    AlreadyStored,
    ReplaceDifferent,
}

pub fn run(config_path: &Path, explicit_config_path: Option<&Path>) -> Result<(), AppError> {
    let secrets_plan: SecretsPlan = read_secrets_plan(config_path)?;
    let store_path: PathBuf = secrets_plan.location.store_path();
    let store_contents: StoreContents = secrets::read_store_contents(&store_path)?;
    require_store_usable_with_key(&secrets_plan.key_source, &store_contents, &store_path)?;

    let stored_token: Option<String> =
        secrets::read_optional_secret(&secrets_plan.location, secrets::BOT_TOKEN_SECRET_NAME)?;

    let token: BotToken = prompt_bot_token()?;
    let telegram_client: TelegramClient = telegram::create_client(token.clone());

    let bot_user: BotUser = telegram_client.get_me()?;
    println!("Bot: {}", render_bot_name(&bot_user));

    let chat_id: i64 = choose_chat(&telegram_client)?;

    let token_storage: TokenStorage = plan_token_storage(stored_token.as_deref(), token.as_str());
    let storage_confirmation: Confirmation = confirm_token_storage(&token_storage, &store_path)?;

    if storage_confirmation == Confirmation::Declined {
        println!("Left {} and {} unchanged.", config_path.display(), store_path.display());

        return Ok(());
    }

    let config: Config = Config {
        chat_id,
        secrets_location: secrets_plan.location,
    };

    private_file::replace_private_file(config_path, &config::render_config(&config))?;
    store_token(&config.secrets_location, &token, &token_storage)?;
    println!("Wrote {} and {}.", config_path.display(), store_path.display());

    offer_test_alert(config_path)?;
    print_usage(explicit_config_path);

    Ok(())
}

fn read_secrets_plan(config_path: &Path) -> Result<SecretsPlan, AppError> {
    let existing_config: String = config::read_existing_config(config_path)?;
    let existing_table: Table = config::parse_table(&existing_config, config_path)?;
    let existing_location: Option<SecretsLocation> = config::find_secrets_location(&existing_table)?;

    Ok(plan_secrets(existing_location, &secrets::generate_key_base64()))
}

pub fn plan_secrets(existing_location: Option<SecretsLocation>, generated_key_base64: &str) -> SecretsPlan {
    match existing_location {
        Some(location) => SecretsPlan {
            location,
            key_source: KeySource::ExistingConfig,
        },
        None => SecretsPlan {
            location: SecretsLocation {
                store: DEFAULT_SECRETS_STORE.to_string(),
                key_base64: generated_key_base64.to_string(),
            },
            key_source: KeySource::Generated,
        },
    }
}

pub fn require_store_usable_with_key(
    key_source: &KeySource,
    store_contents: &StoreContents,
    store_path: &Path,
) -> Result<(), AppError> {
    match (key_source, store_contents) {
        (KeySource::Generated, StoreContents::HasContent) => Err(AppError::new(&format!(
            "config has no [{}] section, so register would generate a new secr key, but {} already holds secrets \
             encrypted with another key; add a [{}] section with {} = \"{}\" and that store's {}, or move {} aside",
            config::SECRETS_SECTION,
            store_path.display(),
            config::SECRETS_SECTION,
            config::SECRETS_STORE_KEY,
            store_path.display(),
            config::SECRETS_KEY_KEY,
            store_path.display(),
        ))),
        _ => Ok(()),
    }
}

pub fn plan_token_storage(stored_token: Option<&str>, new_token: &str) -> TokenStorage {
    match stored_token {
        None => TokenStorage::Add,
        Some(stored) if stored == new_token => TokenStorage::AlreadyStored,
        Some(_) => TokenStorage::ReplaceDifferent,
    }
}

fn confirm_token_storage(token_storage: &TokenStorage, store_path: &Path) -> Result<Confirmation, AppError> {
    if *token_storage != TokenStorage::ReplaceDifferent {
        return Ok(Confirmation::Confirmed);
    }

    prompt::read_confirmation(
        &format!(
            "{} already holds a different {}. Replace it? [y/N]: ",
            store_path.display(),
            secrets::BOT_TOKEN_SECRET_NAME,
        ),
        Confirmation::Declined,
    )
}

fn store_token(location: &SecretsLocation, token: &BotToken, token_storage: &TokenStorage) -> Result<(), AppError> {
    if *token_storage == TokenStorage::AlreadyStored {
        return Ok(());
    }

    let store_path: PathBuf = location.store_path();
    let key: Vec<u8> = secrets::decode_key(&location.key_base64)?;
    let existing_store: String = private_file::read_existing_text(&store_path)?;

    let mut expected_secret_names: Vec<String> = secrets::read_stored_secret_names(&store_path)?;
    expected_secret_names.push(secrets::BOT_TOKEN_SECRET_NAME.to_string());

    let rewritten_store: String =
        secrets::render_store_with_secret(&existing_store, &key, secrets::BOT_TOKEN_SECRET_NAME, token.as_str())?;

    private_file::replace_checked_private_file(&store_path, &rewritten_store, |written_path| {
        secrets::verify_store_file(
            written_path,
            &expected_secret_names,
            &key,
            secrets::BOT_TOKEN_SECRET_NAME,
            token.as_str(),
        )
    })
}

fn prompt_bot_token() -> Result<BotToken, AppError> {
    prompt::read_until_valid(
        || prompt::read_hidden_answer("Bot token from @BotFather (input hidden): "),
        BotToken::parse,
    )
}

fn choose_chat(telegram_client: &TelegramClient) -> Result<i64, AppError> {
    prompt::read_line_answer("Post any message in your channel (the bot must already be an administrator of it), then press Enter: ")?;

    let updates: Vec<Update> = telegram_client.get_updates()?;
    let chats: Vec<Chat> = telegram::get_distinct_group_chats(&updates);

    if chats.is_empty() {
        println!("The bot has not seen a message from any channel or group.");

        return prompt::read_until_valid(
            || prompt::read_line_answer("Chat id (for example -1001234567890): "),
            parse_chat_id_answer,
        );
    }

    println!("Chats the bot has seen:");

    for (index, chat) in chats.iter().enumerate() {
        println!("{}", render_chat_choice(index + 1, chat));
    }

    prompt::read_until_valid(
        || prompt::read_line_answer("Chat number: "),
        |answer| parse_chat_selection(answer, &chats),
    )
}

pub fn render_chat_choice(number: usize, chat: &Chat) -> String {
    let title: &str = chat.title.as_deref().unwrap_or("(untitled)");

    format!("  {}. {} ({}, id {})", number, title, chat.chat_type, chat.id)
}

pub fn parse_chat_selection(answer: &str, chats: &[Chat]) -> Result<i64, AppError> {
    let invalid_message: String = format!("enter a number from 1 to {}", chats.len());

    let number: usize = answer.trim().parse::<usize>().map_err(|_| AppError::new(&invalid_message))?;

    let chat: &Chat = number
        .checked_sub(1)
        .and_then(|index| chats.get(index))
        .ok_or_else(|| AppError::new(&invalid_message))?;

    Ok(chat.id)
}

pub fn parse_chat_id_answer(answer: &str) -> Result<i64, AppError> {
    let chat_id: i64 = answer
        .trim()
        .parse::<i64>()
        .map_err(|_| AppError::new("enter the chat id as an integer, for example -1001234567890"))?;

    config::validate_chat_id(chat_id)?;

    Ok(chat_id)
}

fn render_bot_name(bot_user: &BotUser) -> String {
    match &bot_user.username {
        Some(username) => format!("@{} ({})", username, bot_user.first_name),
        None => bot_user.first_name.clone(),
    }
}

fn offer_test_alert(config_path: &Path) -> Result<(), AppError> {
    let confirmation: Confirmation = prompt::read_confirmation("Send a test alert now? [Y/n]: ", Confirmation::Confirmed)?;

    if confirmation == Confirmation::Declined {
        return Ok(());
    }

    let test_alert: Alert = send::create_test_alert()?;
    send::send_alert(config_path, &test_alert)?;
    println!("Sent \"{}\".", test_alert.render_text());

    Ok(())
}

fn print_usage(explicit_config_path: Option<&Path>) {
    let command_prefix: String = get_command_prefix(explicit_config_path);

    println!(
        "Send alerts with: {} send --service <name> --level <info|warning|error> \"<message>\"",
        command_prefix,
    );
}

pub fn get_command_prefix(explicit_config_path: Option<&Path>) -> String {
    match explicit_config_path {
        Some(path) => format!("simple-alert --config {}", path.display()),
        None => "simple-alert".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_sample_chats() -> Vec<Chat> {
        vec![
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
        ]
    }

    #[test]
    fn require_store_usable_with_key_rejects_a_generated_key_for_a_store_with_content() {
        let secrets_plan: SecretsPlan = plan_secrets(None, &secrets::generate_key_base64());

        let result: Result<(), AppError> = require_store_usable_with_key(
            &secrets_plan.key_source,
            &StoreContents::HasContent,
            Path::new("/tmp/secrets.yaml"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn require_store_usable_with_key_accepts_a_generated_key_for_a_missing_or_blank_store() {
        let secrets_plan: SecretsPlan = plan_secrets(None, &secrets::generate_key_base64());

        let result: Result<(), AppError> = require_store_usable_with_key(
            &secrets_plan.key_source,
            &StoreContents::MissingOrBlank,
            Path::new("/tmp/secrets.yaml"),
        );

        assert!(result.is_ok());
    }

    #[test]
    fn plan_secrets_reuses_an_existing_secrets_location() {
        let existing_location: SecretsLocation = SecretsLocation {
            store: "/srv/secrets.yaml".to_string(),
            key_base64: "EXISTINGKEY".to_string(),
        };

        let secrets_plan: SecretsPlan = plan_secrets(Some(existing_location.clone()), "GENERATEDKEY");

        let expected: SecretsPlan = SecretsPlan {
            location: existing_location,
            key_source: KeySource::ExistingConfig,
        };
        assert_eq!(secrets_plan, expected);
    }

    #[test]
    fn plan_token_storage_asks_to_replace_a_different_stored_token() {
        assert_eq!(plan_token_storage(Some("1:old"), "1:new"), TokenStorage::ReplaceDifferent);
    }

    #[test]
    fn plan_token_storage_leaves_an_identical_stored_token() {
        assert_eq!(plan_token_storage(Some("1:same"), "1:same"), TokenStorage::AlreadyStored);
    }

    #[test]
    fn parse_chat_selection_returns_the_id_of_the_numbered_chat() {
        assert_eq!(parse_chat_selection(" 2 ", &get_sample_chats()).unwrap(), -1002222222222);
    }

    #[test]
    fn parse_chat_selection_rejects_numbers_outside_the_list() {
        let chats: Vec<Chat> = get_sample_chats();

        assert!(parse_chat_selection("0", &chats).is_err());
        assert!(parse_chat_selection("3", &chats).is_err());
    }

    #[test]
    fn parse_chat_selection_rejects_a_non_number() {
        assert!(parse_chat_selection("Alerts", &get_sample_chats()).is_err());
    }

    #[test]
    fn parse_chat_id_answer_accepts_a_negative_channel_id() {
        assert_eq!(parse_chat_id_answer("-1001234567890").unwrap(), -1001234567890);
    }
}
