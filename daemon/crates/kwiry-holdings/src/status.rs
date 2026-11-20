//! Holder status reports (v2 Part I §11.3, §11.4 G.1–G.3, H.4, H.12, H.13,
//! H.19, H.22, H.27, H.50, H.53). Kwiry's rows: Annex K.1 T1–T7, T6c, N5.
//!
//! **What a status says.** The holder's whole unattributed count and the
//! digest of its manifest, read from the purger's independent census
//! against the list applied in this process, `holder_state` (`reset` until
//! a current-epoch receipt clears the trust-on-first-use flag), and at most
//! one release result (`release`). Releases bind to the exact compact JWS
//! of an accepted report (`bound_report_digest`, G.3), so every attempt's
//! digest is indexed to the manifest its body names before it can reach
//! the wire.
//!
//! **Triggers (T1).** A reset to report, a release outcome to report, an
//! epoch acceptance not yet answered by a current-epoch status (H.19, even
//! at zero), a count or manifest that differs from the last accepted
//! status, or a non-zero count last accepted a day ago.
//!
//! **Transmission (T2, T2d).** The body is fixed for its key; retries reuse
//! the key and are exempt from the rate limit. A first transmission of a
//! new key waits until 60 s after the previous new key (H.4.4), and while a
//! possibly-sent earlier status could still commit after it and supersede
//! it (H.50 for status: an unresolved attempt citing a list at or after
//! this body's list in the same epoch). The exact bytes and the attempt are
//! durable before the first byte; at the first byte the census, the route
//! and fail-stop are checked again under the publication freeze.
//!
//! **Answers.** A verified ack for one of the holder's own attempts is a
//! receipt; a receipt minted in the current epoch opens the status-first
//! gate (H.27), clears `reset_pending` and finalizes the carried release
//! result (L7, L8) in the same commit. An ack minted in another epoch is
//! audit only. Bug classes halt the channel and release the gate (T6c,
//! H.22.3); `409 stale_list_ref` is a definitive refusal and a relist
//! (T6d); `401` and `403` are auth halts; everything else is treated as
//! unreachable. There is no convergence path: a status ack under another
//! key matches nothing.

use crate::canonical::{is_sha256_tagged, labelled_digest, sha256_tagged};
use crate::client::{HttpResponse, LIST_TIMEOUTS, Transport, TransportError, problem_code};
use crate::events::HolderEvent;
use crate::holder::{AuthHalt, Holder, RouteHealth};
use crate::obligations::{
    Attempt, Envelope, ObligationBook, PendingStatus, manifest_bytes, manifest_digest,
};
use crate::profile::ROUTE_STATUS;
use crate::purge::{Inventory, ObligationsRefused};
use crate::report::{ABSENCE_BOUND_S, Answer, QUEUE_CAP_MS, UNVERIFIABLE_HALT, sent_file};
use crate::store::{ArtifactPurger, ResourceStore};
use crate::wire::{ListRef, Rev};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Domain label of status idempotency keys.
pub const STATUS_KEY_LABEL: &str = "holdings-status-key/v1";
/// At most one new status key per this window (v2 H.4.4).
pub const STATUS_NEW_KEY_SPACING_MS: i64 = 60_000;
/// A non-zero count is re-reported at least this often (T1).
pub const STATUS_DAILY_MS: i64 = 86_400_000;
/// Manifests of accepted reports are kept this long, and for as long as a
/// release instruction binds them (design §5.5).
pub const MANIFEST_RETENTION_MS: i64 = 30 * 86_400_000;
const BACKOFF_START_MS: u64 = 10_000;
const BACKOFF_MAX_MS: u64 = 300_000;

/// `holder_state` (v2 §11.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderStateClaim {
    Ok,
    Reset,
}

impl HolderStateClaim {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Reset => "reset",
        }
    }

    pub fn from_wire(text: &str) -> Option<Self> {
        match text {
            "ok" => Some(Self::Ok),
            "reset" => Some(Self::Reset),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseOutcome {
    Executed,
    Mismatch,
}

impl ReleaseOutcome {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::Mismatch => "mismatch",
        }
    }
}

/// `release_result` (v2 §11.4 G.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseResult {
    pub instruction_id: String,
    pub result: ReleaseOutcome,
    pub observed_manifest_digest: String,
}

/// The business claims of one status report, fixed for the life of its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusBody {
    pub list_ref: ListRef,
    pub quarantined_unattributed: u64,
    pub quarantine_manifest_digest: String,
    pub holder_state: HolderStateClaim,
    pub release_result: Option<ReleaseResult>,
}

impl StatusBody {
    /// The claim set the key is derived from: the envelope and every
    /// business claim.
    pub fn business_claims(&self, envelope: &Envelope) -> Map<String, Value> {
        let mut claims = Map::new();
        claims.insert("iss".into(), envelope.holder_id.into());
        claims.insert("sub".into(), envelope.holder_id.into());
        claims.insert("aud".into(), envelope.audience.into());
        claims.insert("htu".into(), envelope.htu.into());
        claims.insert("list_ref".into(), self.list_ref.to_value());
        claims.insert(
            "quarantined_unattributed".into(),
            self.quarantined_unattributed.into(),
        );
        claims.insert(
            "quarantine_manifest_digest".into(),
            self.quarantine_manifest_digest.clone().into(),
        );
        claims.insert("holder_state".into(), self.holder_state.wire().into());
        if let Some(result) = &self.release_result {
            claims.insert(
                "release_result".into(),
                json!({
                    "instruction_id": result.instruction_id,
                    "result": result.result.wire(),
                    "observed_manifest_digest": result.observed_manifest_digest,
                }),
            );
        }
        claims
    }

    /// `ik_` and 43 base64url characters, from the claims and the
    /// derivation count.
    pub fn idempotency_key(&self, envelope: &Envelope, derivation: u64) -> String {
        let input = json!({
            "b": Value::Object(self.business_claims(envelope)),
            "n": derivation,
        });
        let digest = labelled_digest(STATUS_KEY_LABEL, &input);
        format!("ik_{}", &crate::b64::encode(&digest)[..43])
    }

    pub fn request_claims(&self, envelope: &Envelope, key: &str, jti: &str, iat_s: i64) -> Value {
        let mut claims = self.business_claims(envelope);
        claims.insert("iat".into(), iat_s.into());
        claims.insert(
            "exp".into(),
            (iat_s + crate::wire::REQUEST_LIFETIME_SECONDS).into(),
        );
        claims.insert("jti".into(), jti.into());
        claims.insert("idempotency_key".into(), key.into());
        Value::Object(claims)
    }

    /// What the T2 guard compares: everything but the list reference.
    fn claim(&self) -> (u64, &str, HolderStateClaim, Option<&ReleaseResult>) {
        (
            self.quarantined_unattributed,
            &self.quarantine_manifest_digest,
            self.holder_state,
            self.release_result.as_ref(),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusPending {
    pub key: String,
    pub body: StatusBody,
    pub status: PendingStatus,
    pub attempts: Vec<Attempt>,
    /// Epoch of the list the body cites.
    pub epoch: String,
}

impl StatusPending {
    pub fn live(&self) -> bool {
        matches!(self.status, PendingStatus::Ready | PendingStatus::Armed)
    }
}

/// A superseded status whose attempts may have been accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusUncertain {
    /// Its delayed receipt resolves the attempt, not the N5 re-derivation.
    #[serde(default)]
    pub before_resume: bool,
    pub body: StatusBody,
    pub epoch: String,
    pub attempts: Vec<Attempt>,
}

impl StatusUncertain {
    pub fn latest_exp_s(&self) -> i64 {
        self.attempts.iter().map(|a| a.exp_s).max().unwrap_or(0)
    }
}

/// The last status the authority accepted, from a verified receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedStatus {
    pub key: String,
    pub report_digest: String,
    pub quarantined_unattributed: u64,
    pub quarantine_manifest_digest: String,
    pub rev: Rev,
    pub accepted_ms: i64,
}

/// Where one release instruction stands (Annex K.1 M7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStep {
    /// The exact units are recorded; the purger has not yet confirmed the
    /// reservation in any process (L1, before its `reserve`).
    Reserving,
    /// Reserved and detached; units are deleted as they become free (L4,
    /// L5).
    Draining,
    /// Every reserved unit is gone; `executed` is owed (L6).
    Deleted,
    /// A status carrying `executed` was accepted (L7).
    Done,
    /// The strict match failed; nothing was reserved or deleted (L2).
    Mismatch,
    /// A status carrying `mismatch` was accepted (L8).
    MismatchReported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionRecord {
    pub bound_report_digest: String,
    pub approved_count: u64,
    pub step: ReleaseStep,
    /// The exact units the manifest named (empty on a mismatch).
    pub units: crate::store::ManifestUnits,
    /// Units claimed for the deletion in progress (written before deleting).
    pub claimed: BTreeSet<String>,
    /// Units deleted under this release, across passes.
    pub deleted: BTreeSet<String>,
    /// The holder's manifest digest when the outcome was decided.
    pub observed_manifest_digest: Option<String>,
    /// Epoch of the accepted status that carried the result.
    pub carried_epoch: Option<String>,
    /// Evaluation order; results are reported oldest first.
    pub order: u64,
}

impl InstructionRecord {
    /// Units the holder holds for this release: never attributed to an
    /// obligation, never claimed by one, counted unattributed.
    pub fn holds_units(&self) -> bool {
        matches!(self.step, ReleaseStep::Reserving | ReleaseStep::Draining)
    }
}

/// Which manifest an attempt's exact bytes named (G.3: `bound_report_digest`
/// is `sha256:` over those bytes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestRef {
    pub key: String,
    pub manifest_digest: String,
    pub recorded_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusBook {
    pub pending: Option<StatusPending>,
    /// Bug halt (T6c). Cleared only by an operator resume (N5).
    pub halted: bool,
    /// Counts re-derivations, so each one gets a new key.
    pub derivations: u64,
    /// Durable upper bound on the first possible dispatch of the newest key (H.4.4).
    pub last_new_key_ms: Option<i64>,
    pub last_accepted: Option<AcceptedStatus>,
    /// A superseded attempt may already have updated the authority. D only
    /// bounds *future* acceptance; acknowledge a replacement even when the
    /// current census equals the last locally acknowledged one.
    #[serde(default)]
    pub correction_owed: bool,
    /// Superseded attempts whose acceptance is unknown, by key.
    pub uncertain: BTreeMap<String, StatusUncertain>,
    /// Release instructions seen, by instruction id.
    pub instructions: BTreeMap<String, InstructionRecord>,
    /// Attempt digest (`sha256:`) → the manifest its body named.
    pub manifests: BTreeMap<String, ManifestRef>,
    /// Creation time of T1 files, including those with no transmitted attempt.
    #[serde(default)]
    pub manifest_files: BTreeMap<String, i64>,
}

impl StatusBook {
    /// Every unit a release currently holds.
    pub fn release_held_units(&self) -> BTreeSet<String> {
        self.instructions
            .values()
            .filter(|r| r.holds_units())
            .flat_map(|r| r.units.keys().cloned())
            .collect()
    }

    pub fn reservation_pending(&self) -> bool {
        self.instructions
            .values()
            .any(|r| r.step == ReleaseStep::Reserving)
    }
}

/// `release_result_disposition` (v2 H.4.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultDisposition {
    Applied,
    UnknownInstruction,
    AlreadyFinal,
}

impl ResultDisposition {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::UnknownInstruction => "unknown_instruction",
            Self::AlreadyFinal => "already_final",
        }
    }
}

/// A verified status ack (v2 §11.4 G.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusAck {
    pub compact: String,
    pub jti: String,
    pub idempotency_key: String,
    pub report_jti: String,
    pub report_digest: String,
    pub accepted_quarantined_unattributed: u64,
    pub accepted_manifest_digest: String,
    pub accepted_holder_state: HolderStateClaim,
    pub rev: Rev,
    pub disposition: Option<ResultDisposition>,
}

const STATUS_ACK_MEMBERS: [&str; 12] = [
    "iss",
    "aud",
    "sub",
    "iat",
    "jti",
    "idempotency_key",
    "report_jti",
    "report_digest",
    "accepted_quarantined_unattributed",
    "accepted_manifest_digest",
    "accepted_holder_state",
    "rev",
];
const DISPOSITION_MEMBER: &str = "release_result_disposition";

/// What one status pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusSummary {
    pub derived: usize,
    pub sent: usize,
    pub receipts: usize,
    pub rejected: usize,
    pub invalidated: usize,
    pub halted: usize,
    /// Attempts cancelled at their first byte (never transmitted).
    pub cancelled: usize,
    /// A status waits: the 60 s new-key spacing, an uncertain earlier
    /// status, a back-off or the queue.
    pub waiting: usize,
}

impl StatusSummary {
    fn record(&mut self, answer: Answer) {
        match answer {
            Answer::Receipt => self.receipts += 1,
            Answer::Rejected => self.rejected += 1,
            Answer::Halted => self.halted += 1,
            _ => {}
        }
    }
}

/// Manifest digests and other opaque holder values on the wire.
fn opaque_digest(text: &str) -> bool {
    !text.is_empty() && text.len() <= 128 && text.bytes().all(|b| b.is_ascii_graphic())
}

fn text<'a>(claims: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    claims.get(name).and_then(Value::as_str)
}

fn token(bytes: usize) -> Option<String> {
    crate::b64::random_token(bytes).ok()
}

enum StatusVerificationFailure {
    Unverifiable,
    PinWrite,
}

pub(crate) enum StatusRefusal {
    Halt(u16),
    Stale(String),
}

struct StatusArmed {
    compact: String,
    attempt: Attempt,
    armed_mono: u64,
}

/// v2 §C.0 *instruction already-done handling* and H.14: the result a
/// release instruction owes a status report, if any. A result not yet
/// carried by an accepted status is owed; a carried one is owed again only
/// when the instruction is still listed in an epoch other than the one
/// whose status carried it (a re-listing after a restore replay).
pub fn owed_outcome(
    record: &InstructionRecord,
    current_epoch: &str,
    listed: bool,
) -> Option<ReleaseOutcome> {
    match record.step {
        ReleaseStep::Deleted => Some(ReleaseOutcome::Executed),
        ReleaseStep::Mismatch => Some(ReleaseOutcome::Mismatch),
        ReleaseStep::Done | ReleaseStep::MismatchReported => {
            let carried_here = record.carried_epoch.as_deref() == Some(current_epoch);
            (listed && !carried_here).then_some(if record.step == ReleaseStep::Done {
                ReleaseOutcome::Executed
            } else {
                ReleaseOutcome::Mismatch
            })
        }
        ReleaseStep::Reserving | ReleaseStep::Draining => None,
    }
}

impl<S: ResourceStore, T: Transport> Holder<S, T> {
    fn status_envelope(&self) -> (String, String, String) {
        let enrollment = self.writer.enrollment();
        (
            enrollment.holder_id.clone(),
            enrollment.issuer.clone(),
            format!("{}{ROUTE_STATUS}", enrollment.issuer),
        )
    }

    /// Commits the status book; on failure emits the storage alert and
    /// returns false (the step fails with no state change).
    pub(crate) fn commit_status(&mut self, status: StatusBook) -> bool {
        self.commit_lifecycle(None, Some(status), false)
    }

    /// Commits obligation records and the status book together.
    pub(crate) fn commit_lifecycle(
        &mut self,
        book: Option<ObligationBook>,
        status: Option<StatusBook>,
        clear_reset: bool,
    ) -> bool {
        match self.writer.update_lifecycle(book, status, clear_reset) {
            Ok(()) => true,
            Err(error) => {
                crate::holder::report_write_failure(self.events.as_ref(), &error);
                false
            }
        }
    }

    /// Verifies a status ack: closed header and members, exact `typ`, ES256
    /// under a live pinned key, exact `iss`, `aud` and `sub`, the grammar of
    /// every member and no `exp`.
    pub fn verify_status_ack(&self, compact: &str) -> Result<StatusAck, &'static str> {
        let profile = &self.config.profile;
        let enrollment = self.writer.enrollment();
        let parsed = crate::jws::parse(compact).map_err(|_| "jws")?;
        if parsed.typ != profile.typ_status_ack {
            return Err("typ");
        }
        if parsed.alg != "ES256" {
            return Err("alg");
        }
        let key = enrollment
            .pinned
            .live(
                &parsed.kid,
                &profile.authority_key_id_prefix,
                self.clock.wall_ms(),
            )
            .ok_or("kid")?;
        if !parsed.verify_es256(&key) {
            return Err("signature");
        }
        let claims = &parsed.claims;
        let has_disposition = claims.contains_key(DISPOSITION_MEMBER);
        let expected = STATUS_ACK_MEMBERS.len() + usize::from(has_disposition);
        if claims.len() != expected
            || claims
                .keys()
                .any(|k| !STATUS_ACK_MEMBERS.contains(&k.as_str()) && k != DISPOSITION_MEMBER)
        {
            return Err("members");
        }
        if text(claims, "iss") != Some(enrollment.issuer.as_str()) {
            return Err("iss");
        }
        if text(claims, "aud") != Some(profile.holder_audience(&enrollment.holder_id).as_str()) {
            return Err("aud");
        }
        if text(claims, "sub") != Some(enrollment.holder_id.as_str()) {
            return Err("sub");
        }
        claims.get("iat").and_then(Value::as_i64).ok_or("iat")?;
        let checked = |name: &'static str, valid: fn(&str) -> bool| {
            text(claims, name)
                .filter(|t| valid(t))
                .map(str::to_owned)
                .ok_or(name)
        };
        let jti = checked("jti", crate::grammar::jti)?;
        let idempotency_key = checked("idempotency_key", crate::grammar::idempotency_key)?;
        let report_jti = checked("report_jti", crate::grammar::jti)?;
        let report_digest = checked("report_digest", is_sha256_tagged)?;
        let accepted_manifest_digest = checked("accepted_manifest_digest", opaque_digest)?;
        let accepted_quarantined_unattributed = claims
            .get("accepted_quarantined_unattributed")
            .and_then(Value::as_u64)
            .ok_or("accepted_quarantined_unattributed")?;
        let accepted_holder_state = text(claims, "accepted_holder_state")
            .and_then(HolderStateClaim::from_wire)
            .ok_or("accepted_holder_state")?;
        let rev = crate::verify::parse_rev(claims.get("rev").ok_or("rev")?).map_err(|_| "rev")?;
        let disposition = match claims.get(DISPOSITION_MEMBER) {
            None => None,
            Some(value) => Some(match value.as_str() {
                Some("applied") => ResultDisposition::Applied,
                Some("unknown_instruction") => ResultDisposition::UnknownInstruction,
                Some("already_final") => ResultDisposition::AlreadyFinal,
                _ => return Err(DISPOSITION_MEMBER),
            }),
        };
        Ok(StatusAck {
            compact: compact.to_owned(),
            jti,
            idempotency_key,
            report_jti,
            report_digest,
            accepted_quarantined_unattributed,
            accepted_manifest_digest,
            accepted_holder_state,
            rev,
            disposition,
        })
    }

    /// One pass of the status channel: invalidate a stale status, derive a
    /// new one if anything triggers it, transmit what is due and classify
    /// the answer.
    pub fn status_once(
        &mut self,
        purger: &dyn ArtifactPurger,
    ) -> Result<StatusSummary, ObligationsRefused> {
        let mut summary = StatusSummary::default();
        if self.traffic_halted() {
            return Ok(summary);
        }
        if self.fail_stopped() {
            return Err(ObligationsRefused::HolderStateUnwritable);
        }
        if let Some(owed) = self.status_refusal.take() {
            summary.record(self.finish_refusal(owed));
            return Ok(summary);
        }
        if let Some((key, response)) = self.transport.take_late_status_response() {
            // A route refusal narrows even if a newer list failed to apply. An
            // affirmative answer must not refresh pins or broaden in that state.
            if response.status == 200
                && (self.obligations_allowed().is_err() || self.apply_failed_since_admit)
            {
                return Ok(summary);
            }
            let answer = self.classify_status(&key, Ok(response));
            summary.record(answer);
            if answer == Answer::BackOff && self.clock.mono_ms() >= self.status_backoff_until {
                self.status_back_off(None);
            }
            return Ok(summary);
        }
        self.obligations_allowed()?;
        if self.apply_failed_since_admit {
            return Ok(summary);
        }
        let Ok(inventory) = self.inventory(purger) else {
            self.events
                .emit(HolderEvent::PurgeStepFailed { step: "census" });
            return Ok(summary);
        };
        self.status_invalidate_failing(&inventory, &mut summary);
        self.obligations_allowed()?;
        if self.status_derive_and_store(&inventory) {
            summary.derived += 1;
        }
        self.obligations_allowed()?;
        self.status_transmit(purger, &mut summary);
        self.prune_manifest_index();
        self.emit_reserve_uses();
        Ok(summary)
    }

    /// The body the current list and census imply.
    fn derive_status(&self, inventory: &Inventory) -> Option<StatusBody> {
        let applied = self.applied.as_ref()?;
        let holder_id = &self.writer.enrollment().holder_id;
        Some(StatusBody {
            list_ref: applied.list_ref.clone(),
            quarantined_unattributed: inventory.unattributed_count(),
            quarantine_manifest_digest: manifest_digest(holder_id, &inventory.unattributed),
            holder_state: if self.writer.state().reset_pending {
                HolderStateClaim::Reset
            } else {
                HolderStateClaim::Ok
            },
            release_result: self.owed_release_result(),
        })
    }

    /// The oldest release result owed (one per status).
    pub(crate) fn owed_release_result(&self) -> Option<ReleaseResult> {
        let epoch = self.high_water_epoch()?;
        let listed: BTreeSet<&str> = self
            .applied
            .as_ref()
            .map(|a| {
                a.instructions
                    .iter()
                    .map(|i| i.instruction_id.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let status = &self.writer.state().status;
        status
            .instructions
            .iter()
            .filter_map(|(id, record)| {
                owed_outcome(record, &epoch, listed.contains(id.as_str())).map(|outcome| {
                    (
                        record.order,
                        ReleaseResult {
                            instruction_id: id.clone(),
                            result: outcome,
                            observed_manifest_digest: record
                                .observed_manifest_digest
                                .clone()
                                .unwrap_or_default(),
                        },
                    )
                })
            })
            .min_by_key(|(order, _)| *order)
            .map(|(_, result)| result)
    }

    /// T1's triggers.
    fn status_triggered(&self, body: &StatusBody, epoch: &str) -> bool {
        let state = self.writer.state();
        let gate = &state.obligations.status_gate;
        let epoch_owed = gate.armed_epoch.as_deref() == Some(epoch)
            && gate.acked_epoch.as_deref() != Some(epoch);
        if body.holder_state == HolderStateClaim::Reset
            || body.release_result.is_some()
            || epoch_owed
            || state.status.correction_owed
        {
            return true;
        }
        match &state.status.last_accepted {
            None => body.quarantined_unattributed > 0,
            Some(accepted) => {
                accepted.rev.epoch != epoch
                    || accepted.quarantined_unattributed != body.quarantined_unattributed
                    || accepted.quarantine_manifest_digest != body.quarantine_manifest_digest
                    || (body.quarantined_unattributed > 0
                        && self.clock.wall_ms() - accepted.accepted_ms >= STATUS_DAILY_MS)
            }
        }
    }

    /// T2's guard: re-derive the claim from the current list and census.
    fn status_guard(&self, pending: &StatusPending, inventory: &Inventory) -> bool {
        self.high_water_epoch().as_deref() == Some(pending.epoch.as_str())
            && self
                .derive_status(inventory)
                .is_some_and(|current| current.claim() == pending.body.claim())
    }

    /// T3: a live status whose claim no longer holds is invalidated.
    fn status_invalidate_failing(&mut self, inventory: &Inventory, summary: &mut StatusSummary) {
        let mut status = self.writer.state().status.clone();
        let Some(pending) = status.pending.clone().filter(StatusPending::live) else {
            return;
        };
        // A status never transmitted cites the newest applied list, so it
        // can follow an uncertain earlier one past the H.4.2 floor.
        let newer_list = pending.attempts.is_empty()
            && self
                .applied
                .as_ref()
                .is_some_and(|a| a.list_ref != pending.body.list_ref);
        if self.status_guard(&pending, inventory) && !newer_list {
            return;
        }
        invalidate_status(&mut status);
        if self.commit_status(status) {
            summary.invalidated += 1;
        }
    }

    /// T1 with its guards. Returns whether a new status was stored.
    fn status_derive_and_store(&mut self, inventory: &Inventory) -> bool {
        if self.relist_before_report {
            return false;
        }
        let Some(epoch) = self.high_water_epoch() else {
            return false;
        };
        let status = self.writer.state().status.clone();
        if status.halted || status.pending.as_ref().is_some_and(StatusPending::live) {
            return false;
        }
        let Some(body) = self.derive_status(inventory) else {
            return false;
        };
        if !self.status_triggered(&body, &epoch) {
            return false;
        }
        let (holder_id, audience, htu) = self.status_envelope();
        let envelope = Envelope {
            holder_id: &holder_id,
            audience: &audience,
            htu: &htu,
        };
        let derivation = status.derivations + 1;
        let key = body.idempotency_key(&envelope, derivation);
        // T1: the manifest the body names is durable before the record.
        if self
            .writer
            .write_side_file(
                &format!("manifest.{key}.json"),
                &manifest_bytes(&holder_id, &inventory.unattributed),
            )
            .is_err()
        {
            self.events.emit(HolderEvent::StorageExhausted);
            return false;
        }
        let mut next = status;
        let mut answered = Vec::new();
        if let Some(previous) = next.pending.take() {
            match previous.status {
                PendingStatus::Rejected => answered = previous.attempts,
                _ if !previous.attempts.is_empty() => {
                    next.correction_owed = true;
                    next.uncertain.insert(
                        previous.key.clone(),
                        StatusUncertain {
                            before_resume: false,
                            body: previous.body,
                            epoch: previous.epoch,
                            attempts: previous.attempts,
                        },
                    );
                }
                _ => {}
            }
        }
        next.manifest_files
            .insert(key.clone(), self.clock.wall_ms());
        next.derivations = derivation;
        next.pending = Some(StatusPending {
            key,
            body,
            status: PendingStatus::Ready,
            attempts: Vec::new(),
            epoch,
        });
        let stored = self.commit_status(next);
        if stored {
            self.prune_sent_attempts(&answered);
        }
        stored
    }

    /// H.50 for status: a possibly-sent earlier status that could still
    /// commit after this one and supersede it. An attempt citing an
    /// earlier list of the same epoch cannot (the H.4.2 floor refuses it
    /// once this one is accepted); one citing another epoch never commits
    /// in this lineage (H.53.1).
    fn status_overtakes_uncertain(&self, pending: &StatusPending) -> bool {
        let seq = pending.body.list_ref.rev.seq;
        self.writer
            .state()
            .status
            .uncertain
            .values()
            .any(|u| u.epoch == pending.epoch && u.body.list_ref.rev.seq >= seq)
    }

    fn status_back_off(&mut self, retry_after_s: Option<u64>) {
        let now = self.clock.mono_ms();
        self.status_backoff_ms = self
            .status_backoff_ms
            .saturating_mul(2)
            .clamp(BACKOFF_START_MS, BACKOFF_MAX_MS);
        let wait = retry_after_s
            .map_or(0, |s| s.saturating_mul(1000))
            .max(self.status_backoff_ms);
        self.status_backoff_until = now.saturating_add(wait);
    }

    /// T2 → T2d → the answer.
    fn status_transmit(&mut self, purger: &dyn ArtifactPurger, summary: &mut StatusSummary) {
        let mut replay_retried = false;
        let mut expiry_retried = false;
        loop {
            if self.fail_stopped() || self.route != RouteHealth::Normal || self.relist_before_report
            {
                return;
            }
            let status = self.writer.state().status.clone();
            if status.halted {
                return;
            }
            let Some(pending) = status.pending.clone().filter(StatusPending::live) else {
                return;
            };
            let first = pending.attempts.is_empty();
            if first {
                // H.4.4: one new key per 60 s; retries are exempt.
                let spaced = self.clock.mono_ms() >= self.status_spacing_until;
                if !spaced || self.status_overtakes_uncertain(&pending) {
                    summary.waiting += 1;
                    return;
                }
            }
            if self.clock.mono_ms() < self.status_backoff_until {
                summary.waiting += 1;
                return;
            }
            let Ok(inventory) = self.inventory(purger) else {
                return;
            };
            if !self.status_guard(&pending, &inventory) {
                let mut next = status;
                invalidate_status(&mut next);
                if self.commit_status(next) {
                    summary.invalidated += 1;
                }
                return;
            }
            let previous_dispatch = status.last_new_key_ms;
            // After restart an arm is evidence of a *possible* send only.
            // The first actual dispatch observed here renews the crash bound.
            let first_dispatch = self.dispatched_key.as_ref() != Some(&pending.key);
            let Some(sent) = self.arm_status_attempt(&pending, first_dispatch) else {
                return;
            };
            let Some(pending) = self.writer.state().status.pending.clone() else {
                return;
            };
            // T2d: the linearization point, immediately before the first
            // byte, under the publication freeze.
            let url = format!("{}{ROUTE_STATUS}", self.writer.enrollment().issuer);
            let content_type = self.config.profile.status_request_content_type();
            let freeze = purger.freeze_publications().ok();
            let first_byte_ok = freeze.is_some()
                && !self.fail_stopped()
                && self.route == RouteHealth::Normal
                && self
                    .inventory(purger)
                    .is_ok_and(|now| self.status_guard(&pending, &now))
                && self.clock.mono_ms().saturating_sub(sent.armed_mono) < QUEUE_CAP_MS;
            if !first_byte_ok {
                drop(freeze);
                if self.cancel_status_attempt(&sent.attempt, previous_dispatch) {
                    summary.cancelled += 1;
                }
                return;
            }
            if first_dispatch {
                self.dispatched_key = Some(pending.key.clone());
                self.status_spacing_until = self
                    .clock
                    .mono_ms()
                    .saturating_add(STATUS_NEW_KEY_SPACING_MS as u64);
            }
            summary.sent += 1;
            let response =
                self.transport
                    .post(&url, &content_type, sent.compact.as_bytes(), LIST_TIMEOUTS);
            drop(freeze);
            let answer = self.classify_status(&pending.key, response);
            summary.record(answer);
            match answer {
                Answer::Receipt => {
                    self.status_backoff_ms = 0;
                    return;
                }
                Answer::Rejected
                | Answer::Halted
                | Answer::Converged
                | Answer::Unverifiable
                | Answer::AuthHalted => return,
                Answer::RetryReplay if !replay_retried => replay_retried = true,
                Answer::RetryExpired if !expiry_retried => expiry_retried = true,
                Answer::RetryExpired => {
                    self.events.emit(HolderEvent::ProofExpiredClockAlert);
                    self.status_back_off(None);
                    return;
                }
                Answer::RetryReplay | Answer::BackOff => {
                    // A `429` already backed off past its `Retry-After`.
                    if self.clock.mono_ms() >= self.status_backoff_until {
                        self.status_back_off(None);
                    }
                    return;
                }
            }
        }
    }

    /// T2: sign a fresh attempt, write its exact bytes, then record it with
    /// the manifest its digest names.
    fn arm_status_attempt(&mut self, pending: &StatusPending, first: bool) -> Option<StatusArmed> {
        let (holder_id, audience, htu) = self.status_envelope();
        let envelope = Envelope {
            holder_id: &holder_id,
            audience: &audience,
            htu: &htu,
        };
        let jti = token(16)?;
        let iat_s = self.clock.wall_ms().div_euclid(1000);
        let claims = pending
            .body
            .request_claims(&envelope, &pending.key, &jti, iat_s);
        let compact = crate::jws::sign_eddsa(
            &self.config.profile.typ_status_request,
            &self.key.thumbprint(),
            &claims,
            self.key.signing_key(),
        );
        let digest = sha256_tagged(compact.as_bytes());
        if self
            .writer
            .write_side_file(&sent_file(&digest), compact.as_bytes())
            .is_err()
        {
            self.events.emit(HolderEvent::StorageExhausted);
            return None;
        }
        let attempt = Attempt {
            jti,
            digest: digest.clone(),
            exp_s: iat_s + crate::wire::REQUEST_LIFETIME_SECONDS,
        };
        let mut next = self.writer.state().status.clone();
        next.manifests.insert(
            digest,
            ManifestRef {
                key: pending.key.clone(),
                manifest_digest: pending.body.quarantine_manifest_digest.clone(),
                recorded_ms: self.clock.wall_ms(),
            },
        );
        let armed_mono = self.clock.mono_ms();
        if first {
            // If dispatch crashes, the queue cap bounds the latest possible send.
            next.last_new_key_ms = Some(self.clock.wall_ms().saturating_add(QUEUE_CAP_MS as i64));
        }
        let record = next.pending.as_mut()?;
        record.attempts.push(attempt.clone());
        record.status = PendingStatus::Armed;
        if !self.commit_status(next) {
            return None;
        }
        Some(StatusArmed {
            compact,
            attempt,
            armed_mono,
        })
    }

    /// T2q → T2x: the attempt never left; prune it and invalidate.
    fn cancel_status_attempt(&mut self, attempt: &Attempt, previous_dispatch: Option<i64>) -> bool {
        let mut next = self.writer.state().status.clone();
        next.last_new_key_ms = previous_dispatch;
        next.manifests.remove(&attempt.digest);
        if let Some(pending) = next.pending.as_mut() {
            pending.attempts.retain(|a| a != attempt);
        }
        invalidate_status(&mut next);
        if self.commit_status(next) {
            self.prune_sent_attempts(std::slice::from_ref(attempt));
            true
        } else {
            false
        }
    }

    pub(crate) fn prune_sent_attempts(&mut self, attempts: &[Attempt]) {
        for attempt in attempts {
            let _ = self.writer.remove_side_file(&sent_file(&attempt.digest));
        }
    }

    pub(crate) fn classify_status(
        &mut self,
        request_key: &str,
        response: Result<HttpResponse, TransportError>,
    ) -> Answer {
        let response = match response {
            Ok(response) => response,
            // T5: sent, outcome unknown; the attempt stays recorded.
            Err(_) => return Answer::BackOff,
        };
        let code = problem_code(&response);
        match (response.status, code.as_deref()) {
            (200, _) => {
                if response.content_type.as_deref()
                    != Some(self.config.profile.status_ack_content_type().as_str())
                {
                    return self.status_unverifiable();
                }
                let Ok(compact) = String::from_utf8(response.body) else {
                    return self.status_unverifiable();
                };
                match self.verify_status_with_refresh(&compact) {
                    Ok(ack) if ack.idempotency_key == request_key => {
                        let pending = self
                            .writer
                            .state()
                            .status
                            .pending
                            .clone()
                            .filter(StatusPending::live);
                        self.on_status_ack(pending.as_ref(), ack)
                    }
                    Ok(_) => self.status_unverifiable(),
                    Err(StatusVerificationFailure::PinWrite) => Answer::BackOff,
                    Err(_) => self.status_unverifiable(),
                }
            }
            (409, Some("stale_list_ref")) => {
                self.relist_before_report = true;
                self.finish_refusal(StatusRefusal::Stale(request_key.into()))
            }
            (409, Some("idempotency_conflict")) | (400 | 413, _) => {
                self.finish_refusal(StatusRefusal::Halt(response.status))
            }
            (401, Some("replay")) => Answer::RetryReplay,
            (401, Some("proof_expired")) => Answer::RetryExpired,
            (401, _) => {
                self.route = RouteHealth::AuthHalt(AuthHalt::InvalidProof);
                self.events.emit(HolderEvent::HolderUnauthorized);
                Answer::AuthHalted
            }
            (403, _) => {
                self.on_not_authorized();
                Answer::AuthHalted
            }
            (503, Some("lifecycle_unavailable")) => {
                self.lifecycle_unavailable = true;
                self.events.emit(HolderEvent::AuthorityUnreachable);
                Answer::BackOff
            }
            (503, Some("authority_epoch_mismatch")) => {
                self.rollback_suspected = true;
                self.events.emit(HolderEvent::AuthorityRollbackSuspected);
                Answer::BackOff
            }
            _ => {
                if response.status == 429 {
                    self.status_back_off(response.retry_after_s);
                }
                Answer::BackOff
            }
        }
    }

    fn verify_status_with_refresh(
        &mut self,
        compact: &str,
    ) -> Result<StatusAck, StatusVerificationFailure> {
        use StatusVerificationFailure::{PinWrite, Unverifiable};
        match self.verify_status_ack(compact) {
            Err("kid") => {
                let enrollment = self.writer.enrollment();
                let profile = &self.config.profile;
                let statement = crate::client::fetch_route_statement(
                    &self.transport,
                    &enrollment.issuer,
                    profile,
                )
                .ok_or(Unverifiable)?;
                let pins = crate::keys::verify_keyset(
                    &statement,
                    &enrollment.pinned,
                    profile,
                    &enrollment.issuer,
                    self.clock.wall_ms(),
                )
                .map_err(|_| Unverifiable)?;
                let seq = pins.keyset_seq;
                self.writer.pin_keys(pins).map_err(|error| {
                    crate::holder::report_write_failure(self.events.as_ref(), &error);
                    PinWrite
                })?;
                if let Some(keyset_seq) = seq {
                    self.events.emit(HolderEvent::KeysetAccepted { keyset_seq });
                }
                self.verify_status_ack(compact).map_err(|_| Unverifiable)
            }
            result => result.map_err(|_| Unverifiable),
        }
    }

    /// T7: deliver a delayed status POST receipt without sending a replacement.
    /// Only an outstanding uncertain key and its recorded attempt may resolve.
    pub fn receive_late_status_receipt(&mut self, compact: &str) -> bool {
        if self.route != RouteHealth::Normal
            || self.status_refusal.is_some()
            || self.obligations_allowed().is_err()
            || self.apply_failed_since_admit
        {
            return false;
        }
        let Ok(ack) = self.verify_status_with_refresh(compact) else {
            return false;
        };
        let Some(u) = self
            .writer
            .state()
            .status
            .uncertain
            .get(&ack.idempotency_key)
        else {
            return false;
        };
        if !Self::receipt_matches(&ack, &ack.idempotency_key, &u.body, &u.attempts) {
            return false;
        }
        self.on_status_ack(None, ack) == Answer::Receipt
    }

    fn status_unverifiable(&mut self) -> Answer {
        self.events.emit(HolderEvent::AckUnverifiable);
        self.status_unverifiable = self.status_unverifiable.saturating_add(1);
        if self.status_unverifiable >= UNVERIFIABLE_HALT {
            return self.finish_refusal(StatusRefusal::Halt(0));
        }
        Answer::Unverifiable
    }

    /// A definitive refusal retries only its durable transition. Success events
    /// and counts follow that commit; a failed write never re-enables the POST.
    fn finish_refusal(&mut self, refusal: StatusRefusal) -> Answer {
        let answer = match &refusal {
            StatusRefusal::Halt(status) => self.halt_status(*status),
            StatusRefusal::Stale(key) if self.reject_status(key, 409) => Answer::Rejected,
            StatusRefusal::Stale(_) => Answer::BackOff,
        };
        if answer == Answer::BackOff {
            self.status_refusal = Some(refusal);
        }
        answer
    }

    /// T6c (and repeated unverifiable acks): the status halts durably, and
    /// the status-first gate is released in the same commit (H.22.3).
    fn halt_status(&mut self, status: u16) -> Answer {
        let mut next = self.writer.state().status.clone();
        next.halted = true;
        let mut book = self.writer.state().obligations.clone();
        book.status_gate.status_halted = true;
        if !self.commit_lifecycle(Some(book), Some(next), false) {
            return Answer::BackOff;
        }
        self.events.emit(HolderEvent::StatusBugHalt { status });
        Answer::Halted
    }

    /// T6d: definitively not accepted; the key's bytes are pruned.
    fn reject_status(&mut self, key: &str, status: u16) -> bool {
        let mut next = self.writer.state().status.clone();
        let mut prune = Vec::new();
        if let Some(record) = next.pending.as_mut().filter(|p| p.key == key) {
            prune = std::mem::take(&mut record.attempts);
            record.status = PendingStatus::Rejected;
        }
        if let Some(u) = next.uncertain.remove(key) {
            prune.extend(u.attempts);
        }
        for attempt in &prune {
            next.manifests.remove(&attempt.digest);
        }
        if !self.commit_status(next) {
            return false;
        }
        self.prune_sent_attempts(&prune);
        self.events.emit(HolderEvent::StatusRejected { status });
        true
    }

    /// Checks a receipt's accepted claims against the body it names.
    fn receipt_matches(
        ack: &StatusAck,
        key: &str,
        body: &StatusBody,
        attempts: &[Attempt],
    ) -> bool {
        ack.idempotency_key == key
            && attempts
                .iter()
                .any(|a| a.digest == ack.report_digest && a.jti == ack.report_jti)
            && ack.accepted_quarantined_unattributed == body.quarantined_unattributed
            && ack.accepted_manifest_digest == body.quarantine_manifest_digest
            && ack.accepted_holder_state == body.holder_state
            && ack.disposition.is_some() == body.release_result.is_some()
            // A receipt is minted in the epoch of the list its report
            // cites (H.53.1); a byte-identical replay keeps that epoch.
            && ack.rev.epoch == body.list_ref.rev.epoch
            && ack.rev.seq > body.list_ref.rev.seq
    }

    fn on_status_ack(&mut self, pending: Option<&StatusPending>, ack: StatusAck) -> Answer {
        let Some(epoch) = self.high_water_epoch() else {
            return self.status_unverifiable();
        };
        let status = self.writer.state().status.clone();
        // T4: a receipt for this key's own claims.
        let (key, body, attempts, from_pending) =
            if let Some(pending) = pending.filter(|p| p.key == ack.idempotency_key) {
                (
                    pending.key.clone(),
                    pending.body.clone(),
                    pending.attempts.clone(),
                    true,
                )
            } else if let Some(uncertain) = status.uncertain.get(&ack.idempotency_key) {
                // T7: a late receipt for one of this holder's earlier keys.
                (
                    ack.idempotency_key.clone(),
                    uncertain.body.clone(),
                    uncertain.attempts.clone(),
                    false,
                )
            } else {
                return self.status_unverifiable();
            };
        if !Self::receipt_matches(&ack, &key, &body, &attempts) {
            return self.status_unverifiable();
        }
        if self
            .writer
            .write_side_file(&format!("sreceipt.{key}.jws"), ack.compact.as_bytes())
            .is_err()
        {
            self.events.emit(HolderEvent::StorageExhausted);
            return Answer::BackOff;
        }
        let before_resume =
            !from_pending && status.uncertain.get(&key).is_some_and(|u| u.before_resume);
        let mut next = status;
        let mut audit = None;
        let mut book = None;
        let mut clear_reset = false;
        if from_pending {
            next.pending = None;
        } else {
            next.uncertain.remove(&key);
        }
        if ack.rev.epoch == epoch {
            let newer = next
                .last_accepted
                .as_ref()
                .is_none_or(|a| a.rev.epoch != epoch || a.rev.seq <= ack.rev.seq);
            if newer {
                // A late ack for the superseded key is not the correction.
                if from_pending {
                    next.correction_owed = false;
                }
                next.last_accepted = Some(AcceptedStatus {
                    key: key.clone(),
                    report_digest: ack.report_digest.clone(),
                    quarantined_unattributed: body.quarantined_unattributed,
                    quarantine_manifest_digest: body.quarantine_manifest_digest.clone(),
                    rev: ack.rev.clone(),
                    accepted_ms: self.clock.wall_ms(),
                });
            }
            // L7, L8: the carried result is final for this epoch.
            if let Some(result) = &body.release_result
                && let Some(record) = next.instructions.get_mut(&result.instruction_id)
            {
                match (record.step, result.result) {
                    (ReleaseStep::Deleted | ReleaseStep::Done, ReleaseOutcome::Executed) => {
                        record.step = ReleaseStep::Done;
                        record.carried_epoch = Some(epoch.clone());
                    }
                    (
                        ReleaseStep::Mismatch | ReleaseStep::MismatchReported,
                        ReleaseOutcome::Mismatch,
                    ) => {
                        record.step = ReleaseStep::MismatchReported;
                        record.carried_epoch = Some(epoch.clone());
                    }
                    _ => {}
                }
            }
            if let Some(disposition) = ack.disposition
                && disposition != ResultDisposition::Applied
            {
                audit = Some(HolderEvent::ReleaseResultAudit {
                    disposition: disposition.wire(),
                });
            }
            clear_reset = body.holder_state == HolderStateClaim::Reset;
            // H.27: a verified current-epoch status ack opens the gate.
            let mut gate_book = self.writer.state().obligations.clone();
            if !before_resume {
                gate_book.status_gate.acked_epoch = Some(epoch.clone());
            }
            book = Some(gate_book);
        } else {
            // Minted in an epoch the holder has left: audit only (H.5).
            audit = Some(HolderEvent::AckOtherEpoch);
        }
        if !self.commit_lifecycle(book, Some(next), clear_reset) {
            return Answer::BackOff;
        }
        if ack.rev.epoch != epoch {
            self.relist_before_report = true;
        }
        if let Some(event) = audit {
            self.events.emit(event);
        }
        self.status_unverifiable = 0;
        self.prune_sent_attempts(&attempts);
        Answer::Receipt
    }

    /// N5: an audited operator resume of a halted status. The status moves
    /// to `invalidated` (attempts that may have been sent become uncertain)
    /// and the status-first gate closes again until a re-derived status
    /// gets a current-epoch ack (H.27). Returns whether it was halted.
    pub fn resume_status(&mut self) -> bool {
        let mut next = self.writer.state().status.clone();
        if !next.halted {
            return false;
        }
        next.halted = false;
        invalidate_status(&mut next);
        for attempt in next.uncertain.values_mut() {
            attempt.before_resume = true;
        }
        let mut book = self.writer.state().obligations.clone();
        book.status_gate.status_halted = false;
        book.status_gate.acked_epoch = None;
        if !self.commit_lifecycle(Some(book), Some(next), false) {
            return false;
        }
        self.status_unverifiable = 0;
        self.events.emit(HolderEvent::StatusResumed);
        true
    }

    /// After a successful apply: uncertain status attempts this list
    /// resolves. An attempt citing another epoch never commits in this
    /// lineage (H.53.2); one whose `D = exp + 120 s` is before page 0's
    /// `iat` can no longer commit (H.13, H.50.2). Neither proves it was
    /// accepted, so their effects stay owed: a release result is reported
    /// again and a reset stays pending.
    pub(crate) fn resolve_status_uncertain(&mut self) {
        let Some(applied) = &self.applied else {
            return;
        };
        let epoch = applied.list_ref.rev.epoch.clone();
        let page0_iat_s = applied.page0_iat_ms.div_euclid(1000);
        let mut next = self.writer.state().status.clone();
        let mut prune = Vec::new();
        next.uncertain.retain(|_, u| {
            let resolved = u.epoch != epoch || page0_iat_s > u.latest_exp_s() + ABSENCE_BOUND_S;
            if resolved {
                prune.extend(u.attempts.iter().cloned());
            }
            !resolved
        });
        if !prune.is_empty() && self.commit_status(next) {
            self.prune_sent_attempts(&prune);
        }
    }

    /// Drops index entries for manifests nothing may bind any more: older
    /// than the retention window and not bound by a release this holder
    /// still tracks. A manifest file goes with its last entry.
    fn prune_manifest_index(&mut self) {
        let now = self.clock.wall_ms();
        let status = self.writer.state().status.clone();
        let bound: BTreeSet<&str> = status
            .instructions
            .values()
            .map(|r| r.bound_report_digest.as_str())
            .chain(
                self.applied
                    .iter()
                    .flat_map(|a| &a.instructions)
                    .map(|i| i.bound_report_digest.as_str()),
            )
            .collect();
        let book = &self.writer.state().obligations;
        let live_keys: BTreeSet<&str> = book
            .channels
            .values()
            .filter_map(|c| c.pending.as_ref())
            .filter(|p| p.live())
            .map(|p| p.key.as_str())
            .chain(book.uncertain.keys().map(String::as_str))
            .chain(
                status
                    .pending
                    .iter()
                    .filter(|p| p.live())
                    .map(|p| p.key.as_str()),
            )
            .collect();
        let pinned: BTreeSet<&str> = status
            .pending
            .iter()
            .flat_map(|p| p.attempts.iter())
            .chain(status.uncertain.values().flat_map(|u| u.attempts.iter()))
            .map(|a| a.digest.as_str())
            .collect();
        let expired: Vec<String> = status
            .manifests
            .iter()
            .filter(|(digest, r)| {
                now - r.recorded_ms > MANIFEST_RETENTION_MS
                    && !live_keys.contains(r.key.as_str())
                    && !bound.contains(digest.as_str())
                    && !pinned.contains(digest.as_str())
            })
            .map(|(digest, _)| digest.clone())
            .collect();
        let mut next = status.clone();
        for digest in &expired {
            next.manifests.remove(digest);
        }
        let keys: BTreeSet<String> = expired
            .iter()
            .filter_map(|d| status.manifests.get(d).map(|r| r.key.clone()))
            .chain(
                status
                    .manifest_files
                    .iter()
                    .filter(|(k, ms)| {
                        now - **ms > MANIFEST_RETENTION_MS && !live_keys.contains(k.as_str())
                    })
                    .map(|(k, _)| k.clone()),
            )
            .filter(|key| next.manifests.values().all(|r| &r.key != key))
            .collect();
        if expired.is_empty() && keys.is_empty() {
            return;
        }
        for key in &keys {
            next.manifest_files.remove(key);
        }
        if self.commit_status(next) {
            for key in keys {
                let _ = self
                    .writer
                    .remove_side_file(&format!("manifest.{key}.json"));
            }
        }
    }
}

/// T3: the status becomes invalidated; any attempt that may have been sent
/// moves to `uncertain`.
pub(crate) fn invalidate_status(status: &mut StatusBook) {
    let Some(pending) = status.pending.as_mut() else {
        return;
    };
    if !pending.attempts.is_empty() {
        status.correction_owed = true;
        status.uncertain.insert(
            pending.key.clone(),
            StatusUncertain {
                before_resume: false,
                body: pending.body.clone(),
                epoch: pending.epoch.clone(),
                attempts: std::mem::take(&mut pending.attempts),
            },
        );
    }
    pending.status = PendingStatus::Invalidated;
}

/// M10 on `403`: a queued, unsent status is cancelled; a possibly-sent one
/// is kept as uncertain. Returns whether one was cancelled.
pub(crate) fn cancel_status_on_403(status: &mut StatusBook) -> bool {
    let Some(pending) = status.pending.as_ref().filter(|p| p.live()) else {
        return false;
    };
    if pending.attempts.is_empty() {
        status.pending = None;
    } else {
        invalidate_status(status);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(step: ReleaseStep, carried: Option<&str>) -> InstructionRecord {
        InstructionRecord {
            bound_report_digest: "sha256:00".into(),
            approved_count: 1,
            step,
            units: Default::default(),
            claimed: BTreeSet::new(),
            deleted: BTreeSet::new(),
            observed_manifest_digest: None,
            carried_epoch: carried.map(str::to_owned),
            order: 0,
        }
    }

    /// L10 and H.14: a finished instruction is never re-run; its result is
    /// re-reported only when it is listed in an epoch whose accepted status
    /// does not carry it.
    #[test]
    fn already_done_results_are_re_reported_only_across_a_re_listing_epoch() {
        use ReleaseOutcome as O;
        use ReleaseStep as S;
        assert_eq!(
            owed_outcome(&record(S::Deleted, None), "e1", false),
            Some(O::Executed)
        );
        assert_eq!(
            owed_outcome(&record(S::Mismatch, None), "e1", true),
            Some(O::Mismatch)
        );
        for step in [S::Reserving, S::Draining] {
            assert_eq!(owed_outcome(&record(step, None), "e1", true), None);
        }
        // Carried in this epoch: still listed by a stale list, owes nothing.
        assert_eq!(owed_outcome(&record(S::Done, Some("e1")), "e1", true), None);
        assert_eq!(
            owed_outcome(&record(S::MismatchReported, Some("e1")), "e1", true),
            None
        );
        // Re-listed after a replay into a new epoch: re-reported.
        assert_eq!(
            owed_outcome(&record(S::Done, Some("e1")), "e2", true),
            Some(O::Executed)
        );
        assert_eq!(
            owed_outcome(&record(S::MismatchReported, Some("e1")), "e2", true),
            Some(O::Mismatch)
        );
        // No longer listed: final.
        assert_eq!(
            owed_outcome(&record(S::Done, Some("e1")), "e2", false),
            None
        );
    }
}
