use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use toml::Table;
use toml::Value;

use crate::alert;
use crate::error::AppError;
use crate::private_file;
use crate::secrets;

pub const SERVICES_SECTION: &str = "services";
pub const CHAT_ID_KEY: &str = "chat_id";
pub const SECRETS_SECTION: &str = "secrets";
pub const SECRETS_STORE_KEY: &str = "store";
pub const SECRETS_KEY_KEY: &str = "key";
pub const LEGACY_TELEGRAM_SECTION: &str = "telegram";

const SERVICE_KEYS: [&str; 1] = [CHAT_ID_KEY];
const SECRETS_KEYS: [&str; 2] = [SECRETS_STORE_KEY, SECRETS_KEY_KEY];
const SECTIONS: [&str; 2] = [SERVICES_SECTION, SECRETS_SECTION];

#[derive(Debug, PartialEq)]
pub struct Config {
    pub chat_id_by_service_name: BTreeMap<String, i64>,
    pub secrets_location: SecretsLocation,
}

impl Config {
    pub fn load(config_path: &Path) -> Result<Config, AppError> {
        let text: String = read_required_config(config_path)?;
        let table: Table = parse_table(&text, config_path)?;

        parse_config(&table)
    }
}

#[derive(Clone, PartialEq)]
pub struct SecretsLocation {
    /// As written in the config, before `~/` expansion.
    pub store: String,
    pub key_base64: String,
}

// The key decrypts the stored bot token, so debug output must never include it.
impl std::fmt::Debug for SecretsLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretsLocation")
            .field("store", &self.store)
            .field("key_base64", &"<redacted>")
            .finish()
    }
}

impl SecretsLocation {
    pub fn store_path(&self) -> PathBuf {
        expand_user_path(&self.store)
    }
}

pub fn parse_config(table: &Table) -> Result<Config, AppError> {
    reject_legacy_telegram_section(table)?;
    reject_unknown_keys(table, None, &SECTIONS)?;

    let chat_id_by_service_name: BTreeMap<String, i64> = find_service_chat_ids(table)?;

    let secrets_location: SecretsLocation = find_secrets_location(table)?.ok_or_else(|| {
        AppError::new(&format!(
            "config has no [{}] section (expected keys: {}, {}); run simple-alert register <service>",
            SECRETS_SECTION, SECRETS_STORE_KEY, SECRETS_KEY_KEY,
        ))
    })?;

    Ok(Config {
        chat_id_by_service_name,
        secrets_location,
    })
}

fn reject_legacy_telegram_section(table: &Table) -> Result<(), AppError> {
    if !table.contains_key(LEGACY_TELEGRAM_SECTION) {
        return Ok(());
    }

    Err(AppError::new(&format!(
        "config has the old [{}] {}, but each service now has its own chat; rename [{}] to [services.<service>] \
         to keep that chat for one service, or run \"simple-alert register <service>\" for each service, which \
         removes [{}] and reuses the stored bot token",
        LEGACY_TELEGRAM_SECTION, CHAT_ID_KEY, LEGACY_TELEGRAM_SECTION, LEGACY_TELEGRAM_SECTION,
    )))
}

pub fn find_service_chat_ids(table: &Table) -> Result<BTreeMap<String, i64>, AppError> {
    let found_section: Option<&Table> = get_section(table, SERVICES_SECTION)?;

    let Some(services_section) = found_section else {
        return Ok(BTreeMap::new());
    };

    let mut chat_id_by_service_name: BTreeMap<String, i64> = BTreeMap::new();

    for (service_name, service_value) in services_section {
        let chat_id: i64 = parse_service_table(service_name, service_value)?;
        chat_id_by_service_name.insert(service_name.clone(), chat_id);
    }

    Ok(chat_id_by_service_name)
}

fn parse_service_table(service_name: &str, service_value: &Value) -> Result<i64, AppError> {
    let table_name: String = get_service_table_name(service_name);

    alert::validate_service_name(service_name)
        .map_err(|error| AppError::new(&format!("config table [{}] has an invalid name: {}", table_name, error.message)))?;

    let Value::Table(service_table) = service_value else {
        return Err(AppError::new(&format!("config key '{}' must be a [{}] table", table_name, table_name)));
    };

    parse_chat_id(service_table, &table_name)
}

pub fn get_service_table_name(service_name: &str) -> String {
    format!("{}.{}", SERVICES_SECTION, service_name)
}

pub fn find_secrets_location(table: &Table) -> Result<Option<SecretsLocation>, AppError> {
    let found_section: Option<&Table> = get_section(table, SECRETS_SECTION)?;

    let Some(secrets_section) = found_section else {
        return Ok(None);
    };

    reject_unknown_keys(secrets_section, Some(SECRETS_SECTION), &SECRETS_KEYS)?;

    let store: String = require_non_empty_string(secrets_section, SECRETS_SECTION, SECRETS_STORE_KEY)?;
    let key_base64: String = require_non_empty_string(secrets_section, SECRETS_SECTION, SECRETS_KEY_KEY)?;
    secrets::decode_key(&key_base64)?;

    Ok(Some(SecretsLocation {
        store,
        key_base64,
    }))
}

pub fn validate_chat_id(chat_id: i64) -> Result<(), AppError> {
    if chat_id == 0 {
        return Err(AppError::new(&format!("{} must not be 0", CHAT_ID_KEY)));
    }

    Ok(())
}

fn parse_chat_id(section: &Table, section_name: &str) -> Result<i64, AppError> {
    reject_unknown_keys(section, Some(section_name), &SERVICE_KEYS)?;

    let value: &Value = section
        .get(CHAT_ID_KEY)
        .ok_or_else(|| AppError::new(&format!("[{}] has no {}", section_name, CHAT_ID_KEY)))?;

    let Value::Integer(chat_id) = value else {
        return Err(AppError::new(&format!(
            "[{}] {} must be an integer such as -1001234567890, not {}",
            section_name, CHAT_ID_KEY, value,
        )));
    };

    validate_chat_id(*chat_id).map_err(|error| AppError::new(&format!("[{}] {}", section_name, error.message)))?;

    Ok(*chat_id)
}

fn get_section<'a>(table: &'a Table, section_name: &str) -> Result<Option<&'a Table>, AppError> {
    match table.get(section_name) {
        None => Ok(None),
        Some(Value::Table(section)) => Ok(Some(section)),
        Some(_) => Err(AppError::new(&format!("config key '{}' must be a [{}] section", section_name, section_name))),
    }
}

fn require_non_empty_string(section: &Table, section_name: &str, key: &str) -> Result<String, AppError> {
    let value: Option<&Value> = section.get(key);

    match value {
        Some(Value::String(text)) if !text.trim().is_empty() => Ok(text.clone()),
        Some(_) => Err(AppError::new(&format!("[{}] {} must be a non-empty string", section_name, key))),
        None => Err(AppError::new(&format!("[{}] has no {}", section_name, key))),
    }
}

fn reject_unknown_keys(table: &Table, section_name: Option<&str>, known_keys: &[&str]) -> Result<(), AppError> {
    let unknown_keys: Vec<&str> = table
        .keys()
        .map(String::as_str)
        .filter(|key| !known_keys.contains(key))
        .collect();

    if unknown_keys.is_empty() {
        return Ok(());
    }

    let location: String = match section_name {
        Some(name) => format!("[{}]", name),
        None => "the config".to_string(),
    };

    Err(AppError::new(&format!(
        "unknown keys in {}: {} (expected: {})",
        location,
        unknown_keys.join(", "),
        known_keys.join(", "),
    )))
}

pub fn render_config(config: &Config) -> String {
    let mut sections: Vec<String> = vec![render_secrets_section(&config.secrets_location)];

    for (service_name, chat_id) in &config.chat_id_by_service_name {
        sections.push(format!("[{}]\n{} = {}\n", get_service_table_name(service_name), CHAT_ID_KEY, chat_id));
    }

    sections.join("\n")
}

fn render_secrets_section(secrets_location: &SecretsLocation) -> String {
    let store: Value = Value::String(secrets_location.store.clone());
    let key_base64: Value = Value::String(secrets_location.key_base64.clone());

    format!(
        "[{}]\n{} = {}\n{} = {}\n",
        SECRETS_SECTION, SECRETS_STORE_KEY, store, SECRETS_KEY_KEY, key_base64,
    )
}

pub fn parse_table(text: &str, config_path: &Path) -> Result<Table, AppError> {
    text.parse::<Table>()
        .map_err(|error| AppError::new(&format!("config at {} is not valid TOML: {}", config_path.display(), error)))
}

pub fn default_config_path() -> Result<PathBuf, AppError> {
    let home: String = std::env::var("HOME").map_err(|_| AppError::new("HOME is not set; pass --config explicitly"))?;

    Ok(PathBuf::from(home).join(".config").join("simple-alert").join("config.toml"))
}

pub fn read_required_config(config_path: &Path) -> Result<String, AppError> {
    std::fs::read_to_string(config_path).map_err(|error| {
        AppError::new(&format!(
            "could not read config at {}: {}; run simple-alert register <service>",
            config_path.display(),
            error,
        ))
    })
}

pub fn read_existing_config(config_path: &Path) -> Result<String, AppError> {
    private_file::read_existing_text(config_path)
}

pub fn expand_user_path(raw: &str) -> PathBuf {
    let Some(remainder) = raw.strip_prefix("~/") else {
        return PathBuf::from(raw);
    };

    match std::env::var("HOME") {
        Ok(home) => PathBuf::from(home).join(remainder),
        Err(_) => PathBuf::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use base64::Engine;

    fn get_config_text(service_tables: &str, key_base64: &str) -> String {
        format!(
            "{}\n[secrets]\nstore = \"~/.config/simple-alert/secrets.yaml\"\nkey = \"{}\"\n",
            service_tables, key_base64,
        )
    }

    fn parse_config_text(text: &str) -> Result<Config, AppError> {
        parse_config(&parse_table(text, Path::new("/tmp/config.toml"))?)
    }

    fn get_error_message(text: &str) -> String {
        parse_config_text(text).unwrap_err().message.clone()
    }

    #[test]
    fn parse_config_reads_multiple_service_tables() {
        let key_base64: String = secrets::generate_key_base64();
        let text: String = get_config_text(
            "[services.xost]\nchat_id = -1001111111111\n\n[services.nightly-backup]\nchat_id = -1002222222222\n",
            &key_base64,
        );

        let config: Config = parse_config_text(&text).unwrap();

        let expected: Config = Config {
            chat_id_by_service_name: BTreeMap::from([
                ("xost".to_string(), -1001111111111),
                ("nightly-backup".to_string(), -1002222222222),
            ]),
            secrets_location: SecretsLocation {
                store: "~/.config/simple-alert/secrets.yaml".to_string(),
                key_base64,
            },
        };
        assert_eq!(config, expected);
    }

    #[test]
    fn parse_config_rejects_the_old_telegram_chat_id_format() {
        let text: String = get_config_text("[telegram]\nchat_id = -1001234567890\n", &secrets::generate_key_base64());

        assert!(get_error_message(&text).contains("run \"simple-alert register <service>\" for each service"));
    }

    #[test]
    fn parse_config_rejects_an_invalid_service_table_name() {
        let text: String = get_config_text("[services.\"x ost\"]\nchat_id = -1001234567890\n", &secrets::generate_key_base64());

        assert!(get_error_message(&text).contains("config table [services.x ost] has an invalid name"));
    }

    #[test]
    fn parse_config_rejects_a_missing_chat_id() {
        let text: String = get_config_text("[services.xost]\n", &secrets::generate_key_base64());

        assert!(get_error_message(&text).contains("[services.xost] has no chat_id"));
    }

    #[test]
    fn parse_config_rejects_a_chat_id_that_is_not_an_integer() {
        let text: String = get_config_text("[services.xost]\nchat_id = \"@my_channel\"\n", &secrets::generate_key_base64());

        assert!(get_error_message(&text).contains("must be an integer"));
    }

    #[test]
    fn parse_config_rejects_a_key_that_does_not_decode_to_32_bytes() {
        let short_key_base64: String = secr::BASE64.encode([0u8; 16]);
        let text: String = get_config_text("[services.xost]\nchat_id = -1001234567890\n", &short_key_base64);

        assert!(get_error_message(&text).contains("decodes to 16 bytes"));
    }

    #[test]
    fn parse_config_rejects_a_missing_secrets_section() {
        let message: String = get_error_message("[services.xost]\nchat_id = -1001234567890\n");

        assert!(message.contains("no [secrets] section"));
    }

    #[test]
    fn parse_config_rejects_an_unknown_key() {
        let text: String = get_config_text("[services.xost]\nchat_id = -1001234567890\nchatid = 5\n", &secrets::generate_key_base64());

        assert!(get_error_message(&text).contains("unknown keys in [services.xost]: chatid"));
    }

    #[test]
    fn render_config_is_read_back_by_parse_config() {
        let config: Config = Config {
            chat_id_by_service_name: BTreeMap::from([
                ("xost".to_string(), -1001111111111),
                ("backup".to_string(), -1002222222222),
            ]),
            secrets_location: SecretsLocation {
                store: "/tmp/a \"quoted\" path/secrets.yaml".to_string(),
                key_base64: secrets::generate_key_base64(),
            },
        };

        let parsed: Config = parse_config_text(&render_config(&config)).unwrap();

        assert_eq!(parsed, config);
    }

    #[test]
    fn expand_user_path_leaves_an_absolute_path_unchanged() {
        assert_eq!(expand_user_path("/etc/simple-alert/secrets.yaml"), PathBuf::from("/etc/simple-alert/secrets.yaml"));
    }

    #[test]
    fn expand_user_path_expands_a_home_relative_path() {
        let home: PathBuf = PathBuf::from(std::env::var("HOME").unwrap());

        assert_eq!(expand_user_path("~/a/b"), home.join("a/b"));
    }

    #[test]
    fn secrets_location_debug_output_redacts_the_key() {
        let secrets_location: SecretsLocation = SecretsLocation {
            store: "~/.config/simple-alert/secrets.yaml".to_string(),
            key_base64: "c2VjcmV0LWtleQ==".to_string(),
        };

        let debug_output: String = format!("{:?}", secrets_location);

        assert!(!debug_output.contains("c2VjcmV0LWtleQ=="));
    }
}
