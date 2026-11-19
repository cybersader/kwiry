//! Unpadded base64url, canonical only.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes canonical unpadded base64url. Padding, other alphabets and
/// non-zero trailing bits are refused.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    if !text.is_empty() && !crate::grammar::is_b64url(text) {
        return None;
    }
    URL_SAFE_NO_PAD.decode(text).ok()
}

/// A fresh random token of `bytes` bytes, base64url encoded.
pub fn random_token(bytes: usize) -> Result<String, getrandom::Error> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer)?;
    Ok(encode(&buffer))
}
