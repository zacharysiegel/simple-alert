use std::path::Path;

use crate::alert::Alert;
use crate::alert::AlertLevel;
use crate::config::Config;
use crate::error::AppError;
use crate::secrets;
use crate::telegram;
use crate::telegram::BotToken;
use crate::telegram::TelegramClient;

pub const TEST_ALERT_SERVICE: &str = "simple-alert";
pub const TEST_ALERT_MESSAGE: &str = "test alert";

pub struct AlertClient {
    telegram_client: TelegramClient,
    chat_id: i64,
}

impl AlertClient {
    pub fn send(&self, alert: &Alert) -> Result<(), AppError> {
        self.telegram_client.send_message(self.chat_id, &alert.render_text())
    }
}

pub fn create_alert_client(config: &Config) -> Result<AlertClient, AppError> {
    let raw_token: String = secrets::read_required_secret(&config.secrets_location, secrets::BOT_TOKEN_SECRET_NAME)?;
    let token: BotToken = BotToken::parse(&raw_token)?;

    Ok(AlertClient {
        telegram_client: telegram::create_client(token),
        chat_id: config.chat_id,
    })
}

pub fn send_alert(config_path: &Path, alert: &Alert) -> Result<(), AppError> {
    let config: Config = Config::load(config_path)?;
    let alert_client: AlertClient = create_alert_client(&config)?;

    alert_client.send(alert)
}

pub fn create_test_alert() -> Result<Alert, AppError> {
    Alert::new(TEST_ALERT_SERVICE, AlertLevel::Info, TEST_ALERT_MESSAGE)
}
