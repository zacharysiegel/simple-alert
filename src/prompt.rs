use std::io::BufRead;
use std::io::Write;

use simple_alert::error::AppError;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Confirmation {
    Confirmed,
    Declined,
}

pub fn read_line_answer(prompt: &str) -> Result<String, AppError> {
    print!("{}", prompt);
    std::io::stdout().flush()?;

    let mut answer: String = String::new();
    let bytes_read: usize = std::io::stdin().lock().read_line(&mut answer)?;

    if bytes_read == 0 {
        return Err(AppError::new("standard input closed before the prompt was answered"));
    }

    Ok(answer.trim().to_string())
}

pub fn read_hidden_answer(prompt: &str) -> Result<String, AppError> {
    let answer: String = rpassword::prompt_password(prompt)?;

    Ok(answer.trim().to_string())
}

pub fn read_until_valid<T>(
    read_answer: impl Fn() -> Result<String, AppError>,
    parse_answer: impl Fn(&str) -> Result<T, AppError>,
) -> Result<T, AppError> {
    loop {
        let answer: String = read_answer()?;
        let parsed: Result<T, AppError> = parse_answer(&answer);

        match parsed {
            Ok(value) => return Ok(value),
            Err(error) => println!("{}", error.message),
        }
    }
}

pub fn read_confirmation(prompt: &str, default: Confirmation) -> Result<Confirmation, AppError> {
    read_until_valid(|| read_line_answer(prompt), |answer| parse_confirmation(answer, default))
}

pub fn parse_confirmation(answer: &str, default: Confirmation) -> Result<Confirmation, AppError> {
    let normalized_answer: String = answer.trim().to_ascii_lowercase();

    match normalized_answer.as_str() {
        "" => Ok(default),
        "y" | "yes" => Ok(Confirmation::Confirmed),
        "n" | "no" => Ok(Confirmation::Declined),
        _ => Err(AppError::new("answer y or n")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_confirmation_uses_the_default_for_a_blank_answer() {
        assert_eq!(parse_confirmation("", Confirmation::Declined).unwrap(), Confirmation::Declined);
    }

    #[test]
    fn parse_confirmation_rejects_an_unrecognized_answer() {
        assert!(parse_confirmation("sure", Confirmation::Declined).is_err());
    }
}
