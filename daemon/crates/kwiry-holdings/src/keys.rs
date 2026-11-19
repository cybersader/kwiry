//! The holder's Ed25519 key and the pinned set of authority keys (v2 Part I
//! §2.1–§2.3, H.6).
//!
//! Authority keys are pinned at enrollment from a bare JWKS. After that a new
//! key is accepted only through a key-set statement signed by a currently
//! pinned key, with a strictly increasing `keyset_seq`.

use crate::profile::WireProfile;
use crate::{b64, clock, jws};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("entropy unavailable")]
    Entropy,
    #[error("malformed holder key")]
    HolderKey,
    #[error("malformed authority key: {0}")]
    AuthorityKey(&'static str),
    #[error("key-set statement refused: {0}")]
    Keyset(&'static str),
    /// v2 H.37.6: applying the statement would leave no pinned key, at once
    /// or at `not_after_old` (`keyset_invalid`).
    #[error("keyset_invalid: the statement would leave no pinned key")]
    KeysetInvalid,
}

/// The holder's signing key. The private half never leaves the holder.
pub struct HolderKey {
    signing: ed25519_dalek::SigningKey,
}

impl HolderKey {
    pub fn generate() -> Result<Self, KeyError> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|_| KeyError::Entropy)?;
        Ok(Self::from_seed(seed))
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            signing: ed25519_dalek::SigningKey::from_bytes(&seed),
        }
    }

    pub fn signing_key(&self) -> &ed25519_dalek::SigningKey {
        &self.signing
    }

    fn x(&self) -> String {
        b64::encode(self.signing.verifying_key().as_bytes())
    }

    pub fn public_jwk(&self) -> Value {
        json!({ "kty": "OKP", "crv": "Ed25519", "x": self.x() })
    }

    /// RFC 7638 thumbprint; the holder's `kid` on every request.
    pub fn thumbprint(&self) -> String {
        okp_thumbprint(&self.x())
    }

    pub fn private_jwk(&self) -> Value {
        json!({
            "kty": "OKP",
            "crv": "Ed25519",
            "x": self.x(),
            "d": b64::encode(self.signing.as_bytes()),
        })
    }

    pub fn from_private_jwk(jwk: &Value) -> Result<Self, KeyError> {
        let object = jwk.as_object().ok_or(KeyError::HolderKey)?;
        if object.len() != 4
            || object.get("kty").and_then(Value::as_str) != Some("OKP")
            || object.get("crv").and_then(Value::as_str) != Some("Ed25519")
        {
            return Err(KeyError::HolderKey);
        }
        let seed: [u8; 32] = object
            .get("d")
            .and_then(Value::as_str)
            .and_then(b64::decode)
            .and_then(|d| d.try_into().ok())
            .ok_or(KeyError::HolderKey)?;
        let key = Self::from_seed(seed);
        if object.get("x").and_then(Value::as_str) != Some(key.x().as_str()) {
            return Err(KeyError::HolderKey);
        }
        Ok(key)
    }
}

/// RFC 7638 thumbprint of an Ed25519 public key with base64url `x`.
pub fn okp_thumbprint(x: &str) -> String {
    let canonical = format!(r#"{{"crv":"Ed25519","kty":"OKP","x":"{x}"}}"#);
    b64::encode(&Sha256::digest(canonical.as_bytes()))
}

/// Parses an Ed25519 public JWK (`kty`, `crv`, `x` only).
pub fn ed25519_public_from_jwk(jwk: &Value) -> Option<ed25519_dalek::VerifyingKey> {
    let object = jwk.as_object()?;
    if object.len() != 3
        || object.get("kty")?.as_str()? != "OKP"
        || object.get("crv")?.as_str()? != "Ed25519"
    {
        return None;
    }
    let bytes: [u8; 32] = b64::decode(object.get("x")?.as_str()?)?.try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&bytes).ok()
}

/// Parses an authority P-256 public JWK. Only `kty`, `crv`, `x`, `y`, `kid`,
/// `use` and `alg` are allowed; a private `d` or any other member is refused.
pub fn parse_authority_jwk(
    jwk: &Value,
    kid_prefix: &str,
) -> Result<(String, p256::ecdsa::VerifyingKey), KeyError> {
    let object = jwk
        .as_object()
        .ok_or(KeyError::AuthorityKey("not an object"))?;
    if object.keys().any(|k| {
        !matches!(
            k.as_str(),
            "kty" | "crv" | "x" | "y" | "kid" | "use" | "alg"
        )
    }) {
        return Err(KeyError::AuthorityKey("unexpected member"));
    }
    let text = |name: &str| object.get(name).and_then(Value::as_str);
    if text("kty") != Some("EC") || text("crv") != Some("P-256") {
        return Err(KeyError::AuthorityKey("not an EC P-256 key"));
    }
    if object.contains_key("use") && text("use") != Some("sig") {
        return Err(KeyError::AuthorityKey("use"));
    }
    if object.contains_key("alg") && text("alg") != Some("ES256") {
        return Err(KeyError::AuthorityKey("alg"));
    }
    let kid = text("kid")
        .filter(|kid| kid.len() > kid_prefix.len() && kid.starts_with(kid_prefix))
        .ok_or(KeyError::AuthorityKey("kid"))?;
    let coordinate = |name: &str| -> Result<[u8; 32], KeyError> {
        text(name)
            .and_then(b64::decode)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(KeyError::AuthorityKey("coordinate"))
    };
    let mut point = Vec::with_capacity(65);
    point.push(4);
    point.extend_from_slice(&coordinate("x")?);
    point.extend_from_slice(&coordinate("y")?);
    let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(&point)
        .map_err(|_| KeyError::AuthorityKey("point"))?;
    Ok((kid.to_owned(), key))
}

/// Public JWK of a P-256 key, for enrollment fixtures and test authorities.
pub fn authority_public_jwk(kid: &str, key: &p256::ecdsa::VerifyingKey) -> Value {
    let point = key.to_encoded_point(false);
    json!({
        "kty": "EC",
        "crv": "P-256",
        "x": b64::encode(point.x().map(|x| x.as_slice()).unwrap_or_default()),
        "y": b64::encode(point.y().map(|y| y.as_slice()).unwrap_or_default()),
        "kid": kid,
        "use": "sig",
        "alg": "ES256",
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedKey {
    pub jwk: Value,
    /// Set when a key-set statement retired this key with `not_after_old`.
    pub not_after_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedKeys {
    /// `None` until the first key-set statement is accepted.
    pub keyset_seq: Option<u64>,
    pub keys: BTreeMap<String, PinnedKey>,
}

impl PinnedKeys {
    /// Pins the keys of a bare JWKS at enrollment. Enrollment is the only
    /// time a bare JWKS is trusted.
    pub fn from_enrollment_jwks(jwks: &Value, kid_prefix: &str) -> Result<Self, KeyError> {
        let keys = jwks
            .as_object()
            .filter(|o| o.len() == 1)
            .and_then(|o| o.get("keys"))
            .and_then(Value::as_array)
            .filter(|keys| !keys.is_empty())
            .ok_or(KeyError::AuthorityKey("jwks"))?;
        let mut pinned = Self::default();
        for jwk in keys {
            let (kid, _) = parse_authority_jwk(jwk, kid_prefix)?;
            let record = PinnedKey {
                jwk: jwk.clone(),
                not_after_ms: None,
            };
            if pinned.keys.insert(kid, record).is_some() {
                return Err(KeyError::AuthorityKey("duplicate kid"));
            }
        }
        Ok(pinned)
    }

    /// The verifying key for `kid` if it is pinned and not past its
    /// retirement instant.
    pub fn live(
        &self,
        kid: &str,
        kid_prefix: &str,
        now_ms: i64,
    ) -> Option<p256::ecdsa::VerifyingKey> {
        let record = self.keys.get(kid)?;
        // v2 H.37.4: retired at `not_after_old`, on the holder's wall clock.
        if record.not_after_ms.is_some_and(|limit| now_ms >= limit) {
            return None;
        }
        parse_authority_jwk(&record.jwk, kid_prefix)
            .ok()
            .filter(|(parsed, _)| parsed == kid)
            .map(|(_, key)| key)
    }
}

fn claim<'a>(claims: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    claims.get(name)
}

/// Verifies a key-set statement under the current pins and returns the pin
/// set it produces. Nothing is changed on refusal; the caller persists the
/// result through the single holder-state writer before trusting any new key.
pub fn verify_keyset(
    compact: &str,
    pins: &PinnedKeys,
    profile: &WireProfile,
    issuer: &str,
    now_ms: i64,
) -> Result<PinnedKeys, KeyError> {
    let refuse = KeyError::Keyset;
    let statement = jws::parse(compact).map_err(|_| refuse("compact"))?;
    if statement.typ != profile.typ_keyset {
        return Err(refuse("typ"));
    }
    if statement.alg != "ES256" {
        return Err(refuse("alg"));
    }
    let signer = pins
        .live(&statement.kid, &profile.authority_key_id_prefix, now_ms)
        .ok_or(refuse("signer not pinned"))?;
    if !statement.verify_es256(&signer) {
        return Err(refuse("signature"));
    }
    let claims = &statement.claims;
    if claims.keys().any(|k| {
        !matches!(
            k.as_str(),
            "iss" | "aud" | "iat" | "keyset_seq" | "keys" | "retire_kids" | "not_after_old"
        )
    }) {
        return Err(refuse("unexpected claim"));
    }
    if claim(claims, "iss").and_then(Value::as_str) != Some(issuer) {
        return Err(refuse("iss"));
    }
    if claim(claims, "aud").and_then(Value::as_str) != Some(profile.broadcast_audience().as_str()) {
        return Err(refuse("aud"));
    }
    claim(claims, "iat")
        .and_then(Value::as_i64)
        .ok_or(refuse("iat"))?;
    let sequence = claim(claims, "keyset_seq")
        .and_then(Value::as_u64)
        .ok_or(refuse("keyset_seq"))?;
    if pins.keyset_seq.is_some_and(|current| sequence <= current) {
        return Err(refuse("keyset_seq does not increase"));
    }
    let not_after_old = match claim(claims, "not_after_old") {
        None => None,
        Some(Value::Number(n)) => Some(
            n.as_i64()
                .and_then(|s| s.checked_mul(1000))
                .ok_or(refuse("not_after_old"))?,
        ),
        Some(Value::String(text)) => {
            Some(clock::parse_utc_ms(text).ok_or(refuse("not_after_old"))?)
        }
        Some(_) => return Err(refuse("not_after_old")),
    };
    let new_keys = claim(claims, "keys")
        .and_then(Value::as_array)
        .ok_or(refuse("keys"))?;
    let retire = claim(claims, "retire_kids")
        .and_then(Value::as_array)
        .ok_or(refuse("retire_kids"))?;

    // v2 H.37: `keys` adds; `not_after_old` retires every earlier kid that
    // `keys` does not list, at that instant; `retire_kids` is immediate.
    let mut next = pins.clone();
    next.keyset_seq = Some(sequence);
    let mut listed = std::collections::BTreeSet::new();
    for jwk in new_keys {
        let (kid, _) = parse_authority_jwk(jwk, &profile.authority_key_id_prefix)
            .map_err(|_| refuse("key"))?;
        match next.keys.get(&kid) {
            Some(existing) if existing.jwk != *jwk => return Err(refuse("kid reused")),
            Some(_) => {}
            None => {
                next.keys.insert(
                    kid.clone(),
                    PinnedKey {
                        jwk: jwk.clone(),
                        not_after_ms: None,
                    },
                );
            }
        }
        listed.insert(kid);
    }
    if let Some(limit) = not_after_old {
        for (kid, record) in &mut next.keys {
            if pins.keys.contains_key(kid) && !listed.contains(kid) {
                record.not_after_ms = Some(record.not_after_ms.map_or(limit, |l| l.min(limit)));
            }
        }
    }
    for kid in retire {
        let kid = kid.as_str().ok_or(refuse("retire_kids"))?;
        next.keys.remove(kid);
    }
    // v2 H.37.6: never an empty set, now or at `not_after_old`.
    let live_now = next
        .keys
        .values()
        .any(|r| r.not_after_ms.is_none_or(|limit| now_ms < limit));
    let live_after = not_after_old.is_none_or(|horizon| {
        next.keys
            .values()
            .any(|r| r.not_after_ms.is_none_or(|limit| limit > horizon))
    });
    if !live_now || !live_after {
        return Err(KeyError::KeysetInvalid);
    }
    Ok(next)
}
