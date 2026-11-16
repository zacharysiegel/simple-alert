use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use simple_alert::alert;
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
use simple_alert::telegram::TokenCheck;
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

pub enum TokenAcquisition {
    ReuseStored(BotToken),
    PromptForNew,
}

#[derive(Debug, PartialEq)]
pub enum ChatAssignment {
    New,
    ReplaceExisting(i64),
}

pub struct VerifiedBot {
    token: BotToken,
    telegram_client: TelegramClient,
}

pub enum BotResolution {
    Verified(VerifiedBot),
    Declined,
}

#[derive(Debug, PartialEq)]
pub enum FreshWrite {
    Write,
    ConfirmReplacingChangedChat(i64),
}

pub enum ConfigUpdate {
    Write(Config),
    Declined,
}

pub fn run(config_path: &Path, explicit_config_path: Option<&Path>, service_name: &str) -> Result<(), AppError> {
    alert::validate_service_name(service_name)?;

    let existing_table: Table = read_existing_table(config_path)?;
    let existing_chat_id_by_service_name: BTreeMap<String, i64> = config::find_service_chat_ids(&existing_table)?;
    let secrets_plan: SecretsPlan =
        plan_secrets(config::find_secrets_location(&existing_table)?, &secrets::generate_key_base64());

    let store_path: PathBuf = secrets_plan.location.store_path();
    let store_contents: StoreContents = secrets::read_store_contents(&store_path)?;
    require_store_usable_with_key(&secrets_plan.key_source, &store_contents, &store_path)?;

    let chat_assignment: ChatAssignment = plan_chat_assignment(&existing_chat_id_by_service_name, service_name);
    let assignment_confirmation: Confirmation = confirm_chat_assignment(&chat_assignment, service_name)?;

    if assignment_confirmation == Confirmation::Declined {
        println!("Left {} unchanged.", config_path.display());

        return Ok(());
    }

    let stored_token: Option<String> =
        secrets::read_optional_secret(&secrets_plan.location, secrets::BOT_TOKEN_SECRET_NAME)?;
    let token_acquisition: TokenAcquisition = plan_token_acquisition(stored_token.as_deref());
    let bot_resolution: BotResolution = resolve_bot(token_acquisition)?;

    let BotResolution::Verified(verified_bot) = bot_resolution else {
        println!("Left {} and {} unchanged.", config_path.display(), store_path.display());

        return Ok(());
    };

    let seen_chats: Vec<Chat> = collect_seen_chats(&verified_bot.telegram_client, service_name)?;
    let chosen_chat_id: i64 = pick_chat(&seen_chats, &existing_chat_id_by_service_name, service_name)?;

    let token_storage: TokenStorage = plan_token_storage(stored_token.as_deref(), verified_bot.token.as_str());
    let storage_confirmation: Confirmation = confirm_token_storage(&token_storage, &store_path)?;

    if storage_confirmation == Confirmation::Declined {
        println!("Left {} and {} unchanged.", config_path.display(), store_path.display());

        return Ok(());
    }

    let started_chat_id: Option<i64> = existing_chat_id_by_service_name.get(service_name).copied();
    let config_update: ConfigUpdate =
        update_fresh_config(config_path, &secrets_plan, service_name, chosen_chat_id, started_chat_id)?;

    let ConfigUpdate::Write(config) = config_update else {
        println!("Left {} and {} unchanged.", config_path.display(), store_path.display());

        return Ok(());
    };

    private_file::replace_private_file(config_path, &config::render_config(&config))?;
    store_token(&config.secrets_location, &verified_bot.token, &token_storage)?;
    println!("{}", render_written_files(config_path, &store_path, &token_storage));

    offer_test_alert(config_path, service_name)?;
    print_usage(explicit_config_path, service_name);

    Ok(())
}

fn read_existing_table(config_path: &Path) -> Result<Table, AppError> {
    let existing_config: String = config::read_existing_config(config_path)?;

    config::parse_table(&existing_config, config_path)
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

pub fn plan_token_acquisition(stored_token: Option<&str>) -> TokenAcquisition {
    let parsed_token: Option<BotToken> = stored_token.and_then(|stored| BotToken::parse(stored).ok());

    match parsed_token {
        Some(token) => TokenAcquisition::ReuseStored(token),
        None => TokenAcquisition::PromptForNew,
    }
}

pub fn plan_chat_assignment(chat_id_by_service_name: &BTreeMap<String, i64>, service_name: &str) -> ChatAssignment {
    match chat_id_by_service_name.get(service_name) {
        Some(existing_chat_id) => ChatAssignment::ReplaceExisting(*existing_chat_id),
        None => ChatAssignment::New,
    }
}

fn confirm_chat_assignment(chat_assignment: &ChatAssignment, service_name: &str) -> Result<Confirmation, AppError> {
    let ChatAssignment::ReplaceExisting(existing_chat_id) = chat_assignment else {
        return Ok(Confirmation::Confirmed);
    };

    println!(
        "[{}] already posts to chat id {}.",
        config::get_service_table_name(service_name),
        existing_chat_id,
    );

    prompt::read_confirmation("Replace it? [y/N]: ", Confirmation::Declined)
}

fn update_fresh_config(
    config_path: &Path,
    secrets_plan: &SecretsPlan,
    service_name: &str,
    chosen_chat_id: i64,
    started_chat_id: Option<i64>,
) -> Result<ConfigUpdate, AppError> {
    let mut acknowledged_chat_id: Option<i64> = started_chat_id;

    loop {
        let fresh_table: Table = read_existing_table(config_path)?;
        let fresh_secrets_location: Option<SecretsLocation> = config::find_secrets_location(&fresh_table)?;
        require_unchanged_secrets_location(secrets_plan, fresh_secrets_location.as_ref())?;
        let fresh_chat_id_by_service_name: BTreeMap<String, i64> = config::find_service_chat_ids(&fresh_table)?;

        let fresh_write: FreshWrite =
            plan_fresh_write(&fresh_chat_id_by_service_name, service_name, chosen_chat_id, acknowledged_chat_id);

        match fresh_write {
            FreshWrite::Write => {
                let config: Config = create_registered_config(
                    fresh_chat_id_by_service_name,
                    service_name,
                    chosen_chat_id,
                    secrets_plan.location.clone(),
                );

                return Ok(ConfigUpdate::Write(config));
            }
            FreshWrite::ConfirmReplacingChangedChat(fresh_chat_id) => {
                let confirmation: Confirmation = confirm_replacing_changed_chat(service_name, fresh_chat_id)?;

                if confirmation == Confirmation::Declined {
                    return Ok(ConfigUpdate::Declined);
                }

                acknowledged_chat_id = Some(fresh_chat_id);
            }
        }
    }
}

pub fn require_unchanged_secrets_location(
    secrets_plan: &SecretsPlan,
    fresh_location: Option<&SecretsLocation>,
) -> Result<(), AppError> {
    let planned_location_in_config: Option<&SecretsLocation> = match secrets_plan.key_source {
        KeySource::ExistingConfig => Some(&secrets_plan.location),
        KeySource::Generated => None,
    };

    if fresh_location == planned_location_in_config {
        return Ok(());
    }

    Err(AppError::new(&format!(
        "the config's [{}] section changed while register was running; run register again",
        config::SECRETS_SECTION,
    )))
}

pub fn plan_fresh_write(
    fresh_chat_id_by_service_name: &BTreeMap<String, i64>,
    service_name: &str,
    chosen_chat_id: i64,
    acknowledged_chat_id: Option<i64>,
) -> FreshWrite {
    let fresh_chat_id: Option<i64> = fresh_chat_id_by_service_name.get(service_name).copied();

    match fresh_chat_id {
        Some(changed_chat_id) if changed_chat_id != chosen_chat_id && fresh_chat_id != acknowledged_chat_id => {
            FreshWrite::ConfirmReplacingChangedChat(changed_chat_id)
        }
        _ => FreshWrite::Write,
    }
}

fn confirm_replacing_changed_chat(service_name: &str, fresh_chat_id: i64) -> Result<Confirmation, AppError> {
    println!(
        "While register was running, [{}] was changed to post to chat id {}.",
        config::get_service_table_name(service_name),
        fresh_chat_id,
    );

    prompt::read_confirmation("Replace it? [y/N]: ", Confirmation::Declined)
}

pub fn create_registered_config(
    existing_chat_id_by_service_name: BTreeMap<String, i64>,
    service_name: &str,
    chat_id: i64,
    secrets_location: SecretsLocation,
) -> Config {
    let mut chat_id_by_service_name: BTreeMap<String, i64> = existing_chat_id_by_service_name;
    chat_id_by_service_name.insert(service_name.to_string(), chat_id);

    Config {
        chat_id_by_service_name,
        secrets_location,
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

fn resolve_bot(token_acquisition: TokenAcquisition) -> Result<BotResolution, AppError> {
    let TokenAcquisition::ReuseStored(stored_token) = token_acquisition else {
        return Ok(BotResolution::Verified(prompt_for_verified_bot()?));
    };

    let telegram_client: TelegramClient = telegram::create_client(stored_token.clone());
    let token_check: TokenCheck = telegram_client.check_token()?;

    match token_check {
        TokenCheck::Accepted(bot_user) => {
            println!("Using the stored bot token: {}", render_bot_name(&bot_user));

            Ok(BotResolution::Verified(VerifiedBot {
                token: stored_token,
                telegram_client,
            }))
        }
        TokenCheck::Rejected => offer_replacing_rejected_token(),
    }
}

fn offer_replacing_rejected_token() -> Result<BotResolution, AppError> {
    println!("Telegram rejected the stored bot token.");

    let confirmation: Confirmation =
        prompt::read_confirmation("Enter a new bot token? [y/N]: ", Confirmation::Declined)?;

    if confirmation == Confirmation::Declined {
        return Ok(BotResolution::Declined);
    }

    Ok(BotResolution::Verified(prompt_for_verified_bot()?))
}

fn prompt_for_verified_bot() -> Result<VerifiedBot, AppError> {
    loop {
        let token: BotToken = prompt_bot_token()?;
        let telegram_client: TelegramClient = telegram::create_client(token.clone());
        let token_check: TokenCheck = telegram_client.check_token()?;

        match token_check {
            TokenCheck::Accepted(bot_user) => {
                println!("Bot: {}", render_bot_name(&bot_user));

                return Ok(VerifiedBot {
                    token,
                    telegram_client,
                });
            }
            TokenCheck::Rejected => println!("Telegram rejected that bot token."),
        }
    }
}

fn prompt_bot_token() -> Result<BotToken, AppError> {
    prompt::read_until_valid(
        || prompt::read_hidden_answer("Bot token from @BotFather (input hidden): "),
        BotToken::parse,
    )
}

fn collect_seen_chats(telegram_client: &TelegramClient, service_name: &str) -> Result<Vec<Chat>, AppError> {
    prompt::read_line_answer(&format!(
        "Post any message in the channel for {} (the bot must already be an administrator of it), then press Enter: ",
        service_name,
    ))?;

    let updates: Vec<Update> = telegram_client.get_updates()?;

    Ok(telegram::get_distinct_group_chats(&updates))
}

fn pick_chat(
    seen_chats: &[Chat],
    chat_id_by_service_name: &BTreeMap<String, i64>,
    service_name: &str,
) -> Result<i64, AppError> {
    if seen_chats.is_empty() {
        println!("The bot has not seen a message from any channel or group.");

        return prompt::read_until_valid(
            || prompt::read_line_answer("Chat id (for example -1001234567890): "),
            |answer| {
                parse_chat_id_answer(answer)
            },
        );
    }

    println!("Chats the bot has seen:");

    for (index, chat) in seen_chats.iter().enumerate() {
        let other_service_name: Option<&str> =
            find_other_service_using_chat(chat_id_by_service_name, service_name, chat.id);
        println!("{}", render_chat_choice(index + 1, chat, other_service_name));
    }

    prompt::read_until_valid(
        || prompt::read_line_answer("Chat number, or a chat id such as -1001234567890: "),
        |answer| {
            parse_chat_number_or_id(answer, seen_chats)
        },
    )
}

// Channel and group chat ids are always negative, so a leading '-' cannot be a list number.
fn parse_chat_number_or_id(answer: &str, seen_chats: &[Chat]) -> Result<i64, AppError> {
    let trimmed_answer: &str = answer.trim();

    if trimmed_answer.starts_with('-') {
        return parse_chat_id_answer(trimmed_answer);
    }

    parse_chat_selection(trimmed_answer, seen_chats)
}

pub fn find_other_service_using_chat<'a>(
    chat_id_by_service_name: &'a BTreeMap<String, i64>,
    service_name: &str,
    chat_id: i64,
) -> Option<&'a str> {
    chat_id_by_service_name
        .iter()
        .find(|(candidate_service_name, candidate_chat_id)| {
            candidate_service_name.as_str() != service_name && **candidate_chat_id == chat_id
        })
        .map(|(other_service_name, _)| other_service_name.as_str())
}

pub fn render_chat_choice(number: usize, chat: &Chat, other_service_name: Option<&str>) -> String {
    let title: &str = chat.title.as_deref().unwrap_or("(untitled)");

    let usage_note: String = match other_service_name {
        Some(other_service_name) => format!(" (used by {})", other_service_name),
        None => String::new(),
    };

    format!("  {}. {} ({}, id {}){}", number, title, chat.chat_type, chat.id, usage_note)
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

fn render_written_files(config_path: &Path, store_path: &Path, token_storage: &TokenStorage) -> String {
    match token_storage {
        TokenStorage::AlreadyStored => format!("Wrote {}.", config_path.display()),
        TokenStorage::Add | TokenStorage::ReplaceDifferent => {
            format!("Wrote {} and {}.", config_path.display(), store_path.display())
        }
    }
}

fn offer_test_alert(config_path: &Path, service_name: &str) -> Result<(), AppError> {
    let confirmation: Confirmation = prompt::read_confirmation("Send a test alert now? [Y/n]: ", Confirmation::Confirmed)?;

    if confirmation == Confirmation::Declined {
        return Ok(());
    }

    let test_alert: Alert = send::create_test_alert(service_name)?;
    send::send_alert(config_path, &test_alert)?;
    println!("Sent \"{}\".", test_alert.render_text());

    Ok(())
}

fn print_usage(explicit_config_path: Option<&Path>, service_name: &str) {
    let command_prefix: String = get_command_prefix(explicit_config_path);

    println!(
        "Send alerts with: {} send --service {} --level <info|warning|error> \"<message>\"",
        command_prefix, service_name,
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

    fn get_sample_chat_id_by_service_name() -> BTreeMap<String, i64> {
        BTreeMap::from([
            ("xost".to_string(), -1001111111111),
            ("backup".to_string(), -1002222222222),
        ])
    }

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
    fn plan_token_acquisition_reuses_a_stored_token() {
        let token_acquisition: TokenAcquisition = plan_token_acquisition(Some("123456:ABC-def_ghi"));

        assert!(matches!(token_acquisition, TokenAcquisition::ReuseStored(token) if token.as_str() == "123456:ABC-def_ghi"));
    }

    #[test]
    fn plan_chat_assignment_asks_to_replace_the_services_existing_chat() {
        assert_eq!(
            plan_chat_assignment(&get_sample_chat_id_by_service_name(), "xost"),
            ChatAssignment::ReplaceExisting(-1001111111111),
        );
    }

    #[test]
    fn create_registered_config_preserves_other_services() {
        let secrets_location: SecretsLocation = SecretsLocation {
            store: "/tmp/secrets.yaml".to_string(),
            key_base64: secrets::generate_key_base64(),
        };

        let registered_config: Config = create_registered_config(
            get_sample_chat_id_by_service_name(),
            "web",
            -1003333333333,
            secrets_location.clone(),
        );

        let expected: Config = Config {
            chat_id_by_service_name: BTreeMap::from([
                ("xost".to_string(), -1001111111111),
                ("backup".to_string(), -1002222222222),
                ("web".to_string(), -1003333333333),
            ]),
            secrets_location,
        };
        assert_eq!(registered_config, expected);
    }

    #[test]
    fn render_chat_choice_labels_a_chat_another_service_uses() {
        let chat: Chat = get_sample_chats().remove(0);

        assert_eq!(
            render_chat_choice(1, &chat, Some("xost")),
            "  1. Alerts (channel, id -1001111111111) (used by xost)",
        );
    }

    #[test]
    fn plan_fresh_write_asks_before_replacing_a_chat_set_while_waiting() {
        let fresh_write: FreshWrite =
            plan_fresh_write(&get_sample_chat_id_by_service_name(), "xost", -1003333333333, None);

        assert_eq!(fresh_write, FreshWrite::ConfirmReplacingChangedChat(-1001111111111));
    }

    #[test]
    fn plan_fresh_write_replaces_the_chat_read_at_the_start_without_asking_again() {
        let fresh_write: FreshWrite =
            plan_fresh_write(&get_sample_chat_id_by_service_name(), "xost", -1003333333333, Some(-1001111111111));

        assert_eq!(fresh_write, FreshWrite::Write);
    }

    #[test]
    fn require_unchanged_secrets_location_rejects_a_secrets_section_added_while_waiting() {
        let secrets_plan: SecretsPlan = plan_secrets(None, &secrets::generate_key_base64());
        let fresh_location: SecretsLocation = SecretsLocation {
            store: "/tmp/secrets.yaml".to_string(),
            key_base64: secrets::generate_key_base64(),
        };

        assert!(require_unchanged_secrets_location(&secrets_plan, Some(&fresh_location)).is_err());
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
    fn parse_chat_number_or_id_accepts_a_typed_chat_id_instead_of_a_list_number() {
        assert_eq!(parse_chat_number_or_id("-1009999999999", &get_sample_chats()).unwrap(), -1009999999999);
    }

    #[test]
    fn parse_chat_id_answer_accepts_a_negative_channel_id() {
        assert_eq!(parse_chat_id_answer("-1001234567890").unwrap(), -1001234567890);
    }
}
