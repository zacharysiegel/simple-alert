use std::path::Path;

use base64::Engine;

use crate::config::SecretsLocation;
use crate::error::AppError;
use crate::private_file;

pub const BOT_TOKEN_SECRET_NAME: &str = "telegram_bot_token";

const SECR_KEY_LENGTH_BYTES: usize = 32;

#[derive(Debug, PartialEq)]
pub enum StoreContents {
    MissingOrBlank,
    HasContent,
}

#[derive(Debug, PartialEq)]
enum EntryPosition {
    InsideRemovedEntry,
    OutsideRemovedEntry,
}

pub fn decode_key(key_base64: &str) -> Result<Vec<u8>, AppError> {
    let key: Vec<u8> = secr::BASE64
        .decode(key_base64.as_bytes())
        .map_err(|error| AppError::new(&format!("the secr key in config is not valid base64: {}", error)))?;

    // secr panics on a key of any other length.
    if key.len() != SECR_KEY_LENGTH_BYTES {
        return Err(AppError::new(&format!(
            "the secr key in config decodes to {} bytes, not {}",
            key.len(),
            SECR_KEY_LENGTH_BYTES,
        )));
    }

    Ok(key)
}

pub fn generate_key_base64() -> String {
    secr::BASE64.encode(secr::cryptography::generate_key())
}

pub fn read_store_contents(store_path: &Path) -> Result<StoreContents, AppError> {
    let existing_store: String = private_file::read_existing_text(store_path)?;

    if existing_store.trim().is_empty() {
        return Ok(StoreContents::MissingOrBlank);
    }

    Ok(StoreContents::HasContent)
}

pub fn read_store(store_path: &Path) -> Result<secr::store::SecretStore, AppError> {
    let store_contents: StoreContents = read_store_contents(store_path)?;

    if store_contents == StoreContents::MissingOrBlank {
        return Ok(secr::store::SecretStore::new());
    }

    let store: secr::store::SecretStore = secr::load::load_secrets_from_file(store_path)?;

    Ok(store)
}

pub fn read_stored_secret_names(store_path: &Path) -> Result<Vec<String>, AppError> {
    let store: secr::store::SecretStore = read_store(store_path)?;

    Ok(secr::store::list_secret_names(&store))
}

pub fn read_optional_secret(location: &SecretsLocation, name: &str) -> Result<Option<String>, AppError> {
    let store: secr::store::SecretStore = read_store(&location.store_path())?;

    let present: bool = secr::store::list_secret_names(&store).iter().any(|candidate| candidate == name);
    if !present {
        return Ok(None);
    }

    let key: Vec<u8> = decode_key(&location.key_base64)?;

    let plaintext: Vec<u8> = secr::cryptography::decrypt(&store, &key, name)?;
    let value: String = String::from_utf8(plaintext)?;

    Ok(Some(value))
}

pub fn read_required_secret(location: &SecretsLocation, name: &str) -> Result<String, AppError> {
    let value: Option<String> = read_optional_secret(location, name)?;

    value.ok_or_else(|| {
        AppError::new(&format!(
            "secret '{}' is not in {}; run simple-alert register",
            name,
            location.store_path().display(),
        ))
    })
}

pub fn render_encrypted_entry(key: &Vec<u8>, name: &str, plaintext: &str) -> Result<String, AppError> {
    let secret: secr::secret::SecretBase64 = secr::cryptography::encrypt(key, plaintext.as_bytes())?;

    Ok(format!("{}\n", secret.to_yaml(name)))
}

/// Keeps the other entries and comments byte for byte.
pub fn render_store_with_secret(store_text: &str, key: &Vec<u8>, name: &str, plaintext: &str) -> Result<String, AppError> {
    let kept_text: String = render_store_without_secret(store_text, name);
    let entry: String = render_encrypted_entry(key, name, plaintext)?;

    if kept_text.trim().is_empty() {
        return Ok(entry);
    }

    let line_terminator: &str = if kept_text.ends_with('\n') { "" } else { "\n" };

    Ok(format!("{}{}\n{}", kept_text, line_terminator, entry))
}

pub fn render_store_without_secret(store_text: &str, removed_secret_name: &str) -> String {
    let mut kept_lines: Vec<&str> = Vec::new();
    let mut entry_position: EntryPosition = EntryPosition::OutsideRemovedEntry;

    for line in store_text.split_inclusive('\n') {
        let top_level_entry_name: Option<&str> = get_top_level_entry_name(line);

        if let Some(entry_name) = top_level_entry_name {
            entry_position = match entry_name == removed_secret_name {
                true => EntryPosition::InsideRemovedEntry,
                false => EntryPosition::OutsideRemovedEntry,
            };
        }

        let line_is_part_of_an_entry: bool = top_level_entry_name.is_some() || is_indented_content_line(line);

        if entry_position == EntryPosition::OutsideRemovedEntry || !line_is_part_of_an_entry {
            push_without_repeated_blank_lines(&mut kept_lines, line);
        }
    }

    kept_lines.concat()
}

// Removing an entry leaves the blank lines that separated it from its neighbours.
fn push_without_repeated_blank_lines<'a>(kept_lines: &mut Vec<&'a str>, line: &'a str) {
    let line_is_blank: bool = line.trim().is_empty();
    let previous_line_is_blank: bool = kept_lines.last().map_or(true, |previous_line| previous_line.trim().is_empty());

    if line_is_blank && previous_line_is_blank {
        return;
    }

    kept_lines.push(line);
}

fn get_top_level_entry_name(line: &str) -> Option<&str> {
    let starts_an_entry: bool = !line.starts_with([' ', '\t', '#']) && !line.trim().is_empty();

    if !starts_an_entry {
        return None;
    }

    let (raw_name, _): (&str, &str) = line.split_once(':')?;
    let name: &str = raw_name.trim().trim_matches(['"', '\'']);

    Some(name)
}

fn is_indented_content_line(line: &str) -> bool {
    line.starts_with([' ', '\t']) && !line.trim().is_empty()
}

/// Fails unless the store file loads with secr, holds exactly `expected_secret_names`, and `name` decrypts to `plaintext`.
pub fn verify_store_file(
    store_path: &Path,
    expected_secret_names: &[String],
    key: &Vec<u8>,
    name: &str,
    plaintext: &str,
) -> Result<(), AppError> {
    let store: secr::store::SecretStore = secr::load::load_secrets_from_file(store_path)?;
    let stored_secret_names: Vec<String> = secr::store::list_secret_names(&store);

    let mut sorted_expected_secret_names: Vec<String> = expected_secret_names.to_vec();
    sorted_expected_secret_names.sort();
    sorted_expected_secret_names.dedup();

    if stored_secret_names != sorted_expected_secret_names {
        return Err(AppError::new(&format!(
            "the rewritten secr store would hold [{}] instead of [{}]",
            stored_secret_names.join(", "),
            sorted_expected_secret_names.join(", "),
        )));
    }

    let decrypted: Vec<u8> = secr::cryptography::decrypt(&store, key, name)?;

    if decrypted != plaintext.as_bytes() {
        return Err(AppError::new(&format!("secret '{}' in the rewritten secr store does not decrypt to the new value", name)));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    fn get_temp_store_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("simple-alert-secrets-test-{}-{}.yaml", std::process::id(), label))
    }

    #[test]
    fn render_store_with_secret_replaces_the_entry_and_keeps_the_others_decryptable() {
        let key: Vec<u8> = decode_key(&generate_key_base64()).unwrap();
        let store_path: PathBuf = get_temp_store_path("replace");
        let existing_store: String = [
            render_encrypted_entry(&key, "other_service_password", "one").unwrap(),
            render_encrypted_entry(&key, BOT_TOKEN_SECRET_NAME, "old-token").unwrap(),
        ]
        .join("\n");

        let rendered: String = render_store_with_secret(&existing_store, &key, BOT_TOKEN_SECRET_NAME, "new-token").unwrap();
        std::fs::write(&store_path, rendered).unwrap();
        let store: Result<secr::store::SecretStore, AppError> = read_store(&store_path);
        std::fs::remove_file(&store_path).ok();

        let store: secr::store::SecretStore = store.unwrap();
        assert_eq!(secr::cryptography::decrypt(&store, &key, "other_service_password").unwrap(), b"one");
        assert_eq!(secr::cryptography::decrypt(&store, &key, BOT_TOKEN_SECRET_NAME).unwrap(), b"new-token");
    }

    #[test]
    fn render_store_without_secret_keeps_comments_and_one_blank_line_between_neighbours() {
        let store_text: &str = "\
# shared secrets
first_password:
    nonce: 'AAAA'
    ciphertext: 'BBBB'

telegram_bot_token:
    nonce: 'CCCC'
    ciphertext: 'DDDD'

last_password:
    nonce: 'EEEE'
    ciphertext: 'FFFF'
";

        let rendered: String = render_store_without_secret(store_text, BOT_TOKEN_SECRET_NAME);

        assert_eq!(
            rendered,
            "# shared secrets\nfirst_password:\n    nonce: 'AAAA'\n    ciphertext: 'BBBB'\n\nlast_password:\n    nonce: 'EEEE'\n    ciphertext: 'FFFF'\n",
        );
    }

    #[test]
    fn verify_store_file_rejects_a_store_missing_an_expected_secret() {
        let key: Vec<u8> = decode_key(&generate_key_base64()).unwrap();
        let store_path: PathBuf = get_temp_store_path("verify");
        std::fs::write(&store_path, render_encrypted_entry(&key, BOT_TOKEN_SECRET_NAME, "token").unwrap()).unwrap();
        let expected_secret_names: Vec<String> = vec!["other_service_password".to_string(), BOT_TOKEN_SECRET_NAME.to_string()];

        let verification: Result<(), AppError> =
            verify_store_file(&store_path, &expected_secret_names, &key, BOT_TOKEN_SECRET_NAME, "token");
        std::fs::remove_file(&store_path).ok();

        assert!(verification.is_err());
    }
}
