//! Compact JWS with a closed header (`alg`, `typ`, `kid` and nothing else).
//!
//! An embedded `jwk`, `jku`, `x5u`, `x5c`, `crit` or any other header member
//! is refused, so no artefact can bring its own key (no TOFU on the wire).

use crate::b64;
use crate::strict_json;
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JwsError {
    #[error("not a three-part compact serialization")]
    Malformed,
    #[error("header is not a strict JSON object")]
    Header,
    #[error("header member outside alg, typ, kid")]
    HeaderMember,
    #[error("payload is not a strict JSON object")]
    Payload,
    /// The signature lacks its algorithm's structure (v2 H.35): an ES256
    /// signature must decode to a valid 64-byte `r || s`; an EdDSA signature
    /// must be 64 bytes.
    #[error("signature encoding does not match alg")]
    SignatureEncoding,
}

pub struct Compact {
    pub alg: String,
    pub typ: String,
    pub kid: String,
    pub claims: Map<String, Value>,
    signing_input: String,
    signature: Vec<u8>,
}

fn header_field(header: &Map<String, Value>, name: &str) -> Result<String, JwsError> {
    header
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(JwsError::Header)
}

pub fn parse(compact: &str) -> Result<Compact, JwsError> {
    let mut parts = compact.split('.');
    let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwsError::Malformed);
    };
    if header_b64.is_empty() || payload_b64.is_empty() || signature_b64.is_empty() {
        return Err(JwsError::Malformed);
    }
    let header_bytes = b64::decode(header_b64).ok_or(JwsError::Malformed)?;
    let payload_bytes = b64::decode(payload_b64).ok_or(JwsError::Malformed)?;
    let signature = b64::decode(signature_b64).ok_or(JwsError::Malformed)?;
    let Ok(Value::Object(header)) = strict_json::parse(&header_bytes) else {
        return Err(JwsError::Header);
    };
    if header.len() != 3
        || header
            .keys()
            .any(|k| !matches!(k.as_str(), "alg" | "typ" | "kid"))
    {
        return Err(JwsError::HeaderMember);
    }
    let Ok(Value::Object(claims)) = strict_json::parse(&payload_bytes) else {
        return Err(JwsError::Payload);
    };
    let alg = header_field(&header, "alg")?;
    let well_formed = match alg.as_str() {
        "ES256" => p256::ecdsa::Signature::from_slice(&signature).is_ok(),
        "EdDSA" => signature.len() == 64,
        // Every verifier in this crate refuses any other `alg`.
        _ => true,
    };
    if !well_formed {
        return Err(JwsError::SignatureEncoding);
    }
    Ok(Compact {
        alg,
        typ: header_field(&header, "typ")?,
        kid: header_field(&header, "kid")?,
        claims,
        signing_input: format!("{header_b64}.{payload_b64}"),
        signature,
    })
}

impl Compact {
    pub fn verify_es256(&self, key: &p256::ecdsa::VerifyingKey) -> bool {
        use p256::ecdsa::signature::Verifier;
        let Ok(signature) = p256::ecdsa::Signature::from_slice(&self.signature) else {
            return false;
        };
        key.verify(self.signing_input.as_bytes(), &signature)
            .is_ok()
    }

    pub fn verify_eddsa(&self, key: &ed25519_dalek::VerifyingKey) -> bool {
        let Ok(bytes) = <[u8; 64]>::try_from(self.signature.as_slice()) else {
            return false;
        };
        key.verify_strict(
            self.signing_input.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&bytes),
        )
        .is_ok()
    }
}

fn signing_input(alg: &str, typ: &str, kid: &str, claims: &Value) -> String {
    let header = serde_json::json!({ "alg": alg, "typ": typ, "kid": kid });
    format!(
        "{}.{}",
        b64::encode(header.to_string().as_bytes()),
        b64::encode(claims.to_string().as_bytes())
    )
}

/// Signs `claims` with the holder's Ed25519 key.
pub fn sign_eddsa(typ: &str, kid: &str, claims: &Value, key: &ed25519_dalek::SigningKey) -> String {
    use ed25519_dalek::Signer;
    let input = signing_input("EdDSA", typ, kid, claims);
    let signature = key.sign(input.as_bytes());
    format!("{input}.{}", b64::encode(&signature.to_bytes()))
}

/// Signs `claims` with a P-256 key (authority side; used by test fakes).
pub fn sign_es256(typ: &str, kid: &str, claims: &Value, key: &p256::ecdsa::SigningKey) -> String {
    sign_with_header("ES256", typ, kid, claims, key)
}

/// Signs with an arbitrary `alg` header value; exists so tests can build
/// artefacts that lie about their algorithm.
pub fn sign_with_header(
    alg: &str,
    typ: &str,
    kid: &str,
    claims: &Value,
    key: &p256::ecdsa::SigningKey,
) -> String {
    use p256::ecdsa::signature::Signer;
    let input = signing_input(alg, typ, kid, claims);
    let signature: p256::ecdsa::Signature = key.sign(input.as_bytes());
    format!("{input}.{}", b64::encode(&signature.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p256_key() -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap()
    }

    #[test]
    fn closed_header_and_signature_round_trip() {
        let key = p256_key();
        let compact = sign_es256("x+jwt", "k1", &serde_json::json!({"a": 1}), &key);
        let parsed = parse(&compact).unwrap();
        assert_eq!(
            (parsed.alg.as_str(), parsed.typ.as_str()),
            ("ES256", "x+jwt")
        );
        assert!(parsed.verify_es256(key.verifying_key()));
        let other = p256::ecdsa::SigningKey::from_slice(&[0x12; 32]).unwrap();
        assert!(!parsed.verify_es256(other.verifying_key()));

        let header = b64::encode(br#"{"alg":"ES256","typ":"x+jwt","kid":"k1","jwk":{}}"#);
        let body = b64::encode(br#"{"a":1}"#);
        let forged = format!("{header}.{body}.{}", b64::encode(&[0; 64]));
        assert_eq!(parse(&forged).err(), Some(JwsError::HeaderMember));
        let duplicate = b64::encode(br#"{"alg":"ES256","alg":"none","kid":"k1"}"#);
        assert_eq!(
            parse(&format!("{duplicate}.{body}.{}", b64::encode(&[0; 64]))).err(),
            Some(JwsError::Header)
        );
        assert_eq!(parse("a.b").err(), Some(JwsError::Malformed));
        assert_eq!(
            parse(&format!("{compact}.x")).err(),
            Some(JwsError::Malformed)
        );
        assert_eq!(
            parse(&format!("{header}=.{body}.AA")).err(),
            Some(JwsError::Malformed)
        );
    }
}
