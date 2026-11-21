//! Durable obligation records (v2 Part I §4, §C.0 *floor*, *uncertain
//! attempt*, *bug halt*; H.22, H.27). Kwiry's ids: Annex K.1 R1–R19, T*.
//!
//! The holder keeps no history of what it owes: obligations come from every
//! applied list, and the tombstones keep their resources refused forever.
//! What it keeps per obligation is the report channel:
//!
//! - **floor**: the highest state the authority accepted in the current
//!   epoch, from a verified ack or a verified list's `last_accepted`;
//! - **pending**: the one report being derived, sent or retried, with every
//!   attempt's `jti`, digest and `exp`. Its body is fixed; the key is stable
//!   across retries and new for every re-derivation;
//! - **halted**: a bug halt; nothing is re-derived until an operator resume;
//! - **purge**: where the crash-safe purge sequence stands.
//!
//! Attempts that may have been accepted but were superseded live in
//! `uncertain` until an ack or a list resolves them. All of it lives in the
//! holder-state file and changes only through its single writer.

use crate::canonical::{canonical, labelled_digest};
use crate::wire::{ListRef, ReportState, ResourceKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Domain label of holder-chosen idempotency keys. The authority treats keys
/// as opaque; only stability across retries matters.
pub const REPORT_KEY_LABEL: &str = "holdings-report-key/v1";
/// Domain label of the quarantine manifest digest (v2 §11.2).
pub const MANIFEST_LABEL: &str = "holdings-quarantine-manifest/v1";
/// `evidence.kind` of a final-scan witness digest.
pub const EVIDENCE_KIND: &str = "witness-digest/v2";
/// Reported `operator` label for unattended reports.
pub const OPERATOR_AUTO: &str = "op_auto";
/// Same-state reports per obligation at most once per this window (v2 §12.1
/// Q7).
pub const SAME_STATE_WINDOW_MS: i64 = 300_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObligationBook {
    /// Full remaining wait when last backed off. On restart no elapsed time is trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) restart_wait_ms: Option<u64>,
    /// Report channels by obligation id.
    pub channels: BTreeMap<String, Channel>,
    /// Superseded attempts whose acceptance is unknown, by idempotency key.
    pub uncertain: BTreeMap<String, Uncertain>,
    pub status_gate: StatusGate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub state: ReportState,
    pub epoch: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub holding_id: String,
    pub resource: ResourceKey,
    pub floor: Option<Floor>,
    pub pending: Option<Pending>,
    /// Bug halt (`400`, `413`, `409 idempotency_conflict`, repeated
    /// unverifiable acks). Cleared only by an operator resume.
    pub halted: bool,
    /// Counts re-derivations, so each one gets a new key.
    pub derivations: u64,
    /// Wall time of the last same-state report derived (Q7).
    pub last_same_state_ms: Option<i64>,
    /// Counts and manifest of the last report the authority accepted.
    pub last_accepted_counts: Option<Counts>,
    pub purge: PurgeRecord,
    /// §3.4 rule 4: residual data of a retired-complete holding is being
    /// purged, and `complete` is owed again. Written before the residual
    /// claim; cleared only once a covering `complete` is acknowledged.
    #[serde(default)]
    pub reconfirm_pending: bool,
}

impl Channel {
    pub fn new(holding_id: &str, resource: ResourceKey) -> Self {
        Self {
            holding_id: holding_id.to_owned(),
            resource,
            floor: None,
            pending: None,
            halted: false,
            derivations: 0,
            last_same_state_ms: None,
            last_accepted_counts: None,
            purge: PurgeRecord::default(),
            reconfirm_pending: false,
        }
    }

    /// The floor if it belongs to `epoch`; floors of other epochs are history.
    pub fn floor_in(&self, epoch: &str) -> Option<ReportState> {
        self.floor
            .as_ref()
            .filter(|f| f.epoch == epoch)
            .map(|f| f.state)
    }

    /// Raises the floor in `epoch`; it never falls.
    pub fn raise_floor(&mut self, state: ReportState, epoch: &str) -> bool {
        match self.floor_in(epoch) {
            Some(current) if current >= state => false,
            _ => {
                self.floor = Some(Floor {
                    state,
                    epoch: epoch.to_owned(),
                });
                true
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingStatus {
    /// Derived and durable; not yet attempted, or due for a retry.
    Ready,
    /// An attempt's bytes and record are durable; it may be on the wire.
    Armed,
    /// Its claim no longer holds; it will be re-derived under a new key.
    Invalidated,
    /// The authority definitively refused it.
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub jti: String,
    /// `sha256:` over the exact compact JWS sent.
    pub digest: String,
    pub exp_s: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub key: String,
    pub body: ReportBody,
    pub status: PendingStatus,
    pub attempts: Vec<Attempt>,
    /// Epoch of the list the body cites.
    pub epoch: String,
}

impl Pending {
    pub fn live(&self) -> bool {
        matches!(self.status, PendingStatus::Ready | PendingStatus::Armed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Uncertain {
    /// A delayed 400 resolves this attempt without halting the resumed channel.
    #[serde(default)]
    pub before_resume: bool,
    pub obligation_id: String,
    pub holding_id: String,
    pub state: ReportState,
    pub epoch: String,
    pub attempts: Vec<Attempt>,
}

impl Uncertain {
    pub fn latest_exp_s(&self) -> i64 {
        self.attempts.iter().map(|a| a.exp_s).max().unwrap_or(0)
    }
}

/// v2 H.22, H.27: the optional status-first gate, derived from durable state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusGate {
    /// Set when an epoch acceptance was consumed; the gate then waits for a
    /// status ack minted in that epoch.
    pub armed_epoch: Option<String>,
    pub acked_epoch: Option<String>,
    /// The status channel is durably bug-halted: the gate is released.
    pub status_halted: bool,
}

impl StatusGate {
    /// Open when no epoch acceptance is waiting, when a current-epoch status
    /// ack is recorded, or while the status is halted (H.22.3, H.27).
    pub fn open(&self) -> bool {
        match &self.armed_epoch {
            None => true,
            Some(epoch) => self.status_halted || self.acked_epoch.as_ref() == Some(epoch),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeStep {
    /// Nothing started for this obligation in any process.
    #[default]
    Idle,
    /// Written before the purger detaches the resource.
    Detaching,
    Detached,
    /// Written before any deletion: the exact claimed units.
    Deleting,
    /// Written after the deletion pass.
    Deleted,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PurgeRecord {
    pub step: PurgeStep,
    /// Units claimed for the deletion in progress (written before deleting).
    pub claimed: BTreeSet<String>,
    /// Every unit deleted under this obligation, across passes.
    pub deleted: BTreeSet<String>,
    pub passes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub units_purged: u64,
    pub units_remaining: u64,
    pub quarantined_unattributed: u64,
    pub quarantine_manifest_digest: String,
}

/// The business claims of one obligation report (v2 §4.1), fixed for the
/// life of its key. `jti`, `iat`, `exp` and the key itself are not part of
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportBody {
    pub obligation_id: String,
    pub holding_id: String,
    pub state: ReportState,
    pub list_ref: ListRef,
    pub counts: Counts,
    /// Present for `complete`: `logical_delete`, `completed_at`, evidence.
    pub completion: Option<Completion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub method: String,
    pub completed_at: String,
    /// `sha256:` over the final-scan record bytes.
    pub evidence_digest: String,
}

/// Request-level claims every holder request carries (v2 §2.2).
pub struct Envelope<'a> {
    pub holder_id: &'a str,
    pub audience: &'a str,
    pub htu: &'a str,
}

impl ReportBody {
    /// The claim set `B` the key is derived from: the envelope's `iss`,
    /// `sub`, `aud`, `htu` and every business claim.
    pub fn business_claims(&self, envelope: &Envelope) -> Map<String, Value> {
        let mut claims = Map::new();
        claims.insert("iss".into(), envelope.holder_id.into());
        claims.insert("sub".into(), envelope.holder_id.into());
        claims.insert("aud".into(), envelope.audience.into());
        claims.insert("htu".into(), envelope.htu.into());
        claims.insert("obligation_id".into(), self.obligation_id.clone().into());
        claims.insert("holding_id".into(), self.holding_id.clone().into());
        claims.insert("state".into(), self.state.wire().into());
        claims.insert("list_ref".into(), self.list_ref.to_value());
        claims.insert(
            "counts".into(),
            json!({
                "units_purged": self.counts.units_purged,
                "units_remaining": self.counts.units_remaining,
                "quarantined_unattributed": self.counts.quarantined_unattributed,
            }),
        );
        claims.insert(
            "quarantine_manifest_digest".into(),
            self.counts.quarantine_manifest_digest.clone().into(),
        );
        if let Some(completion) = &self.completion {
            claims.insert("method".into(), completion.method.clone().into());
            claims.insert(
                "completed_at".into(),
                completion.completed_at.clone().into(),
            );
            claims.insert(
                "evidence".into(),
                json!({ "kind": EVIDENCE_KIND, "digest": completion.evidence_digest }),
            );
        }
        claims.insert("operator".into(), OPERATOR_AUTO.into());
        claims
    }

    /// `ik_` and 43 base64url characters, from `B` and the derivation count.
    pub fn idempotency_key(&self, envelope: &Envelope, derivation: u64) -> String {
        let input = json!({
            "b": Value::Object(self.business_claims(envelope)),
            "n": derivation,
        });
        let digest = labelled_digest(REPORT_KEY_LABEL, &input);
        format!("ik_{}", &crate::b64::encode(&digest)[..43])
    }

    /// The signed request claims of one attempt.
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
}

/// `qm1.` + base64url(SHA-256(label ‖ 0x00 ‖ JCS({holder_id, count, units}))).
pub fn manifest_digest(holder_id: &str, units: &crate::store::ManifestUnits) -> String {
    let value = json!({
        "holder_id": holder_id,
        "count": units.len(),
        "units": units,
    });
    format!(
        "qm1.{}",
        crate::b64::encode(&labelled_digest(MANIFEST_LABEL, &value))
    )
}

/// The manifest file stored before the first transmission of a report that
/// carries its digest (v2 §11.2), so a later release can bind to it.
pub fn manifest_bytes(holder_id: &str, units: &crate::store::ManifestUnits) -> Vec<u8> {
    canonical(&json!({
        "holder_id": holder_id,
        "count": units.len(),
        "units": units,
    }))
    .into_bytes()
}

impl ReportState {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Incomplete => "incomplete",
            Self::Complete => "complete",
        }
    }

    pub fn from_wire(text: &str) -> Option<Self> {
        match text {
            "revoked" => Some(Self::Revoked),
            "incomplete" => Some(Self::Incomplete),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }
}
