use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::jwk::{
    AlgorithmParameters, EllipticCurve, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse,
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use kwiry_core::{
    HostProfile, OpenClastAuthConfig, Principal, ROOM_INCLUSIVE_IDS, ResourceKey, Scope,
    valid_room_identifier, valid_tenant_identifier,
};

use crate::replay::ReplayState;
use serde::de::{Error as _, IgnoredAny};
use serde::{Deserialize, Serialize};

pub(crate) const ROOM_FORMAT: kwiry_core::OpenClastRoomFormat =
    kwiry_core::openclast_room_format!();

const MAX_RESOURCES: usize = 256;
const MAX_SEARCH_LIMIT: usize = 100;
const SEARCH_ACTION: &str = "search:lexical";
const CAPABILITY_TYP: &str = "kwiry-cap+jwt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityFailure {
    Unauthorized,
    Forbidden,
    ReplayStateUnavailable,
}

#[derive(Clone)]
struct VerificationKey {
    algorithm: Algorithm,
    decoding_key: DecodingKey,
}

#[derive(Clone)]
pub(crate) struct CapabilityVerifier {
    issuer: String,
    audience: String,
    tenant_id: String,
    max_token_ttl_seconds: u64,
    workload_subject_allowlist: Vec<String>,
    legacy_identity_accept_until: Option<u64>,
    keys: HashMap<String, VerificationKey>,
    replay: Option<Arc<ReplayState>>,
    room_format: kwiry_core::OpenClastRoomFormat,
}

#[derive(Deserialize)]
struct CapabilityHeader {
    alg: Algorithm,
    kid: Option<String>,
    // A present typ must be a string; only omission permits legacy handling.
    #[serde(default, deserialize_with = "present_string")]
    typ: Option<String>,
    #[serde(default, deserialize_with = "present_ignored")]
    crit: Option<IgnoredAny>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CapabilityClaims {
    iss: String,
    aud: String,
    sub: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_string"
    )]
    actor: Option<String>,
    jti: String,
    iat: u64,
    nbf: u64,
    exp: u64,
    tenant: String,
    actions: Vec<String>,
    resources: Vec<CapabilityResource>,
    constraints: CapabilityConstraints,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_optional_string"
    )]
    policy_revision: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_optional_string"
    )]
    subject_revision: Option<Option<String>>,
    #[serde(flatten, skip_serializing)]
    unknown: HashMap<String, IgnoredAny>,
}

// Only actor-bearing claims may omit typ. JWT decoding checks this shape after
// signature verification and before validating registered claims.
struct LegacyCapabilityClaims(CapabilityClaims);

impl<'de> Deserialize<'de> for LegacyCapabilityClaims {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let claims = CapabilityClaims::deserialize(deserializer)?;
        if claims.actor.is_none() {
            return Err(D::Error::missing_field("actor"));
        }
        Ok(Self(claims))
    }
}

// Workload claims are closed at every signed object level; legacy claims keep
// ignoring unrecognized fields until the operator activates the cutover.
#[derive(Clone, Serialize, Deserialize)]
struct CapabilityResource {
    tenant_id: String,
    vault_id: String,
    room_id: String,
    #[serde(flatten, skip_serializing)]
    unknown: HashMap<String, IgnoredAny>,
}

impl From<CapabilityResource> for ResourceKey {
    fn from(resource: CapabilityResource) -> Self {
        Self {
            tenant_id: resource.tenant_id,
            vault_id: resource.vault_id,
            room_id: resource.room_id,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct CapabilityConstraints {
    max_limit: Box<serde_json::value::RawValue>,
    #[serde(flatten, skip_serializing)]
    unknown: HashMap<String, IgnoredAny>,
}

fn present_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn present_ignored<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<IgnoredAny>, D::Error> {
    IgnoredAny::deserialize(deserializer).map(Some)
}

// Legacy issuers could send null; workload issuers must omit the key entirely.
fn present_optional_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

impl CapabilityVerifier {
    pub(crate) fn load(config: &OpenClastAuthConfig, data_root: &Path) -> Result<Self, String> {
        let source = fs::read_to_string(&config.jwks_file).map_err(|error| {
            format!(
                "could not read OpenClast JWKS at {}: {error}",
                config.jwks_file.display()
            )
        })?;
        let mut verifier = Self::from_jwks(config, &source)?;
        if ROOM_INCLUSIVE_IDS {
            verifier.replay = Some(Arc::new(
                ReplayState::start(data_root, unix_time()?)
                    .map_err(|error| format!("could not establish replay state: {error}"))?,
            ));
        }
        Ok(verifier)
    }

    fn from_jwks(config: &OpenClastAuthConfig, source: &str) -> Result<Self, String> {
        let jwks: JwkSet = serde_json::from_str(source)
            .map_err(|error| format!("OpenClast JWKS is invalid JSON: {error}"))?;
        if jwks.keys.is_empty() {
            return Err("OpenClast JWKS must contain at least one key".to_owned());
        }

        let mut keys = HashMap::new();
        for jwk in &jwks.keys {
            if jwk.common.public_key_use != Some(PublicKeyUse::Signature) {
                return Err("every OpenClast JWK must declare use=\"sig\"".to_owned());
            }
            if jwk.common.key_operations.as_deref() != Some(&[KeyOperations::Verify]) {
                return Err("every OpenClast JWK must declare key_ops=[\"verify\"]".to_owned());
            }
            let kid = jwk
                .common
                .key_id
                .as_deref()
                .filter(|kid| !kid.trim().is_empty())
                .ok_or_else(|| "every OpenClast JWK must declare a nonempty kid".to_owned())?;
            let algorithm = allowed_algorithm(jwk.common.key_algorithm, &jwk.algorithm)?;
            let decoding_key = DecodingKey::from_jwk(jwk)
                .map_err(|error| format!("OpenClast JWK {kid:?} is invalid: {error}"))?;
            if keys
                .insert(
                    kid.to_owned(),
                    VerificationKey {
                        algorithm,
                        decoding_key,
                    },
                )
                .is_some()
            {
                return Err(format!("duplicate OpenClast JWK kid: {kid}"));
            }
        }

        Ok(Self {
            issuer: config.issuer.clone(),
            audience: config.audience.clone(),
            tenant_id: config.tenant_id.clone(),
            max_token_ttl_seconds: config.max_token_ttl_seconds,
            workload_subject_allowlist: config.workload_subject_allowlist.clone(),
            legacy_identity_accept_until: config.legacy_identity_accept_until,
            keys,
            replay: None,
            room_format: ROOM_FORMAT,
        })
    }

    pub(crate) fn verify_with_format(
        &self,
        token: &str,
        active_room_format: bool,
    ) -> Result<Principal, CapabilityFailure> {
        let (principal, workload) = self.verify_claims(token, active_room_format)?;
        if let Some((iat, now)) = workload {
            self.replay
                .as_ref()
                .ok_or(CapabilityFailure::ReplayStateUnavailable)?
                .record(
                    principal
                        .jti
                        .as_deref()
                        .expect("verified capability has a jti"),
                    iat,
                    principal.exp.expect("verified capability has an expiry"),
                    now,
                )?;
        }
        Ok(principal)
    }

    #[cfg(test)]
    fn verify(&self, token: &str) -> Result<Principal, CapabilityFailure> {
        self.verify_claims(token, false)
            .map(|(principal, _)| principal)
    }

    fn verify_claims(
        &self,
        token: &str,
        active_room_format: bool,
    ) -> Result<(Principal, Option<(u64, u64)>), CapabilityFailure> {
        let (encoded_header, rest) = token
            .split_once('.')
            .ok_or(CapabilityFailure::Unauthorized)?;
        let decode_segment = |segment| {
            URL_SAFE_NO_PAD
                .decode(segment)
                .map_err(|_| CapabilityFailure::Unauthorized)
        };
        let raw_header = decode_segment(encoded_header)?;
        if ROOM_INCLUSIVE_IDS {
            reject_duplicate_members(&raw_header)?;
        }
        let header: CapabilityHeader =
            serde_json::from_slice(&raw_header).map_err(|_| CapabilityFailure::Unauthorized)?;
        if ROOM_INCLUSIVE_IDS && header.crit.is_some() {
            return Err(CapabilityFailure::Unauthorized);
        }
        // RFC 8725 §3.11: do not accept another kind of signed document here.
        let legacy_only = match header.typ.as_deref() {
            Some(CAPABILITY_TYP) => false,
            None => true,
            Some(_) => return Err(CapabilityFailure::Unauthorized),
        };
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.trim().is_empty())
            .ok_or(CapabilityFailure::Unauthorized)?;
        let key = self.keys.get(kid).ok_or(CapabilityFailure::Unauthorized)?;
        if header.alg != key.algorithm {
            return Err(CapabilityFailure::Unauthorized);
        }

        let mut validation = Validation::new(key.algorithm);
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer]);
        validation.set_required_spec_claims(&["exp", "nbf", "aud", "iss", "sub"]);
        if ROOM_INCLUSIVE_IDS {
            let (encoded_payload, _) = rest
                .split_once('.')
                .ok_or(CapabilityFailure::Unauthorized)?;
            reject_duplicate_members(&decode_segment(encoded_payload)?)?;
        }
        let claims = if legacy_only {
            decode::<LegacyCapabilityClaims>(token, &key.decoding_key, &validation)
                .map(|data| data.claims.0)
        } else {
            decode::<CapabilityClaims>(token, &key.decoding_key, &validation)
                .map(|data| data.claims)
        }
        .map_err(|_| CapabilityFailure::Unauthorized)?;
        self.validate_claims(claims, active_room_format)
    }

    fn validate_claims(
        &self,
        claims: CapabilityClaims,
        active_room_format: bool,
    ) -> Result<(Principal, Option<(u64, u64)>), CapabilityFailure> {
        if claims.iss != self.issuer
            || claims.aud != self.audience
            || claims.sub.trim().is_empty()
            || claims.jti.trim().is_empty()
        {
            return Err(CapabilityFailure::Unauthorized);
        }
        let now = unix_time().map_err(|_| CapabilityFailure::Unauthorized)?;
        match claims.actor.as_deref() {
            None if (!ROOM_INCLUSIVE_IDS || claims.policy_revision.is_none())
                && claims.subject_revision.is_none()
                && claims.unknown.is_empty()
                && claims.constraints.unknown.is_empty()
                && claims
                    .resources
                    .iter()
                    .all(|resource| resource.unknown.is_empty())
                && self
                    .workload_subject_allowlist
                    .iter()
                    .any(|subject| subject.as_bytes() == claims.sub.as_bytes()) => {}
            Some(actor)
                if !actor.trim().is_empty()
                    && actor != claims.sub
                    && self
                        .legacy_identity_accept_until
                        .is_none_or(|cutoff| now < cutoff) => {}
            _ => return Err(CapabilityFailure::Unauthorized),
        }
        if claims.exp < claims.iat
            || claims.nbf > claims.exp
            || claims.iat > now
            || claims.nbf > now
            || claims.exp <= now
            || claims.exp.saturating_sub(claims.iat) > self.max_token_ttl_seconds
        {
            return Err(CapabilityFailure::Unauthorized);
        }
        let raw_limit = claims.constraints.max_limit.get();
        let integer = raw_limit.strip_prefix('-').unwrap_or(raw_limit);
        if !integer.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(CapabilityFailure::Unauthorized);
        }
        let max_limit = match raw_limit.parse::<usize>() {
            Ok(limit) => limit,
            Err(_) if ROOM_INCLUSIVE_IDS => 0, // integer out of range
            Err(_) => return Err(CapabilityFailure::Unauthorized),
        };
        if claims.tenant != self.tenant_id
            || (ROOM_INCLUSIVE_IDS && !valid_tenant_identifier(&claims.tenant))
            || claims.actions != [SEARCH_ACTION]
            || claims.resources.is_empty()
            || claims.resources.len() > MAX_RESOURCES
            || !(1..=MAX_SEARCH_LIMIT).contains(&max_limit)
        {
            return Err(CapabilityFailure::Forbidden);
        }

        let workload = claims.actor.is_none();
        let repeated_vault_allowed =
            self.room_format.admits_repeated_vault() && active_room_format && workload;
        let resources: Vec<ResourceKey> = claims.resources.into_iter().map(Into::into).collect();
        let mut vault_ids = BTreeSet::new();
        let mut room_ids = BTreeSet::new();
        for resource in &resources {
            let valid_ids = if ROOM_INCLUSIVE_IDS {
                valid_room_identifier(&resource.vault_id)
                    && valid_room_identifier(&resource.room_id)
            } else {
                !resource.vault_id.trim().is_empty() && !resource.room_id.trim().is_empty()
            };
            if resource.tenant_id != self.tenant_id
                || resource.tenant_id.trim().is_empty()
                || !valid_ids
                || (!repeated_vault_allowed && !vault_ids.insert(&resource.vault_id))
                || (ROOM_INCLUSIVE_IDS && !room_ids.insert(&resource.room_id))
            {
                return Err(CapabilityFailure::Forbidden);
            }
        }
        let resources = if ROOM_INCLUSIVE_IDS {
            if resources.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(CapabilityFailure::Forbidden);
            }
            resources
        } else {
            resources
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };

        Ok((
            Principal {
                profile: HostProfile::OpenClast,
                exp: Some(claims.exp),
                action: Some(SEARCH_ACTION.to_owned()),
                scopes: vec![Scope::Search],
                resources,
                jti: Some(claims.jti),
                policy_revision: claims.policy_revision.flatten(),
                max_limit,
            },
            (ROOM_INCLUSIVE_IDS && workload).then_some((claims.iat, now)),
        ))
    }
}

/// Walk the original JSON before any flatten or Value map can drop keys.
fn reject_duplicate_members(raw: &[u8]) -> Result<(), CapabilityFailure> {
    use serde_json::value::RawValue;

    struct Unique(usize);
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("JSON containers without duplicate member names")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut keys = BTreeSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !keys.insert(key) {
                    return Err(A::Error::custom("duplicate JSON member"));
                }
                let raw = map.next_value::<&RawValue>()?;
                inspect_child(raw.get(), self.0 + 1).map_err(A::Error::custom)?;
            }
            Ok(())
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while let Some(raw) = seq.next_element::<&RawValue>()? {
                inspect_child(raw.get(), self.0 + 1).map_err(A::Error::custom)?;
            }
            Ok(())
        }
    }

    fn inspect_child(raw: &str, depth: usize) -> Result<(), serde_json::Error> {
        // A RawValue has already passed syntax validation. Walk containers
        // without converting JSON numbers to floats (notably max_limit).
        if !matches!(raw.as_bytes().first(), Some(b'{' | b'[')) {
            return Ok(());
        }
        if depth >= 128 {
            return Err(serde_json::Error::custom(
                "signed JSON is nested too deeply",
            ));
        }
        let mut input = serde_json::Deserializer::from_str(raw);
        serde::Deserializer::deserialize_any(&mut input, Unique(depth))?;
        input.end()
    }

    let raw: &RawValue =
        serde_json::from_slice(raw).map_err(|_| CapabilityFailure::Unauthorized)?;
    inspect_child(raw.get(), 0).map_err(|_| CapabilityFailure::Unauthorized)
}

fn allowed_algorithm(
    algorithm: Option<KeyAlgorithm>,
    parameters: &AlgorithmParameters,
) -> Result<Algorithm, String> {
    match (algorithm, parameters) {
        (Some(KeyAlgorithm::EdDSA), AlgorithmParameters::OctetKeyPair(parameters))
            if parameters.curve == EllipticCurve::Ed25519 =>
        {
            Ok(Algorithm::EdDSA)
        }
        (Some(KeyAlgorithm::ES256), AlgorithmParameters::EllipticCurve(parameters))
            if parameters.curve == EllipticCurve::P256 =>
        {
            Ok(Algorithm::ES256)
        }
        (Some(KeyAlgorithm::RS256), AlgorithmParameters::RSA(_)) => Ok(Algorithm::RS256),
        (Some(KeyAlgorithm::HS256 | KeyAlgorithm::HS384 | KeyAlgorithm::HS512), _)
        | (_, AlgorithmParameters::OctetKey(_)) => {
            Err("symmetric OpenClast JWKs are prohibited".to_owned())
        }
        (Some(_), _) => Err("OpenClast JWK algorithm/key type is unsupported".to_owned()),
        (None, _) => Err("every OpenClast JWK must declare alg".to_owned()),
    }
}

pub(crate) fn unix_time() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jsonwebtoken::{EncodingKey, Header, encode};

    use super::*;

    const ED25519_PRIVATE_DER: &[u8] = &[
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20, 0x6a, 0xc3, 0xfd, 0xee, 0xee, 0x29, 0x8a, 0x92, 0x63, 0x8b, 0x70, 0x0c, 0x4b, 0x11,
        0x7c, 0xc3, 0x2e, 0x2d, 0x2a, 0xce, 0x0d, 0xfd, 0x78, 0x76, 0x94, 0xe2, 0x4c, 0xae, 0x8a,
        0xd5, 0x82, 0x34,
    ];
    const ED25519_JWKS: &str = r#"{"keys":[{
      "kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8",
      "use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"
    }]}"#;

    fn config() -> OpenClastAuthConfig {
        OpenClastAuthConfig {
            tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            issuer: "issuer".into(),
            audience: "kwiry-search".into(),
            jwks_file: Path::new("/unused").to_path_buf(),
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: Vec::new(),
            legacy_identity_accept_until: None,
        }
    }

    fn limit(value: i64) -> Box<serde_json::value::RawValue> {
        serde_json::value::to_raw_value(&value).unwrap()
    }

    fn claims() -> CapabilityClaims {
        let now = unix_time().unwrap();
        CapabilityClaims {
            iss: "issuer".into(),
            aud: "kwiry-search".into(),
            sub: "user-a".into(),
            actor: Some("openclast-orchestrator".into()),
            jti: "decision-a".into(),
            iat: now,
            nbf: now.saturating_sub(1),
            exp: now + 60,
            tenant: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
            actions: vec![SEARCH_ACTION.into()],
            resources: vec![CapabilityResource {
                tenant_id: "tenant_AAAAAAAAAAAAAAAAAAAA".into(),
                vault_id: "VaultOne_0123456789abc".into(),
                room_id: "RoomAaaa_0123456789abc".into(),
                unknown: HashMap::new(),
            }],
            constraints: CapabilityConstraints {
                max_limit: limit(20),
                unknown: HashMap::new(),
            },
            policy_revision: Some(Some("policy-a".into())),
            subject_revision: Some(Some("subject-a".into())),
            unknown: HashMap::new(),
        }
    }

    fn sign(claims: &CapabilityClaims, kid: Option<&str>) -> String {
        sign_with_typ(claims, kid, Some(CAPABILITY_TYP))
    }

    fn sign_with_typ(claims: &impl Serialize, kid: Option<&str>, typ: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = kid.map(str::to_owned);
        header.typ = typ.map(str::to_owned);
        encode(
            &header,
            claims,
            &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
        )
        .unwrap()
    }

    #[test]
    fn jwks_rejects_symmetric_and_duplicate_keys() {
        let config = config();
        let symmetric = r#"{"keys":[{"kty":"oct","k":"c2VjcmV0","use":"sig","key_ops":["verify"],"alg":"HS256","kid":"bad"}]}"#;
        let error = CapabilityVerifier::from_jwks(&config, symmetric)
            .err()
            .expect("symmetric JWKS should be rejected");
        assert!(error.contains("symmetric"));

        let duplicate = r#"{"keys":[
          {"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWlE9dYa73ZpY7E-wD4mKnyM1UQ","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"same"},
          {"kty":"OKP","crv":"Ed25519","x":"11qYAYKxCrfVS_7TyWlE9dYa73ZpY7E-wD4mKnyM1UQ","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"same"}
        ]}"#;
        let error = CapabilityVerifier::from_jwks(&config, duplicate)
            .err()
            .expect("duplicate JWK IDs should be rejected");
        assert!(error.contains("duplicate"));
    }

    #[test]
    fn jwks_requires_verify_key_operations() {
        let missing = r#"{"keys":[{
          "kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8",
          "use":"sig","alg":"EdDSA","kid":"ed01"
        }]}"#;
        let error = CapabilityVerifier::from_jwks(&config(), missing)
            .err()
            .expect("missing key operations should be rejected");
        assert!(error.contains("key_ops"));

        let mixed = r#"{"keys":[{
          "kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8",
          "use":"sig","key_ops":["verify","sign"],"alg":"EdDSA","kid":"ed01"
        }]}"#;
        let error = CapabilityVerifier::from_jwks(&config(), mixed)
            .err()
            .expect("non-verify key operations should be rejected");
        assert!(error.contains("key_ops"));
    }

    #[test]
    fn jwks_accepts_each_supported_asymmetric_algorithm() {
        let jwks = r#"{"keys":[
          {"kty":"OKP","crv":"Ed25519","x":"2-Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8","use":"sig","key_ops":["verify"],"alg":"EdDSA","kid":"ed01"},
          {"kty":"EC","crv":"P-256","x":"w7JAoU_gJbZJvV-zCOvU9yFJq0FNC_edCMRM78P8eQQ","y":"wQg1EytcsEmGrM70Gb53oluoDbVhCZ3Uq3hHMslHVb4","use":"sig","key_ops":["verify"],"alg":"ES256","kid":"ec01"},
          {"kty":"RSA","n":"yRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5_CYYi_cvI-SXVT9kPWSKXxJXBXd_4LkvcPuUakBoAkfh-eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG_AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi-yUod-j8MtvIj812dkS4QMiRVN_by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQ","e":"AQAB","use":"sig","key_ops":["verify"],"alg":"RS256","kid":"rsa01"}
        ]}"#;
        let verifier = CapabilityVerifier::from_jwks(&config(), jwks).unwrap();
        assert_eq!(verifier.keys.len(), 3);
        assert_eq!(verifier.keys["ed01"].algorithm, Algorithm::EdDSA);
        assert_eq!(verifier.keys["ec01"].algorithm, Algorithm::ES256);
        assert_eq!(verifier.keys["rsa01"].algorithm, Algorithm::RS256);
    }

    #[test]
    fn signed_capability_preserves_exact_authorization_context() {
        let verifier = CapabilityVerifier::from_jwks(&config(), ED25519_JWKS).unwrap();
        let claims = claims();
        let principal = verifier.verify(&sign(&claims, Some("ed01"))).unwrap();

        assert_eq!(principal.profile, HostProfile::OpenClast);
        assert_eq!(principal.exp, Some(claims.exp));
        assert_eq!(principal.action.as_deref(), Some(SEARCH_ACTION));
        assert_eq!(principal.scopes, [Scope::Search]);
        assert_eq!(
            principal.resources,
            claims
                .resources
                .into_iter()
                .map(ResourceKey::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(principal.jti.as_deref(), Some("decision-a"));
        assert_eq!(principal.policy_revision.as_deref(), Some("policy-a"));
        assert_eq!(principal.max_limit, 20);
    }

    #[test]
    fn invalid_signature_context_is_unauthorized() {
        let verifier = CapabilityVerifier::from_jwks(&config(), ED25519_JWKS).unwrap();
        assert_eq!(
            verifier.verify(&sign(&claims(), None)),
            Err(CapabilityFailure::Unauthorized)
        );
        assert_eq!(
            verifier.verify(&sign(&claims(), Some("unknown"))),
            Err(CapabilityFailure::Unauthorized)
        );

        let mut wrong_issuer = claims();
        wrong_issuer.iss = "other-issuer".into();
        assert_eq!(
            verifier.verify(&sign(&wrong_issuer, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );

        let mut overlong = claims();
        overlong.exp = overlong.iat + 61;
        assert_eq!(
            verifier.verify(&sign(&overlong, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );

        let mut self_acted = claims();
        self_acted.actor = Some(self_acted.sub.clone());
        assert_eq!(
            verifier.verify(&sign(&self_acted, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );
    }

    #[test]
    fn capability_time_boundaries_are_fail_closed_without_leeway() {
        let verifier = CapabilityVerifier::from_jwks(&config(), ED25519_JWKS).unwrap();
        let now = unix_time().unwrap();

        let mut expired = claims();
        expired.iat = now.saturating_sub(30);
        expired.nbf = expired.iat;
        expired.exp = now.saturating_sub(1);
        assert_eq!(
            verifier.verify(&sign(&expired, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );

        let mut not_yet_valid = claims();
        not_yet_valid.iat = now;
        not_yet_valid.nbf = now + 30;
        not_yet_valid.exp = now + 60;
        assert_eq!(
            verifier.verify(&sign(&not_yet_valid, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );

        let mut issued_in_the_future = claims();
        issued_in_the_future.iat = now + 30;
        issued_in_the_future.nbf = now;
        issued_in_the_future.exp = now + 60;
        assert_eq!(
            verifier.verify(&sign(&issued_in_the_future, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );
    }

    #[test]
    fn valid_signature_with_excess_authority_is_forbidden() {
        let verifier = CapabilityVerifier::from_jwks(&config(), ED25519_JWKS).unwrap();

        let mut foreign_tenant = claims();
        foreign_tenant.tenant = "tenant-b".into();
        assert_eq!(
            verifier.verify(&sign(&foreign_tenant, Some("ed01"))),
            Err(CapabilityFailure::Forbidden)
        );

        let mut unknown_action = claims();
        unknown_action.actions.push("search:semantic".into());
        assert_eq!(
            verifier.verify(&sign(&unknown_action, Some("ed01"))),
            Err(CapabilityFailure::Forbidden)
        );

        let mut duplicate_resource = claims();
        duplicate_resource
            .resources
            .push(duplicate_resource.resources[0].clone());
        assert_eq!(
            verifier.verify(&sign(&duplicate_resource, Some("ed01"))),
            Err(CapabilityFailure::Forbidden)
        );

        let mut excessive_limit = claims();
        excessive_limit.constraints.max_limit = limit((MAX_SEARCH_LIMIT + 1) as i64);
        assert_eq!(
            verifier.verify(&sign(&excessive_limit, Some("ed01"))),
            Err(CapabilityFailure::Forbidden)
        );
    }

    #[test]
    fn both_shapes_accept_before_cutoff_and_legacy_only_until_explicit_cutoff() {
        let mut config = config();
        config.workload_subject_allowlist = vec!["workload:search".into()];
        config.legacy_identity_accept_until = Some(unix_time().unwrap() + 300);
        let before = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        let legacy = claims();
        let mut workload = claims();
        workload.sub = "workload:search".into();
        workload.actor = None;
        workload.subject_revision = None;
        workload.policy_revision = None;
        let legacy_principal = before.verify(&sign(&legacy, Some("ed01"))).unwrap();
        let workload_principal = before.verify(&sign(&workload, Some("ed01"))).unwrap();
        assert_eq!(
            Principal {
                policy_revision: None,
                ..legacy_principal.clone()
            },
            workload_principal,
            "identity is not retained"
        );
        for principal in [&legacy_principal, &workload_principal] {
            let context = format!("{principal:?}");
            for sentinel in [
                "user-a",
                "openclast-orchestrator",
                "subject-a",
                "workload:search",
                "subject",
                "actor",
            ] {
                assert!(!context.contains(sentinel), "context retained {sentinel}");
            }
            assert_eq!(principal.exp, Some(legacy.exp));
            assert_eq!(principal.action.as_deref(), Some(SEARCH_ACTION));
        }
        config.legacy_identity_accept_until = Some(unix_time().unwrap());
        let after = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        assert_eq!(
            after.verify(&sign(&legacy, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );
        assert!(after.verify(&sign(&workload, Some("ed01"))).is_ok());
        // An unset cutoff must not force workload-only before issuer coordination.
        config.legacy_identity_accept_until = None;
        let unset = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        assert!(unset.verify(&sign(&legacy, Some("ed01"))).is_ok());
    }

    #[test]
    fn capability_typ_is_required_for_workload_and_optional_for_legacy_before_cutoff() {
        let mut config = config();
        config.workload_subject_allowlist = vec!["workload:search".into()];
        config.legacy_identity_accept_until = Some(unix_time().unwrap() + 300);
        let verifier = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        let legacy = claims();
        let mut workload = legacy.clone();
        workload.sub = "workload:search".into();
        workload.actor = None;
        workload.subject_revision = None;
        workload.policy_revision = None;

        let signed = |claims, typ| sign_with_typ(claims, Some("ed01"), typ);
        assert!(
            verifier
                .verify(&signed(&workload, Some(CAPABILITY_TYP)))
                .is_ok()
        );
        assert_eq!(
            verifier.verify(&signed(&workload, None)),
            Err(CapabilityFailure::Unauthorized)
        );
        for typ in [None, Some(CAPABILITY_TYP)] {
            assert!(
                verifier.verify(&signed(&legacy, typ)).is_ok(),
                "typ {typ:?}"
            );
        }

        for (shape, claims) in [("workload", &workload), ("legacy", &legacy)] {
            for typ in [
                "JWT",
                "Kwiry-cap+jwt",
                "openclast-holdings-list+jwt",
                "openclast-obligation-ack+jwt",
                "openclast-holder-status+jwt",
                "openclast-holder-status-ack+jwt",
                "openclast-lifecycle-epoch+jwt",
                "openclast-tombstone-merge+jwt",
                "secevent+jwt",
            ] {
                assert_eq!(
                    verifier.verify(&signed(claims, Some(typ))),
                    Err(CapabilityFailure::Unauthorized),
                    "{shape} shape with typ {typ:?}"
                );
            }
        }

        config.legacy_identity_accept_until = Some(0);
        let after = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        assert_eq!(
            after.verify(&signed(&legacy, None)),
            Err(CapabilityFailure::Unauthorized)
        );
    }

    #[test]
    fn explicit_null_typ_is_not_an_omitted_legacy_typ() {
        let verifier = CapabilityVerifier::from_jwks(&config(), ED25519_JWKS).unwrap();
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","kid":"ed01","typ":null}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims()).unwrap());
        let message = format!("{header}.{payload}");
        let signature = jsonwebtoken::crypto::sign(
            message.as_bytes(),
            &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
            Algorithm::EdDSA,
        )
        .unwrap();
        assert_eq!(
            verifier.verify(&format!("{message}.{signature}")),
            Err(CapabilityFailure::Unauthorized)
        );
    }

    #[test]
    fn workload_shape_is_bytewise_allowlisted_and_cannot_smuggle_identity_claims() {
        let mut config = config();
        config.workload_subject_allowlist = vec!["workload:search".into()];
        config.legacy_identity_accept_until = Some(0);
        let verifier = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        let mut workload = claims();
        workload.actor = None;
        workload.subject_revision = None;
        workload.policy_revision = None;
        for sub in ["user-a", "Workload:search", "workload:search ", ""] {
            workload.sub = sub.into();
            assert_eq!(
                verifier.verify(&sign(&workload, Some("ed01"))),
                Err(CapabilityFailure::Unauthorized)
            );
        }
        workload.sub = "workload:search".into();
        assert!(verifier.verify(&sign(&workload, Some("ed01"))).is_ok());
        workload.subject_revision = Some(Some("identity-revision".into()));
        assert_eq!(
            verifier.verify(&sign(&workload, Some("ed01"))),
            Err(CapabilityFailure::Unauthorized)
        );
        workload.subject_revision = None;
        workload.policy_revision = None;
        for (key, value) in [
            ("actor", serde_json::Value::Null),
            ("subject_revision", serde_json::Value::Null),
            ("unknown_identity", serde_json::json!("secret-identity")),
        ] {
            let mut raw = serde_json::to_value(&workload).unwrap();
            raw[key] = value;
            let token = sign_with_typ(&raw, Some("ed01"), Some(CAPABILITY_TYP));
            assert_eq!(
                verifier.verify(&token),
                Err(CapabilityFailure::Unauthorized),
                "claim {key}"
            );
        }
        // Shared signature/issuer/audience/time/authority checks apply to both shapes.
        for mutate in 0..8 {
            let mut invalid = workload.clone();
            match mutate {
                0 => invalid.iss = "foreign-issuer".into(),
                1 => invalid.aud = "foreign-audience".into(),
                2 => invalid.exp = invalid.iat + 61,
                3 => invalid.nbf = invalid.exp + 1,
                4 => invalid.iat = invalid.exp + 1,
                5 => invalid.actions = vec!["other-action".into()],
                6 => invalid.jti.clear(),
                _ => invalid.constraints.max_limit = limit(101),
            }
            assert!(verifier.verify(&sign(&invalid, Some("ed01"))).is_err());
        }
        assert_eq!(
            verifier.verify(&sign(&workload, None)),
            Err(CapabilityFailure::Unauthorized)
        );
        assert_eq!(
            verifier.verify(&sign(&workload, Some("unknown"))),
            Err(CapabilityFailure::Unauthorized)
        );
    }

    #[test]
    fn unknown_claims_are_ignored_only_by_the_legacy_branch_before_cutover() {
        let mut config = config();
        config.workload_subject_allowlist = vec!["workload:search".into()];
        config.legacy_identity_accept_until = Some(unix_time().unwrap() + 300);
        let before = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        config.legacy_identity_accept_until = Some(0);
        let after = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        let mut legacy = claims();
        legacy.subject_revision = Some(None);
        let mut workload = legacy.clone();
        workload.sub = "workload:search".into();
        workload.actor = None;
        workload.subject_revision = None;
        workload.policy_revision = None;
        let context = before.verify(&sign(&legacy, Some("ed01"))).unwrap();
        for (shape, legacy_shape) in [(legacy, true), (workload, false)] {
            for level in 0..3 {
                let mut raw = serde_json::to_value(&shape).unwrap();
                let target = match level {
                    0 => &mut raw,
                    1 => &mut raw["constraints"],
                    _ => &mut raw["resources"][0],
                };
                target["unknown_identity"] = serde_json::json!("synthetic-identity");
                let token = sign_with_typ(&raw, Some("ed01"), Some(CAPABILITY_TYP));
                let expected = if legacy_shape {
                    Ok(context.clone())
                } else {
                    Err(CapabilityFailure::Unauthorized)
                };
                assert_eq!(before.verify(&token), expected, "claim level {level}");
                assert_eq!(after.verify(&token), Err(CapabilityFailure::Unauthorized));
            }
            let mut header = Header::new(Algorithm::HS256);
            header.kid = Some("ed01".into());
            header.typ = Some(CAPABILITY_TYP.into());
            let token =
                encode(&header, &shape, &EncodingKey::from_secret(b"synthetic-key")).unwrap();
            assert_eq!(before.verify(&token), Err(CapabilityFailure::Unauthorized));
        }
    }

    #[test]
    fn repeated_vault_id_is_refused_even_for_distinct_resource_tuples() {
        let verifier = CapabilityVerifier::from_jwks(&config(), ED25519_JWKS).unwrap();
        let mut repeated = claims();
        let mut second = repeated.resources[0].clone();
        second.room_id = "RoomBbbb_0123456789abc".into();
        repeated.resources.push(second);
        assert_eq!(
            verifier.verify(&sign(&repeated, Some("ed01"))),
            Err(CapabilityFailure::Forbidden)
        );
    }

    fn workload_fixture() -> (CapabilityVerifier, CapabilityClaims) {
        let mut config = config();
        config.workload_subject_allowlist = vec!["openclast:search-gateway".into()];
        config.legacy_identity_accept_until = Some(0);
        let mut workload = claims();
        workload.sub = config.workload_subject_allowlist[0].clone();
        workload.actor = None;
        workload.subject_revision = None;
        workload.policy_revision = None;
        let mut second = workload.resources[0].clone();
        second.room_id = "RoomBbbb_0123456789abc".into();
        workload.resources.push(second);
        (
            CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap(),
            workload,
        )
    }

    #[test]
    fn workload_scope_vectors_n1_to_n7_n28_and_limits() {
        let (verifier, claims) = workload_fixture();
        let check = |claims: &CapabilityClaims, room_format| {
            verifier
                .verify_claims(&sign(claims, Some("ed01")), room_format)
                .map(|(principal, _)| principal.resources)
        };
        assert_eq!(
            check(&claims, false),
            Err(CapabilityFailure::Forbidden),
            "N28"
        );
        assert_eq!(check(&claims, true).unwrap().len(), 2);
        for (name, case) in [
            ("N1", vec![claims.resources[0].clone(); 2]),
            (
                "N2",
                vec![
                    claims.resources[0].clone(),
                    CapabilityResource {
                        vault_id: "VaultTwo_0123456789abc".into(),
                        ..claims.resources[0].clone()
                    },
                ],
            ),
            ("N3 room", claims.resources.iter().cloned().rev().collect()),
            (
                "N3 vault",
                vec![
                    CapabilityResource {
                        vault_id: "VaultTwo_0123456789abc".into(),
                        ..claims.resources[0].clone()
                    },
                    claims.resources[1].clone(),
                ],
            ),
            ("N4", Vec::new()),
            (
                "N5",
                (0..257)
                    .map(|i| CapabilityResource {
                        room_id: format!("Room_{i:017}"),
                        ..claims.resources[0].clone()
                    })
                    .collect(),
            ),
            (
                "N6",
                vec![
                    claims.resources[0].clone(),
                    CapabilityResource {
                        tenant_id: "tenant_BBBBBBBBBBBBBBBBBBBB".into(),
                        ..claims.resources[1].clone()
                    },
                ],
            ),
        ] {
            let mut invalid = claims.clone();
            invalid.resources = case;
            assert_eq!(
                check(&invalid, true),
                Err(CapabilityFailure::Forbidden),
                "{name}"
            );
        }
        for malformed in [
            "R".repeat(21),
            "R".repeat(23),
            "RoomAaaa_0123456789ab:".into(),
            "RoomAaaa_0123456789ab=".into(),
            "RoomAaaa_0123456789ab.".into(),
            "RoomAaaa_0123456789abé".into(),
            " RoomAaaa_0123456789ab".into(),
            "RoomAaaa_0123456789ab\0".into(),
            "RoomAaaa_0123456789ab\n".into(),
        ] {
            let mut invalid = claims.clone();
            invalid.resources[0].room_id = malformed.clone();
            assert_eq!(
                check(&invalid, true),
                Err(CapabilityFailure::Forbidden),
                "N7 room"
            );
            let mut invalid = claims.clone();
            invalid.resources[0].vault_id = malformed;
            assert_eq!(
                check(&invalid, true),
                Err(CapabilityFailure::Forbidden),
                "N7 vault"
            );
        }
        for malformed in ["tenant_AAAAAAAAAAAAAAAAAAA", "tenant_AAAAAAAAAAAAAAAAAAAA "] {
            let mut invalid = claims.clone();
            invalid.tenant = malformed.into();
            assert_eq!(
                check(&invalid, true),
                Err(CapabilityFailure::Forbidden),
                "N7 tenant"
            );
            let mut invalid = claims.clone();
            invalid.resources[0].tenant_id = malformed.into();
            assert_eq!(
                check(&invalid, true),
                Err(CapabilityFailure::Forbidden),
                "N7 tenant_id"
            );
        }
        for invalid_limit in [-1, 0, 101] {
            let mut invalid = claims.clone();
            invalid.constraints.max_limit = limit(invalid_limit);
            assert_eq!(
                check(&invalid, true),
                Err(CapabilityFailure::Forbidden),
                "R3 range"
            );
        }
    }

    #[test]
    fn closed_claim_vectors_n8_to_n13_and_n22() {
        use serde_json::{Value, json};
        let (verifier, workload) = workload_fixture();
        let unauthorized = |token: &str| {
            assert!(matches!(
                verifier.verify_claims(token, true),
                Err(CapabilityFailure::Unauthorized)
            ))
        };
        let verify_value =
            |raw: &Value| unauthorized(&sign_with_typ(raw, Some("ed01"), Some(CAPABILITY_TYP)));
        let original = serde_json::to_value(&workload).unwrap();
        for (key, value) in [
            ("alias", json!("notes")),
            ("mode", json!("ro")),
            ("path_prefix", json!("")),
            ("rooms", json!(["RoomBbbb_0123456789abc"])),
        ] {
            let mut raw = original.clone();
            raw["resources"][0][key] = value;
            verify_value(&raw); // N8
        }
        let mut raw = original.clone();
        raw["resources"][0]
            .as_object_mut()
            .unwrap()
            .remove("room_id");
        raw["resources"][0]["rooms"] = json!(["RoomAaaa_0123456789abc", "RoomBbbb_0123456789abc"]);
        verify_value(&raw); // N9
        for value in [Value::Null, json!("1"), json!(true), json!(1.25)] {
            let mut raw = original.clone();
            raw["constraints"]["max_limit"] = value;
            verify_value(&raw); // R3 requires a JSON integer.
        }
        let mut enormous = workload.clone();
        enormous.constraints.max_limit =
            serde_json::value::RawValue::from_string("9".repeat(80)).unwrap();
        assert!(matches!(
            verifier.verify_claims(&sign(&enormous, Some("ed01")), true),
            Err(CapabilityFailure::Forbidden)
        ));
        for value in [
            Value::Null,
            json!(123),
            json!(["RoomAaaa_0123456789abc"]),
            json!({}),
        ] {
            let mut raw = original.clone();
            raw["resources"][0]["room_id"] = value;
            verify_value(&raw); // N10
        }
        let mut legacy_config = config();
        legacy_config.legacy_identity_accept_until = Some(unix_time().unwrap() + 60);
        let before = CapabilityVerifier::from_jwks(&legacy_config, ED25519_JWKS).unwrap();
        let mut raw = original.clone();
        raw["actor"] = json!("orchestrator");
        for typ in [None, Some(CAPABILITY_TYP)] {
            assert!(matches!(
                before.verify_claims(&sign_with_typ(&raw, Some("ed01"), typ), true),
                Err(CapabilityFailure::Forbidden)
            )); // N11
        }
        unauthorized(&sign_with_typ(&raw, Some("ed01"), None));
        raw["resources"][0]["unknown"] = json!(true);
        verify_value(&raw);
        for value in [json!("revision"), Value::Null] {
            let mut raw = original.clone();
            raw["policy_revision"] = value;
            verify_value(&raw); // N12
        }
        let mut raw = original.clone();
        raw["resources"][1] = raw["resources"][0].clone();
        raw["resources"][0]["extraneous"] = json!(true);
        verify_value(&raw); // N22

        let header = r#"{"alg":"EdDSA","kid":"ed01","typ":"kwiry-cap+jwt"}"#;
        let payload = original.to_string();
        let verify_raw = |header: &str, payload: &str| {
            let input = format!(
                "{}.{}",
                URL_SAFE_NO_PAD.encode(header),
                URL_SAFE_NO_PAD.encode(payload)
            );
            let signature = jsonwebtoken::crypto::sign(
                input.as_bytes(),
                &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
                Algorithm::EdDSA,
            )
            .unwrap();
            unauthorized(&format!("{input}.{signature}"));
        };
        for member in [
            r#""typ":"kwiry-cap+jwt""#,
            r#""alg":"EdDSA""#,
            r#""kid":"ed01""#,
            r#""crit":[]"#,
        ] {
            verify_raw(
                &format!("{{{member},{}}}", &header[1..header.len() - 1]),
                &payload,
            );
        }
        for key in ["resources", "exp", "aud", "tenant"] {
            verify_raw(
                header,
                &format!(
                    "{{\"{key}\":{},{}}}",
                    original[key],
                    &payload[1..payload.len() - 1]
                ),
            );
        }
        verify_raw(
            header,
            &format!(
                r#"{{"actor":"first","actor":"second",{} }}"#,
                &payload[1..payload.len() - 1]
            ),
        );
        verify_raw(
            header,
            &payload.replacen(
                r#""room_id":"RoomAaaa_0123456789abc""#,
                r#""room_id":"RoomAaaa_0123456789abc","room_id":"RoomCccc_0123456789abc""#,
                1,
            ),
        );
    }

    #[test]
    fn n30_format_gate_leaves_exact_replay_jti_and_count_unchanged() {
        let (mut verifier, mut claims) = workload_fixture();
        verifier.room_format = kwiry_core::OpenClastRoomFormat::from_build_flags(
            ROOM_INCLUSIVE_IDS,
            kwiry_core::HOLDINGS_SERVING_IMPLEMENTED,
        );
        let directory = tempfile::tempdir().unwrap();
        claims.jti = "synthetic-n30-exact-jti".into();
        let replay = Arc::new(ReplayState::start(directory.path(), claims.iat - 2).unwrap());
        verifier.replay = Some(replay.clone());
        let before = replay.test_entry_state(&claims.jti);
        assert_eq!(before, (false, 0));
        assert_eq!(
            verifier.verify_with_format(&sign(&claims, Some("ed01")), true),
            Err(CapabilityFailure::Forbidden)
        );
        assert_eq!(replay.test_entry_state(&claims.jti), before);
        claims.resources.pop();
        assert!(
            verifier
                .verify_with_format(&sign(&claims, Some("ed01")), true)
                .is_ok()
        );
        assert_eq!(replay.test_entry_state(&claims.jti), (true, 1));
    }

    #[test]
    fn repeated_capability_requires_fresh_replay_state_n17_n18() {
        let (mut verifier, mut workload) = workload_fixture();
        let directory = tempfile::tempdir().unwrap();
        let now = workload.iat;
        let iat = now - 2;
        workload.iat = iat;
        workload.nbf = iat;
        workload.exp = iat + 60;
        verifier.replay = Some(Arc::new(
            ReplayState::start(directory.path(), iat - 1).unwrap(),
        ));
        let valid = sign(&workload, Some("ed01"));
        assert_eq!(
            verifier.verify_with_format(&valid, false),
            Err(CapabilityFailure::Forbidden)
        );
        assert!(verifier.verify_with_format(&valid, true).is_ok());
        assert_eq!(
            verifier.verify_with_format(&valid, true),
            Err(CapabilityFailure::Unauthorized)
        );
        verifier.replay = None;
        verifier.replay = Some(Arc::new(
            ReplayState::start(directory.path(), iat + 1).unwrap(),
        ));
        assert_eq!(
            verifier.verify_with_format(&valid, true),
            Err(CapabilityFailure::Unauthorized)
        );
        workload.iat = now;
        workload.jti = "fresh-after-restart".into();
        assert!(
            verifier
                .verify_with_format(&sign(&workload, Some("ed01")), true)
                .is_ok()
        );
        workload.iat = unix_time().unwrap() + 1;
        assert_eq!(
            verifier.verify_with_format(&sign(&workload, Some("ed01")), true),
            Err(CapabilityFailure::Unauthorized)
        );
    }

    #[test]
    fn case_tail_and_maximum_cardinality_are_bytewise() {
        let (verifier, mut workload) = workload_fixture();
        let verify = |claims: &CapabilityClaims| {
            verifier
                .verify_claims(&sign(claims, Some("ed01")), true)
                .unwrap()
                .0
        };
        workload.resources[1].room_id = "roomAaaa_0123456789abc".into();
        assert_eq!(verify(&workload).resources.len(), 2); // P9 verifier half
        workload.resources[0].room_id = "RoomXxxx_0123456789abc".into();
        workload.resources[1].room_id = "RoomXxxx_0123456789abd".into();
        assert_eq!(verify(&workload).resources.len(), 2); // P15 verifier half
        workload.resources = (0..256)
            .map(|index| CapabilityResource {
                room_id: format!("Room_{index:017}"),
                ..workload.resources[0].clone()
            })
            .collect();
        assert_eq!(verify(&workload).resources.len(), 256); // P4 verifier half
    }
}
