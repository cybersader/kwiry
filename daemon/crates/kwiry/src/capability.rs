use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::jwk::{
    AlgorithmParameters, EllipticCurve, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse,
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use kwiry_core::{HostProfile, OpenClastAuthConfig, Principal, ResourceKey, Scope};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

const MAX_RESOURCES: usize = 256;
const MAX_SEARCH_LIMIT: usize = 100;
const SEARCH_ACTION: &str = "search:lexical";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityFailure {
    Unauthorized,
    Forbidden,
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
    #[serde(default)]
    policy_revision: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_optional_string"
    )]
    subject_revision: Option<Option<String>>,
    #[serde(flatten, skip_serializing)]
    unknown: HashMap<String, IgnoredAny>,
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
    max_limit: usize,
    #[serde(flatten, skip_serializing)]
    unknown: HashMap<String, IgnoredAny>,
}

fn present_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

// Legacy issuers could send null; workload issuers must omit the key entirely.
fn present_optional_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

impl CapabilityVerifier {
    pub(crate) fn load(config: &OpenClastAuthConfig) -> Result<Self, String> {
        let source = fs::read_to_string(&config.jwks_file).map_err(|error| {
            format!(
                "could not read OpenClast JWKS at {}: {error}",
                config.jwks_file.display()
            )
        })?;
        Self::from_jwks(config, &source)
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
        })
    }

    pub(crate) fn verify(&self, token: &str) -> Result<Principal, CapabilityFailure> {
        let header = decode_header(token).map_err(|_| CapabilityFailure::Unauthorized)?;
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
        let claims = decode::<CapabilityClaims>(token, &key.decoding_key, &validation)
            .map_err(|_| CapabilityFailure::Unauthorized)?
            .claims;
        self.validate_claims(claims)
    }

    fn validate_claims(&self, claims: CapabilityClaims) -> Result<Principal, CapabilityFailure> {
        if claims.iss != self.issuer
            || claims.aud != self.audience
            || claims.sub.trim().is_empty()
            || claims.jti.trim().is_empty()
        {
            return Err(CapabilityFailure::Unauthorized);
        }
        let now = unix_time().map_err(|_| CapabilityFailure::Unauthorized)?;
        match claims.actor.as_deref() {
            None if claims.subject_revision.is_none()
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
        if claims.tenant != self.tenant_id
            || claims.actions != [SEARCH_ACTION]
            || claims.resources.is_empty()
            || claims.resources.len() > MAX_RESOURCES
            || !(1..=MAX_SEARCH_LIMIT).contains(&claims.constraints.max_limit)
        {
            return Err(CapabilityFailure::Forbidden);
        }

        let mut resources = BTreeSet::new();
        let mut vault_ids = BTreeSet::new();
        for resource in claims.resources {
            let resource = ResourceKey::from(resource);
            if resource.tenant_id != self.tenant_id
                || resource.tenant_id.trim().is_empty()
                || resource.vault_id.trim().is_empty()
                || resource.room_id.trim().is_empty()
                || !vault_ids.insert(resource.vault_id.clone())
                || !resources.insert(resource)
            {
                return Err(CapabilityFailure::Forbidden);
            }
        }

        Ok(Principal {
            profile: HostProfile::OpenClast,
            exp: Some(claims.exp),
            action: Some(SEARCH_ACTION.to_owned()),
            scopes: vec![Scope::Search],
            resources: resources.into_iter().collect(),
            jti: Some(claims.jti),
            policy_revision: claims.policy_revision,
            max_limit: claims.constraints.max_limit,
        })
    }
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

fn unix_time() -> Result<u64, String> {
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
            tenant_id: "tenant-a".into(),
            issuer: "issuer".into(),
            audience: "kwiry-search".into(),
            jwks_file: Path::new("/unused").to_path_buf(),
            max_token_ttl_seconds: 60,
            workload_subject_allowlist: Vec::new(),
            legacy_identity_accept_until: None,
        }
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
            tenant: "tenant-a".into(),
            actions: vec![SEARCH_ACTION.into()],
            resources: vec![CapabilityResource {
                tenant_id: "tenant-a".into(),
                vault_id: "vault-a".into(),
                room_id: "room-a".into(),
                unknown: HashMap::new(),
            }],
            constraints: CapabilityConstraints {
                max_limit: 20,
                unknown: HashMap::new(),
            },
            policy_revision: Some("policy-a".into()),
            subject_revision: Some(Some("subject-a".into())),
            unknown: HashMap::new(),
        }
    }

    fn sign(claims: &CapabilityClaims, kid: Option<&str>) -> String {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = kid.map(str::to_owned);
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
        excessive_limit.constraints.max_limit = MAX_SEARCH_LIMIT + 1;
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
        let legacy_principal = before.verify(&sign(&legacy, Some("ed01"))).unwrap();
        let workload_principal = before.verify(&sign(&workload, Some("ed01"))).unwrap();
        assert_eq!(
            legacy_principal, workload_principal,
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
    fn workload_shape_is_bytewise_allowlisted_and_cannot_smuggle_identity_claims() {
        let mut config = config();
        config.workload_subject_allowlist = vec!["workload:search".into()];
        config.legacy_identity_accept_until = Some(0);
        let verifier = CapabilityVerifier::from_jwks(&config, ED25519_JWKS).unwrap();
        let mut workload = claims();
        workload.actor = None;
        workload.subject_revision = None;
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
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("ed01".into());
        for (key, value) in [
            ("actor", serde_json::Value::Null),
            ("subject_revision", serde_json::Value::Null),
            ("unknown_identity", serde_json::json!("secret-identity")),
        ] {
            let mut raw = serde_json::to_value(&workload).unwrap();
            raw[key] = value;
            let token = encode(
                &header,
                &raw,
                &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
            )
            .unwrap();
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
                _ => invalid.constraints.max_limit = 101,
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
                let mut header = Header::new(Algorithm::EdDSA);
                header.kid = Some("ed01".into());
                let token = encode(
                    &header,
                    &raw,
                    &EncodingKey::from_ed_der(ED25519_PRIVATE_DER),
                )
                .unwrap();
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
        second.room_id = "other-room".into();
        repeated.resources.push(second);
        assert_eq!(
            verifier.verify(&sign(&repeated, Some("ed01"))),
            Err(CapabilityFailure::Forbidden)
        );
    }
}
