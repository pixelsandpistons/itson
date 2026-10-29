//! Session tokens: 256 random bits, shown to the client once as 64 hex
//! characters. The store keeps only the SHA-256 of the token, so a copied
//! database does not give away live sessions.

use itson_protocol::SessionToken;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};

/// Hex SHA-256 of a token; safe to store and log.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenHash(String);

impl TokenHash {
    pub fn of(token: &SessionToken) -> Self {
        TokenHash(crate::password::hex(
            digest(&SHA256, token.expose().as_bytes()).as_ref(),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Makes a new token and its hash. Fails only if the random source does.
pub fn generate() -> Option<(SessionToken, TokenHash)> {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes).ok()?;
    let token = SessionToken::new(crate::password::hex(&bytes));
    let hash = TokenHash::of(&token);
    Some((token, hash))
}

/// Makes an invite code: 10 characters from an unambiguous alphabet.
pub fn invite_code() -> Option<String> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 10];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(
        bytes
            .iter()
            .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
            .collect(),
    )
}
