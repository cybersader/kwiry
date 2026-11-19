//! A reference fake authority for the list route and the key-set route.
//!
//! It evaluates a list request in the v2 H.12 single validation order: size →
//! authority state → signature → rollback witness → time → replay → scope →
//! rate → grammar → cursor. It signs pages with ES256, computes
//! `state_digest` and `list_tag` with HKDF/HMAC over canonical JSON, seals
//! cursors bound to the holder's projection revision (`holder_rev`), and
//! records every request. Tests mutate it between and during requests.

use super::fakes::FakeClock;
use super::{TEST_AUTHORITY_KEY_SEED, TEST_CURSOR_KEY, TEST_DIGEST_ROOT, jcs, pin_id};
use crate::client::{HttpResponse, Timeouts, Transport, TransportError};
use crate::clock::{Clock, format_utc_seconds};
use crate::keys::{HolderKey, authority_public_jwk, ed25519_public_from_jwk};
use crate::profile::{PROBLEM_CONTENT_TYPE, ROUTE_KEYSET, ROUTE_LIST, WireProfile};
use crate::{b64, grammar, jws};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

pub const REQUEST_MAX_BYTES: usize = 16 * 1024;
const SKEW_S: i64 = 120;

#[derive(Clone, Debug)]
pub struct AuthorityConfig {
    pub profile: WireProfile,
    pub issuer: String,
    /// HKDF salt for the per-holder digest key.
    pub digest_salt: String,
    pub holder_kind: String,
    pub serve_seconds: u64,
    pub relist_seconds: u64,
}

/// A response the fake returns instead of processing the next request.
pub enum Forced {
    Response(HttpResponse),
    Unreachable,
    Timeout,
}

/// Rewrites one JSON object of an artefact before it is signed.
pub type MapHook = Box<dyn Fn(&mut Map<String, Value>) + Send>;

/// Ways to make the fake emit artefacts a correct authority never would.
#[derive(Default)]
pub struct Hooks {
    pub item: Option<MapHook>,
    pub page: Option<MapHook>,
    pub typ: Option<String>,
    pub alg: Option<String>,
    pub kid: Option<String>,
    pub foreign_key: Option<p256::ecdsa::SigningKey>,
    pub content_type: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub claims: Option<Map<String, Value>>,
    pub status: u16,
}

type Mutation = Box<dyn FnOnce(&mut FakeState) + Send>;

pub struct FakeState {
    pub config: AuthorityConfig,
    clock: Arc<FakeClock>,
    /// Authority clock minus holder wall clock.
    pub offset_ms: i64,
    /// Simulated transit per request, split before and after processing.
    pub latency_ms: u64,
    keys: BTreeMap<String, p256::ecdsa::SigningKey>,
    pub active_kid: String,
    pub keyset_statement: Option<String>,
    pub attach_keyset: bool,
    keyset_seq: u64,
    pub digest_root: [u8; 32],
    cursor_key: [u8; 32],
    pub epoch: String,
    pub seq: u64,
    pub lineage: Vec<String>,
    pub holder_id: String,
    holder_key: Option<ed25519_dalek::VerifyingKey>,
    holder_jkt: String,
    pub holder_rev: u64,
    /// Wire items by holding id, without `quarantine_after` (computed at
    /// projection time).
    pub items: BTreeMap<String, Value>,
    pub instructions: Vec<Value>,
    pub disabled: bool,
    pub rollback_suspected: bool,
    pub custody_unreadable: bool,
    seen_jti: HashSet<String>,
    last_list_start_mono: Option<u64>,
    exempt_next_start: bool,
    pub forced: VecDeque<Forced>,
    pub hooks: Hooks,
    scheduled: Vec<(usize, Mutation)>,
    posts: usize,
    pub requests: Vec<RecordedRequest>,
    pub audit: Vec<&'static str>,
    /// The last page served, for replaying it later.
    pub last_page: Option<HttpResponse>,
    /// Every POST body received, forced answers included.
    pub bodies: Vec<Vec<u8>>,
    counter: u64,
}

#[derive(Clone)]
pub struct FakeAuthority(Arc<Mutex<FakeState>>);

fn signing_key(seed: [u8; 32]) -> p256::ecdsa::SigningKey {
    p256::ecdsa::SigningKey::from_slice(&seed).expect("test seed is a valid scalar")
}

impl FakeAuthority {
    pub fn new(config: AuthorityConfig, clock: Arc<FakeClock>) -> Self {
        let kid = format!("{}20261117-0a0a", config.profile.authority_key_id_prefix);
        let mut keys = BTreeMap::new();
        keys.insert(kid.clone(), signing_key(TEST_AUTHORITY_KEY_SEED));
        let epoch = "ep_TESTepoch0000000000001".to_owned();
        Self(Arc::new(Mutex::new(FakeState {
            config,
            clock,
            offset_ms: 0,
            latency_ms: 0,
            keys,
            active_kid: kid,
            keyset_statement: None,
            attach_keyset: false,
            keyset_seq: 0,
            digest_root: TEST_DIGEST_ROOT,
            cursor_key: TEST_CURSOR_KEY,
            lineage: vec![epoch.clone()],
            epoch,
            seq: 100,
            holder_id: "hd_TESTholder000000000001".into(),
            holder_key: None,
            holder_jkt: String::new(),
            holder_rev: 100,
            items: BTreeMap::new(),
            instructions: Vec::new(),
            disabled: false,
            rollback_suspected: false,
            custody_unreadable: false,
            seen_jti: HashSet::new(),
            last_list_start_mono: None,
            exempt_next_start: false,
            forced: VecDeque::new(),
            hooks: Hooks::default(),
            scheduled: Vec::new(),
            posts: 0,
            requests: Vec::new(),
            audit: Vec::new(),
            last_page: None,
            bodies: Vec::new(),
            counter: 0,
        })))
    }

    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Enrolls the holder key; returns the holder id and the bare JWKS a
    /// holder pins at enrollment.
    pub fn enroll(&self, key: &HolderKey) -> (String, Value) {
        let mut state = self.state();
        state.holder_key = ed25519_public_from_jwk(&key.public_jwk());
        state.holder_jkt = key.thumbprint();
        let kid = state.active_kid.clone();
        let jwk = authority_public_jwk(&kid, state.keys[&kid].verifying_key());
        (state.holder_id.clone(), json!({ "keys": [jwk] }))
    }
}

impl FakeState {
    pub fn now_ms(&self) -> i64 {
        self.clock.wall_ms() + self.offset_ms
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{:022}", self.counter)
    }

    /// One governance transaction that changes this holder's projection.
    pub fn commit(&mut self) {
        self.seq += 1;
        self.holder_rev = self.seq;
    }

    fn pinned(&self, kind: &str) -> bool {
        self.config.profile.pins(&self.config.holder_kind, kind)
    }

    /// Adds an `active` holding; pinned kinds start with `root_pin: null`.
    pub fn add_active(&mut self, kind: &str, id: &str) -> String {
        let holding_id = self.next_id("hl_");
        let mut item = json!({
            "holding_id": holding_id,
            "resource": { "kind": kind, "id": id },
            "phase": "active",
            "obligation": null,
        });
        if self.pinned(kind) {
            item["root_pin"] = Value::Null;
        }
        self.items.insert(holding_id.clone(), item);
        self.commit();
        holding_id
    }

    /// Attests a pin observed at `label`; returns the fresh `pin_ref`.
    pub fn attest(&mut self, holding_id: &str, label: &str) -> String {
        self.attest_pin_id(holding_id, &pin_id(label))
    }

    pub fn attest_pin_id(&mut self, holding_id: &str, pin: &str) -> String {
        let pin_ref = self.next_id("pr_");
        self.items.get_mut(holding_id).expect("holding")["root_pin"] =
            json!({ "pin_id": pin, "pin_ref": pin_ref });
        self.commit();
        pin_ref
    }

    pub fn unpin(&mut self, holding_id: &str) {
        self.items.get_mut(holding_id).expect("holding")["root_pin"] = Value::Null;
        self.commit();
    }

    /// `active → terminating` with an open retire obligation.
    pub fn retire(&mut self, holding_id: &str) -> String {
        let obligation_id = self.next_id("ob_");
        let now = self.now_ms();
        let created_seq = self.seq + 1;
        let item = self.items.get_mut(holding_id).expect("holding");
        item["phase"] = "terminating".into();
        item["obligation"] = json!({
            "obligation_id": obligation_id,
            "reason": "retire",
            "required": "purge",
            "state": "open",
            "created_seq": created_seq,
            "created_at": format_utc_seconds(now),
            "deadline": format_utc_seconds(now + 7 * 86_400_000),
            "last_accepted": null,
        });
        self.commit();
        obligation_id
    }

    /// Sets the obligation state; `complete` also moves the phase to
    /// `retired`, `blocked` adds the decline record.
    pub fn set_obligation_state(&mut self, holding_id: &str, state: &str) {
        let at = format_utc_seconds(self.now_ms());
        let item = self.items.get_mut(holding_id).expect("holding");
        let obligation = item["obligation"].as_object_mut().expect("obligation");
        obligation.insert("state".into(), state.into());
        obligation.remove("blocked");
        match state {
            "complete" => {
                obligation.insert(
                    "last_accepted".into(),
                    json!({ "state": "complete", "at": at }),
                );
                item["phase"] = "retired".into();
            }
            "blocked" => {
                obligation.insert(
                    "blocked".into(),
                    json!({ "by": "release_declined", "at": at, "status_report_digest": "sha256:00" }),
                );
            }
            _ => {}
        }
        self.commit();
    }

    pub fn remove(&mut self, holding_id: &str) {
        self.items.remove(holding_id);
        self.commit();
    }

    pub fn new_epoch(&mut self) {
        self.epoch = self.next_id("ep_");
        self.lineage.push(self.epoch.clone());
        self.commit();
    }

    /// A raw restore: `seq` rolls back without a new epoch.
    pub fn raw_restore(&mut self, seq: u64) {
        self.seq = seq;
        self.holder_rev = seq;
    }

    /// Adds a signing key, publishes a key-set statement signed by the
    /// current key, and signs pages with the new key from now on.
    pub fn rotate_key(&mut self, seed: [u8; 32], attach: bool, retire_old: bool) -> String {
        let kid = format!(
            "{}20261118-{:04x}",
            self.config.profile.authority_key_id_prefix,
            self.keyset_seq + 1
        );
        let key = signing_key(seed);
        let old = self.active_kid.clone();
        self.keyset_seq += 1;
        let claims = json!({
            "iss": self.config.issuer,
            "aud": self.config.profile.broadcast_audience(),
            "iat": self.now_ms().div_euclid(1000),
            "keyset_seq": self.keyset_seq,
            "keys": [authority_public_jwk(&kid, key.verifying_key())],
            "retire_kids": if retire_old { vec![old.clone()] } else { Vec::new() },
        });
        self.keyset_statement = Some(jws::sign_es256(
            &self.config.profile.typ_keyset,
            &old,
            &claims,
            &self.keys[&old],
        ));
        self.attach_keyset = attach;
        self.keys.insert(kid.clone(), key);
        self.active_kid = kid.clone();
        kid
    }

    /// Publishes a key-set statement signed by the current key, adding a key
    /// per seed, retiring `retire` at once and, with `not_after_old_s`, every
    /// earlier key not added here at that instant. Pages keep their current
    /// signing key until [`FakeState::use_key`]. Returns the added kids.
    pub fn issue_keyset(
        &mut self,
        seeds: &[[u8; 32]],
        retire: &[&str],
        not_after_old_s: Option<i64>,
        attach: bool,
    ) -> Vec<String> {
        self.keyset_seq += 1;
        let mut kids = Vec::new();
        let mut jwks = Vec::new();
        for (index, seed) in seeds.iter().enumerate() {
            let kid = format!(
                "{}20261119-{:02x}{index:02x}",
                self.config.profile.authority_key_id_prefix, self.keyset_seq
            );
            let key = signing_key(*seed);
            jwks.push(authority_public_jwk(&kid, key.verifying_key()));
            self.keys.insert(kid.clone(), key);
            kids.push(kid);
        }
        let mut claims = json!({
            "iss": self.config.issuer,
            "aud": self.config.profile.broadcast_audience(),
            "iat": self.now_ms().div_euclid(1000),
            "keyset_seq": self.keyset_seq,
            "keys": jwks,
            "retire_kids": retire,
        });
        if let Some(limit) = not_after_old_s {
            claims["not_after_old"] = limit.into();
        }
        let signer = self.active_kid.clone();
        self.keyset_statement =
            Some(self.sign_as(&signer, &self.config.profile.typ_keyset.clone(), &claims));
        self.attach_keyset = attach;
        kids
    }

    /// Signs pages with `kid` from now on.
    pub fn use_key(&mut self, kid: &str) {
        assert!(self.keys.contains_key(kid), "unknown test kid");
        self.active_kid = kid.to_owned();
    }

    /// Signs arbitrary claims with the current key (key-set statements a
    /// correct authority would not issue).
    pub fn sign_with_active(&self, typ: &str, claims: &Value) -> String {
        self.sign_as(&self.active_kid, typ, claims)
    }

    pub fn sign_as(&self, kid: &str, typ: &str, claims: &Value) -> String {
        jws::sign_es256(typ, kid, claims, &self.keys[kid])
    }

    /// Runs `mutation` just before the `n`th POST (1-based, counting all).
    pub fn before_post(
        &mut self,
        n: usize,
        mutation: impl FnOnce(&mut FakeState) + Send + 'static,
    ) {
        self.scheduled.push((n, Box::new(mutation)));
    }

    pub fn posts(&self) -> usize {
        self.posts
    }

    pub fn list_claims(&self) -> Vec<Map<String, Value>> {
        self.requests
            .iter()
            .filter_map(|r| r.claims.clone())
            .collect()
    }

    fn has_open_obligations(&self) -> bool {
        self.items.values().any(|i| i["phase"] == "terminating")
    }

    fn digest_key(&self) -> [u8; 32] {
        let hkdf = Hkdf::<Sha256>::new(Some(self.config.digest_salt.as_bytes()), &self.digest_root);
        let mut info = b"state-digest".to_vec();
        info.extend_from_slice(self.holder_id.as_bytes());
        let mut key = [0u8; 32];
        hkdf.expand(&info, &mut key).expect("32-byte okm");
        key
    }

    fn mac(key: &[u8], label: &[u8], content: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac key");
        mac.update(label);
        mac.update(&[0]);
        mac.update(content);
        b64::encode(&mac.finalize().into_bytes())
    }

    fn state_digest(&self, list_id: &str, nonce: &str, rev: &Value, items: &[Value]) -> String {
        let entries: Vec<Value> = items
            .iter()
            .map(|item| {
                let obligation = &item["obligation"];
                let mut entry = json!({
                    "holding_id": item["holding_id"],
                    "resource": item["resource"],
                    "phase": item["phase"],
                    "obligation_id": obligation.get("obligation_id").cloned().unwrap_or(Value::Null),
                    "obligation_state": obligation.get("state").cloned().unwrap_or(Value::Null),
                });
                if let Some(pin) = item.get("root_pin") {
                    entry["root_pin"] = pin.clone();
                }
                entry
            })
            .collect();
        let content = jcs(&json!({
            "holder_id": self.holder_id,
            "rev": rev,
            "list_id": list_id,
            "nonce": nonce,
            "items": entries,
        }));
        format!(
            "hs1.{}",
            Self::mac(&self.digest_key(), b"content", content.as_bytes())
        )
    }

    fn list_tag(&self, list_id: &str, rev: &Value, nonce: &str, digest: &str) -> String {
        let content = jcs(&json!({
            "holder_id": self.holder_id,
            "list_id": list_id,
            "rev": rev,
            "nonce": nonce,
            "state_digest": digest,
        }));
        format!(
            "lt1.{}",
            Self::mac(&self.digest_key(), b"issue", content.as_bytes())
        )
    }

    fn project(&self, page0_iat_s: i64) -> Vec<Value> {
        let lease_end = format_utc_seconds(
            (page0_iat_s + i64::try_from(self.config.serve_seconds).unwrap_or(0)) * 1000,
        );
        self.items
            .values()
            .map(|item| {
                let mut item = item.clone();
                item["quarantine_after"] = if item["phase"] == "active" {
                    lease_end.clone().into()
                } else {
                    item["obligation"]["created_at"].clone()
                };
                item
            })
            .collect()
    }

    fn problem(
        &mut self,
        claims: Option<Map<String, Value>>,
        status: u16,
        code: &str,
    ) -> HttpResponse {
        if matches!(code, "list_expired" | "replay" | "proof_expired") {
            self.exempt_next_start = true;
        }
        self.requests.push(RecordedRequest { claims, status });
        HttpResponse {
            status,
            content_type: Some(PROBLEM_CONTENT_TYPE.into()),
            retry_after_s: (status == 429).then_some(5),
            body: json!({ "type": "about:blank", "status": status, "code": code })
                .to_string()
                .into_bytes(),
        }
    }

    /// `{list_id, rev, nonce, state_digest, list_tag}` with exact grammar.
    fn well_formed_ref(value: &Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        let text = |name: &str| object.get(name).and_then(Value::as_str);
        object.len() == 5
            && text("list_id").is_some_and(grammar::list_id)
            && text("nonce").is_some_and(grammar::nonce)
            && text("state_digest").is_some_and(|d| grammar::tagged_mac(d, "hs1."))
            && text("list_tag").is_some_and(|t| grammar::tagged_mac(t, "lt1."))
            && object
                .get("rev")
                .is_some_and(|rev| crate::verify::parse_rev(rev).is_ok())
    }

    fn tag_valid(&self, reference: &Value) -> bool {
        let text = |name: &str| reference[name].as_str().unwrap_or_default();
        self.list_tag(
            text("list_id"),
            &reference["rev"],
            text("nonce"),
            text("state_digest"),
        ) == text("list_tag")
    }

    fn seal(&self, payload: &Value) -> String {
        let bytes = payload.to_string().into_bytes();
        format!(
            "{}.{}",
            b64::encode(&bytes),
            Self::mac(&self.cursor_key, b"cursor", &bytes)
        )
    }

    fn unseal(&self, cursor: &str) -> Option<Value> {
        let (payload, mac) = cursor.split_once('.')?;
        let bytes = b64::decode(payload)?;
        (Self::mac(&self.cursor_key, b"cursor", &bytes) == mac)
            .then(|| serde_json::from_slice(&bytes).ok())
            .flatten()
    }

    fn list_grammar(claims: &Map<String, Value>) -> bool {
        const ALLOWED: [&str; 13] = [
            "iss",
            "sub",
            "aud",
            "htu",
            "iat",
            "exp",
            "jti",
            "op",
            "nonce",
            "high_water",
            "page_size",
            "cursor",
            "last_list_ref",
        ];
        let text = |name: &str| claims.get(name).and_then(Value::as_str);
        claims.keys().all(|k| ALLOWED.contains(&k.as_str()))
            && ALLOWED[..12].iter().all(|k| claims.contains_key(*k))
            && text("op") == Some("list")
            && text("nonce").is_some_and(grammar::nonce)
            && text("jti").is_some_and(grammar::jti)
            && match &claims["high_water"] {
                Value::Null => true,
                rev => crate::verify::parse_rev(rev).is_ok(),
            }
            && claims["page_size"]
                .as_u64()
                .is_some_and(|size| (1..=1000).contains(&size))
            && (claims["cursor"].is_null() || claims["cursor"].is_string())
            && claims
                .get("last_list_ref")
                .is_none_or(Self::well_formed_ref)
    }

    fn handle_list(&mut self, content_type: &str, body: &[u8]) -> HttpResponse {
        // 1. Size.
        if body.len() > REQUEST_MAX_BYTES {
            return self.problem(None, 413, "payload_too_large");
        }
        // 2. Authority state.
        if self.rollback_suspected {
            return self.problem(None, 503, "authority_epoch_mismatch");
        }
        if self.custody_unreadable {
            return self.problem(None, 503, "lifecycle_unavailable");
        }
        // 3. Signature.
        let issuer = self.config.issuer.clone();
        let verified = std::str::from_utf8(body)
            .ok()
            .and_then(|text| jws::parse(text).ok())
            .filter(|request| {
                content_type == self.config.profile.list_request_content_type()
                    && request.typ == self.config.profile.typ_list_request
                    && request.alg == "EdDSA"
                    && request.kid == self.holder_jkt
                    && self
                        .holder_key
                        .is_some_and(|key| request.verify_eddsa(&key))
            })
            .map(|request| request.claims)
            .filter(|claims| {
                let text = |name: &str| claims.get(name).and_then(Value::as_str);
                text("iss") == Some(self.holder_id.as_str())
                    && text("sub") == Some(self.holder_id.as_str())
                    && text("aud") == Some(issuer.as_str())
                    && text("htu") == Some(format!("{issuer}{ROUTE_LIST}").as_str())
            });
        let Some(claims) = verified else {
            return self.problem(None, 401, "invalid_holder_proof");
        };
        // 4. Rollback witness.
        if let Some(reference) = claims.get("last_list_ref")
            && Self::well_formed_ref(reference)
            && self.tag_valid(reference)
        {
            let epoch = reference["rev"]["epoch"].as_str().unwrap_or_default();
            let seq = reference["rev"]["seq"].as_u64().unwrap_or_default();
            if (epoch == self.epoch && seq > self.seq) || !self.lineage.iter().any(|e| e == epoch) {
                self.rollback_suspected = true;
                self.audit.push("restore_detected");
                return self.problem(Some(claims), 503, "authority_epoch_mismatch");
            }
        }
        // 5. Time.
        let now_ms = self.now_ms();
        let now_s = now_ms.div_euclid(1000);
        if let (Some(iat), Some(exp)) = (
            claims.get("iat").and_then(Value::as_i64),
            claims.get("exp").and_then(Value::as_i64),
        ) && (now_s > exp + SKEW_S
            || (now_s - iat).abs() > SKEW_S
            || !(60..=300).contains(&(exp - iat)))
        {
            return self.problem(Some(claims), 401, "proof_expired");
        }
        // 6. Replay.
        if let Some(jti) = claims.get("jti").and_then(Value::as_str)
            && !self.seen_jti.insert(jti.to_owned())
        {
            return self.problem(Some(claims), 401, "replay");
        }
        // 7. Credential scope.
        if self.disabled && !self.has_open_obligations() {
            return self.problem(Some(claims), 403, "holder_not_authorized");
        }
        // 8. Rate: list starts.
        // Measured on the authority's monotonic clock, not its wall clock.
        if claims.get("cursor").is_some_and(Value::is_null) {
            let exempt = std::mem::take(&mut self.exempt_next_start);
            let mono = self.clock.mono_ms();
            if !exempt
                && self
                    .last_list_start_mono
                    .is_some_and(|last| mono - last < 5_000)
            {
                return self.problem(Some(claims), 429, "rate_limited");
            }
            self.last_list_start_mono = Some(mono);
        }
        // 9. Grammar.
        if !Self::list_grammar(&claims) {
            return self.problem(Some(claims), 400, "invalid_request");
        }
        // 10. Route: an unverifiable witness is ignored; the cursor decides.
        if let Some(reference) = claims.get("last_list_ref")
            && !self.tag_valid(reference)
        {
            self.audit.push("witness_unverifiable");
        }
        let nonce = claims["nonce"].as_str().unwrap_or_default().to_owned();
        let page_size = usize::try_from(claims["page_size"].as_u64().unwrap_or(1)).unwrap_or(1);
        let (list_id, page0_iat_s, offset, first_ms) = match claims["cursor"].as_str() {
            None => (self.next_id("ls_"), now_s, 0, now_ms),
            Some(cursor) => {
                let sealed = self.unseal(cursor).filter(|c| {
                    c["holder_id"] == self.holder_id.as_str()
                        && c["nonce"] == nonce.as_str()
                        && c["holder_rev"] == self.holder_rev
                        && now_ms - c["issued_ms"].as_i64().unwrap_or(i64::MIN) <= 300_000
                        && now_ms - c["first_ms"].as_i64().unwrap_or(i64::MIN) <= 1_800_000
                });
                let Some(sealed) = sealed else {
                    return self.problem(Some(claims), 410, "list_expired");
                };
                (
                    sealed["list_id"].as_str().unwrap_or_default().to_owned(),
                    sealed["page0_iat"].as_i64().unwrap_or_default(),
                    usize::try_from(sealed["offset"].as_u64().unwrap_or(0)).unwrap_or(0),
                    sealed["first_ms"].as_i64().unwrap_or_default(),
                )
            }
        };
        let response = self.page(&nonce, &list_id, page0_iat_s, offset, first_ms, page_size);
        self.last_page = Some(response.clone());
        self.requests.push(RecordedRequest {
            claims: Some(claims),
            status: 200,
        });
        response
    }

    fn page(
        &mut self,
        nonce: &str,
        list_id: &str,
        page0_iat_s: i64,
        offset: usize,
        first_ms: i64,
        page_size: usize,
    ) -> HttpResponse {
        let now_ms = self.now_ms();
        let now_s = now_ms.div_euclid(1000);
        let projection = self.project(page0_iat_s);
        let total = projection.len();
        let count = total.div_ceil(page_size).max(1);
        let rev = json!({ "epoch": self.epoch, "seq": self.seq });
        let digest = self.state_digest(list_id, nonce, &rev, &projection);
        let tag = self.list_tag(list_id, &rev, nonce, &digest);
        let end = (offset + page_size).min(total);
        let next_cursor = (end < total).then(|| {
            self.seal(&json!({
                "holder_id": self.holder_id,
                "list_id": list_id,
                "nonce": nonce,
                "holder_rev": self.holder_rev,
                "issued_ms": now_ms,
                "first_ms": first_ms,
                "offset": end,
                "page0_iat": page0_iat_s,
            }))
        });
        let items: Vec<Value> = projection[offset.min(total)..end]
            .iter()
            .map(|item| {
                let mut item = item.as_object().cloned().unwrap_or_default();
                if let Some(hook) = &self.hooks.item {
                    hook(&mut item);
                }
                Value::Object(item)
            })
            .collect();
        let jti = self.next_id("lp_");
        let mut claims = json!({
            "iss": self.config.issuer,
            "aud": self.config.profile.holder_audience(&self.holder_id),
            "sub": self.holder_id,
            "iat": now_s,
            "exp": now_s + 300,
            "jti": jti,
            "list_id": list_id,
            "nonce": nonce,
            "rev": rev,
            "holder": {
                "holder_id": self.holder_id,
                "kind": self.config.holder_kind,
                "lease": {
                    "serve_seconds": self.config.serve_seconds,
                    "relist_seconds": self.config.relist_seconds,
                },
            },
            "page": { "index": offset / page_size, "count": count, "items_total": total },
            "state_digest": digest,
            "list_tag": tag,
            "items": items,
            "next_cursor": next_cursor,
        });
        let page = claims.as_object_mut().expect("object");
        if offset == 0 && !self.instructions.is_empty() {
            page.insert(
                "instructions".into(),
                Value::Array(self.instructions.clone()),
            );
        }
        if offset == 0
            && self.attach_keyset
            && let Some(statement) = &self.keyset_statement
        {
            page.insert("keyset".into(), statement.clone().into());
        }
        if let Some(hook) = &self.hooks.page {
            hook(page);
        }
        let typ = self
            .hooks
            .typ
            .clone()
            .unwrap_or_else(|| self.config.profile.typ_list_page.clone());
        let alg = self.hooks.alg.clone().unwrap_or_else(|| "ES256".into());
        let kid = self
            .hooks
            .kid
            .clone()
            .unwrap_or_else(|| self.active_kid.clone());
        let key = self
            .hooks
            .foreign_key
            .clone()
            .unwrap_or_else(|| self.keys[&self.active_kid].clone());
        let compact = jws::sign_with_header(&alg, &typ, &kid, &claims, &key);
        HttpResponse {
            status: 200,
            content_type: Some(
                self.hooks
                    .content_type
                    .clone()
                    .unwrap_or_else(|| self.config.profile.list_page_content_type()),
            ),
            retry_after_s: None,
            body: compact.into_bytes(),
        }
    }
}

impl Transport for FakeAuthority {
    fn post(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        _timeouts: Timeouts,
    ) -> Result<HttpResponse, TransportError> {
        let mut state = self.state();
        state.posts += 1;
        state.bodies.push(body.to_vec());
        let posts = state.posts;
        let due: Vec<Mutation> = {
            let (now, later): (Vec<_>, Vec<_>) = std::mem::take(&mut state.scheduled)
                .into_iter()
                .partition(|(n, _)| *n == posts);
            state.scheduled = later;
            now.into_iter().map(|(_, m)| m).collect()
        };
        for mutation in due {
            mutation(&mut state);
        }
        let half = state.latency_ms / 2;
        let rest = state.latency_ms - half;
        state.clock.advance(half);
        let response = match state.forced.pop_front() {
            Some(Forced::Unreachable) => Err(TransportError::Unreachable),
            Some(Forced::Timeout) => Err(TransportError::Timeout),
            Some(Forced::Response(response)) => Ok(response),
            None if url == format!("{}{ROUTE_LIST}", state.config.issuer) => {
                Ok(state.handle_list(content_type, body))
            }
            None => Ok(state.problem(None, 404, "not_found")),
        };
        state.clock.advance(rest);
        response
    }

    fn get(&self, url: &str, _timeouts: Timeouts) -> Result<HttpResponse, TransportError> {
        let state = self.state();
        match &state.keyset_statement {
            Some(statement) if url == format!("{}{ROUTE_KEYSET}", state.config.issuer) => {
                Ok(HttpResponse {
                    status: 200,
                    content_type: Some(state.config.profile.keyset_content_type()),
                    retry_after_s: None,
                    body: statement.clone().into_bytes(),
                })
            }
            _ => Ok(HttpResponse {
                status: 404,
                ..HttpResponse::default()
            }),
        }
    }
}
