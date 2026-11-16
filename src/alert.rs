use crate::error::AppError;

// Telegram rejects sendMessage text longer than 4096 characters; the limit is fixed on their side.
pub const TELEGRAM_MESSAGE_CHARACTER_LIMIT: usize = 4096;
pub const TRUNCATION_MARKER: &str = "\n[truncated]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertLevel {
    Info,
    Warning,
    Error,
}

impl AlertLevel {
    pub fn tag(self) -> &'static str {
        match self {
            AlertLevel::Info => "INFO",
            AlertLevel::Warning => "WARNING",
            AlertLevel::Error => "ERROR",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    service: String,
    level: AlertLevel,
    message: String,
}

impl Alert {
    pub fn new(service: &str, level: AlertLevel, message: &str) -> Result<Alert, AppError> {
        validate_service_name(service)?;

        let trimmed_message: &str = message.trim();

        if trimmed_message.is_empty() {
            return Err(AppError::new("the alert message is empty"));
        }

        Ok(Alert {
            service: service.to_string(),
            level,
            message: trimmed_message.to_string(),
        })
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    pub fn level(&self) -> AlertLevel {
        self.level
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn render_text(&self) -> String {
        let text: String = format!("{} [{}] {}", self.level.tag(), self.service, self.message);

        truncate_to_telegram_limit(&text)
    }
}

pub fn validate_service_name(service: &str) -> Result<(), AppError> {
    let valid: bool = !service.is_empty()
        && service.chars().all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_');

    if !valid {
        return Err(AppError::new(&format!(
            "service name '{}' must be non-empty and contain only letters, digits, '-', or '_'",
            service,
        )));
    }

    Ok(())
}

pub fn truncate_to_telegram_limit(text: &str) -> String {
    let character_count: usize = text.chars().count();

    if character_count <= TELEGRAM_MESSAGE_CHARACTER_LIMIT {
        return text.to_string();
    }

    let kept_character_count: usize = TELEGRAM_MESSAGE_CHARACTER_LIMIT - TRUNCATION_MARKER.chars().count();
    let kept_text: String = text.chars().take(kept_character_count).collect();

    format!("{}{}", kept_text, TRUNCATION_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_text_prefixes_the_level_tag_and_service() {
        let alert: Alert = Alert::new("xost", AlertLevel::Error, "tweet id 2 was marked failed").unwrap();

        assert_eq!(alert.render_text(), "ERROR [xost] tweet id 2 was marked failed");
    }

    #[test]
    fn new_trims_the_message() {
        let alert: Alert = Alert::new("xost", AlertLevel::Warning, "  disk at 91%\n").unwrap();

        assert_eq!(alert.message(), "disk at 91%");
    }

    #[test]
    fn new_rejects_a_blank_message() {
        assert!(Alert::new("xost", AlertLevel::Info, " \n\t").is_err());
    }

    #[test]
    fn new_rejects_a_service_name_with_other_characters() {
        assert!(Alert::new("x ost", AlertLevel::Info, "message").is_err());
    }

    #[test]
    fn truncate_to_telegram_limit_counts_characters_and_marks_the_truncation() {
        let text: String = "é".repeat(TELEGRAM_MESSAGE_CHARACTER_LIMIT + 1);

        let truncated: String = truncate_to_telegram_limit(&text);

        assert_eq!(truncated.chars().count(), TELEGRAM_MESSAGE_CHARACTER_LIMIT);
        assert!(truncated.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn truncate_to_telegram_limit_keeps_text_at_the_limit_unchanged() {
        let text: String = "é".repeat(TELEGRAM_MESSAGE_CHARACTER_LIMIT);

        assert_eq!(truncate_to_telegram_limit(&text), text);
    }
}
