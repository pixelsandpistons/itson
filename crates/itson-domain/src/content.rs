//! Validation for member-supplied text.

use itson_protocol::limits::{MAX_MESSAGE_CHARS, MAX_USERNAME_CHARS, MIN_USERNAME_CHARS};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentError {
    Empty,
    TooLong {
        chars: usize,
        max: usize,
    },
    /// A control character other than newline or tab.
    ControlCharacter,
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentError::Empty => write!(f, "Write something before sending."),
            ContentError::TooLong { chars, max } => {
                write!(
                    f,
                    "Messages can be up to {max} characters. This one has {chars}."
                )
            }
            ContentError::ControlCharacter => {
                write!(f, "The message contains a hidden control character.")
            }
        }
    }
}

impl std::error::Error for ContentError {}

/// Normalizes line endings to `\n`, trims surrounding whitespace and checks
/// length and characters. Returns the text to store.
pub fn normalize_message(input: &str) -> Result<String, ContentError> {
    let unified = input.replace("\r\n", "\n").replace('\r', "\n");
    let text = unified.trim();
    if text.is_empty() {
        return Err(ContentError::Empty);
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ContentError::ControlCharacter);
    }
    let chars = text.chars().count();
    if chars > MAX_MESSAGE_CHARS {
        return Err(ContentError::TooLong {
            chars,
            max: MAX_MESSAGE_CHARS,
        });
    }
    Ok(text.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsernameError {
    TooShort,
    TooLong,
    /// Only `a`-`z`, `0`-`9`, `_` and `.` are allowed.
    InvalidCharacter,
    /// A dot at either end, or two dots together.
    BadDots,
}

impl fmt::Display for UsernameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsernameError::TooShort => {
                write!(
                    f,
                    "Usernames need at least {MIN_USERNAME_CHARS} characters."
                )
            }
            UsernameError::TooLong => {
                write!(f, "Usernames can be up to {MAX_USERNAME_CHARS} characters.")
            }
            UsernameError::InvalidCharacter => {
                write!(f, "Use only letters a to z, numbers, underscores and dots.")
            }
            UsernameError::BadDots => write!(
                f,
                "A username cannot start or end with a dot or have two dots together."
            ),
        }
    }
}

impl std::error::Error for UsernameError {}

/// Lowercases and checks a login username. Usernames are ASCII only so that
/// look-alike characters cannot impersonate another member; display names
/// carry any other script.
pub fn normalize_username(input: &str) -> Result<String, UsernameError> {
    let name = input.trim().to_ascii_lowercase();
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
    {
        return Err(UsernameError::InvalidCharacter);
    }
    if name.len() < MIN_USERNAME_CHARS {
        return Err(UsernameError::TooShort);
    }
    if name.len() > MAX_USERNAME_CHARS {
        return Err(UsernameError::TooLong);
    }
    if name.starts_with('.') || name.ends_with('.') || name.contains("..") {
        return Err(UsernameError::BadDots);
    }
    Ok(name)
}
