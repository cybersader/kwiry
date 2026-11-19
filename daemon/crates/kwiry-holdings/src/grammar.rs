//! Identifier grammar from the wire's limits table.

/// `[A-Za-z0-9_-]+`.
pub fn is_b64url(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn b64url_len(text: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&text.len()) && is_b64url(text)
}

/// `prefix` followed by exactly 22 base64url characters (128 random bits).
pub fn random_id(text: &str, prefix: &str) -> bool {
    text.strip_prefix(prefix)
        .is_some_and(|rest| rest.len() == 22 && is_b64url(rest))
}

pub fn holder_id(text: &str) -> bool {
    random_id(text, "hd_")
}

pub fn holding_id(text: &str) -> bool {
    random_id(text, "hl_")
}

pub fn obligation_id(text: &str) -> bool {
    random_id(text, "ob_")
}

pub fn list_id(text: &str) -> bool {
    random_id(text, "ls_")
}

pub fn epoch_id(text: &str) -> bool {
    random_id(text, "ep_")
}

pub fn pin_ref(text: &str) -> bool {
    random_id(text, "pr_")
}

/// Holder-observed storage pin: exactly 43 base64url characters.
pub fn pin_id(text: &str) -> bool {
    text.len() == 43 && is_b64url(text)
}

/// Holder-scoped opaque resource identifier: 1–128 of `[A-Za-z0-9_-]`.
pub fn resource_id(text: &str) -> bool {
    b64url_len(text, 1, 128)
}

pub fn nonce(text: &str) -> bool {
    b64url_len(text, 22, 86)
}

pub fn jti(text: &str) -> bool {
    b64url_len(text, 22, 64)
}

pub fn idempotency_key(text: &str) -> bool {
    b64url_len(text, 22, 96)
}

/// A tagged HMAC-SHA256 value such as `hs1.<43>` or `lt1.<43>`.
pub fn tagged_mac(text: &str, prefix: &str) -> bool {
    text.strip_prefix(prefix)
        .is_some_and(|rest| rest.len() == 43 && is_b64url(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_boundaries() {
        assert!(holder_id("hd_7fQ2mN0xR4bL9sK1pW3eYa"));
        assert!(!holder_id("hd_7fQ2mN0xR4bL9sK1pW3eY"));
        assert!(!holder_id("hd_7fQ2mN0xR4bL9sK1pW3eYa1"));
        assert!(!holder_id("hl_7fQ2mN0xR4bL9sK1pW3eYa"));
        assert!(!holder_id("hd_7fQ2mN0xR4bL9sK1pW3eY="));
        assert!(resource_id("a"));
        assert!(resource_id(&"a".repeat(128)));
        assert!(!resource_id(&"a".repeat(129)));
        assert!(!resource_id(""));
        assert!(!resource_id("a/b"));
        assert!(nonce(&"n".repeat(22)) && nonce(&"n".repeat(86)));
        assert!(!nonce(&"n".repeat(21)) && !nonce(&"n".repeat(87)));
        assert!(pin_id(&"p".repeat(43)) && !pin_id(&"p".repeat(42)));
        assert!(tagged_mac(&format!("hs1.{}", "x".repeat(43)), "hs1."));
        assert!(!tagged_mac(&format!("hs1.{}", "x".repeat(44)), "hs1."));
    }
}
