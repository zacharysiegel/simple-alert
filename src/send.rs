use std::collections::BTreeMap;
use std::path::Path;

use crate::alert::Alert;
use crate::alert::AlertLevel;
use crate::config;
use crate::config::Config;
use crate::config::SecretsLocation;
use crate::error::AppError;
use crate::secrets;
use crate::telegram;
use crate::telegram::BotToken;
use crate::telegram::TelegramClient;

pub const TEST_ALERT_MESSAGE: &str = "test alert";

pub struct AlertClient {
    telegram_client: TelegramClient,
    chat_id_by_service_name: BTreeMap<String, i64>,
}

impl AlertClient {
    pub fn send(&self, alert: &Alert) -> Result<(), AppError> {
        let chat_id: i64 = get_service_chat_id(&self.chat_id_by_service_name, alert.service())?;

        self.telegram_client.send_message(chat_id, &alert.render_text())
    }
}

pub fn create_alert_client(config: &Config) -> Result<AlertClient, AppError> {
    let telegram_client: TelegramClient = create_stored_token_client(&config.secrets_location)?;

    Ok(AlertClient {
        telegram_client,
        chat_id_by_service_name: config.chat_id_by_service_name.clone(),
    })
}

fn create_stored_token_client(secrets_location: &SecretsLocation) -> Result<TelegramClient, AppError> {
    let raw_token: String = secrets::read_required_secret(secrets_location, secrets::BOT_TOKEN_SECRET_NAME)?;
    let token: BotToken = BotToken::parse(&raw_token)?;

    Ok(telegram::create_client(token))
}

pub fn get_service_chat_id(chat_id_by_service_name: &BTreeMap<String, i64>, service_name: &str) -> Result<i64, AppError> {
    let found_chat_id: Option<&i64> = chat_id_by_service_name.get(service_name);

    found_chat_id.copied().ok_or_else(|| {
        AppError::new(&format!(
            "service '{}' has no [{}] table in the config; run \"simple-alert register {}\"",
            service_name,
            config::get_service_table_name(service_name),
            service_name,
        ))
    })
}

pub fn send_alert(config_path: &Path, alert: &Alert) -> Result<(), AppError> {
    let config: Config = Config::load(config_path)?;
    let chat_id: i64 = get_service_chat_id(&config.chat_id_by_service_name, alert.service())?;

    let telegram_client: TelegramClient = create_stored_token_client(&config.secrets_location)?;

    telegram_client.send_message(chat_id, &alert.render_text())
}

pub fn create_test_alert(service_name: &str) -> Result<Alert, AppError> {
    Alert::new(service_name, AlertLevel::Info, TEST_ALERT_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn get_sample_chat_id_by_service_name() -> BTreeMap<String, i64> {
        BTreeMap::from([
            ("xost".to_string(), -1001111111111),
            ("backup".to_string(), -1002222222222),
        ])
    }

    #[test]
    fn get_service_chat_id_picks_the_chat_of_the_alerts_service() {
        let chat_id: i64 = get_service_chat_id(&get_sample_chat_id_by_service_name(), "backup").unwrap();

        assert_eq!(chat_id, -1002222222222);
    }

    #[test]
    fn get_service_chat_id_rejects_an_unregistered_service_and_names_the_register_command() {
        let error: AppError = get_service_chat_id(&get_sample_chat_id_by_service_name(), "web").unwrap_err();

        assert!(error.message.contains("run \"simple-alert register web\""));
    }

    #[test]
    fn send_alert_reports_an_unregistered_service_before_reading_the_missing_token() {
        let directory: PathBuf =
            std::env::temp_dir().join(format!("simple-alert-send-test-{}-unregistered", std::process::id()));
        let config_path: PathBuf = directory.join("config.toml");
        let config_text: String = format!(
            "[secrets]\nstore = \"{}\"\nkey = \"{}\"\n\n[services.xost]\nchat_id = -1001111111111\n",
            directory.join("secrets.yaml").display(),
            secrets::generate_key_base64(),
        );
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&config_path, config_text).unwrap();

        let alert: Alert = Alert::new("web", AlertLevel::Info, "message").unwrap();
        let result: Result<(), AppError> = send_alert(&config_path, &alert);
        std::fs::remove_dir_all(&directory).ok();

        assert!(result.unwrap_err().message.contains("simple-alert register web"));
    }
}
