//! The model state: the authority, the holder's durable store, the holder's
//! volatile process state, the network, and the remaining adversary budgets.
//!
//! Everything is small and `Copy`-friendly so millions of states fit in
//! memory. The fixed universe is:
//!
//! - rooms `R0`, `R1` (both configured), with obligation `o` = room index;
//! - units `U0` (R0's current object), `U1` (R0's superseded object, garbage
//!   from the start, so GC has work) and `U2` (R1's current object);
//! - report channels 0 and 1 (obligation reports) and 2 (holder status);
//! - epochs 0 and 1 (a supported restore mints epoch 1);
//! - release instruction ids 0 and 1 (issued alternately, one listed at a time).

use crate::bag::Bag;

pub const ROOMS: usize = 2;
pub const UNITS: usize = 3;
pub const OBLS: usize = 2;
pub const CHANS: usize = 3;
pub const STATUS: usize = 2;
pub const INSTRS: usize = 2;
/// Storage directories a room's configured root can resolve to (H.16 pin
/// ids): `D0` and `D1` hold R0's and R1's data at the start, `D2` is spare.
pub const DIRS: usize = 3;
/// Attestations the bounded journal can hold.
pub const PIN_ENTRIES: usize = 3;

/// The room each unit's descriptor names.
pub const UNIT_ROOM: [usize; UNITS] = [0, 0, 1];
/// Each room's current (mapped) object. `U1` is never mapped.
pub const ROOM_CUR: [usize; ROOMS] = [0, 2];

pub const fn bit(i: usize) -> u8 {
    1 << i
}

pub fn units_of(r: usize) -> impl Iterator<Item = usize> {
    (0..UNITS).filter(move |u| UNIT_ROOM[*u] == r)
}

// ---------------------------------------------------------------- vocabulary

/// Obligation report state (HW §4.1), ordered `revoked < incomplete < complete`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RState {
    Revoked,
    Incomplete,
    Complete,
}

/// A list revision `{epoch, seq}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rev {
    pub epoch: u8,
    pub seq: u8,
}

/// Obligation report business body. The idempotency key is a hash of this
/// body (§5.6), so the model uses the body itself as the key: equal bodies
/// are equal keys, and a different body is always a different key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OKey {
    pub o: u8,
    pub state: RState,
    pub list_ref: Rev,
    /// `quarantine_manifest_digest`: the unattributed unit set it names.
    pub manifest: u8,
    /// `counts.units_remaining > 0`.
    pub remaining: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelResult {
    pub instr: u8,
    pub executed: bool,
    pub observed: u8,
}

/// Holder status business body (HW §14 E, G.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SKey {
    pub list_ref: Rev,
    pub reset: bool,
    pub manifest: u8,
    pub result: Option<RelResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    O(OKey),
    S(SKey),
}

impl Key {
    pub fn obl(self) -> Option<OKey> {
        match self {
            Key::O(k) => Some(k),
            Key::S(_) => None,
        }
    }
    pub fn status(self) -> Option<SKey> {
        match self {
            Key::S(k) => Some(k),
            Key::O(_) => None,
        }
    }
    pub fn list_ref(self) -> Rev {
        match self {
            Key::O(k) => k.list_ref,
            Key::S(k) => k.list_ref,
        }
    }
}

/// A signed ack as the holder sees it: the accepted state and the epoch in
/// which it was minted (`rev.epoch` of the ack).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ack {
    pub state: RState,
    pub epoch: u8,
}

// ------------------------------------------------------------------ authority

/// An authority-held root pin (HW §15 H.16): the holder-observed directory
/// (`pin_id`) and the authority-random `pin_ref` minted at attestation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pin {
    pub id: u8,
    pub pin_ref: u8,
}

/// A value of the id-space `root_pin_history`: a room, or the reserved `"*"`
/// of an attestation whose holding did not survive replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PinOwner {
    Room(u8),
    Orphan,
}

/// A `root_pin_attested` journal entry (one id-space, so `provider_scope`
/// is implicit). `pin: None` is `pin_id: none`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PinEntry {
    pub seq: u8,
    pub room: u8,
    pub pin: Option<Pin>,
    pub prev: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum APhase {
    Active,
    /// Not in this holder's projection, with no obligation (an adversarial
    /// list change), or a retired item dropped after retention.
    Unlisted,
    Terminating,
    Retired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AObl {
    pub created_seq: u8,
    pub blocked: bool,
    pub blocked_by: Option<DeclineEntry>,
    /// The last accepted report's ack (the "current ack" of HW §4.4).
    pub accepted: Option<Ack>,
    /// Idempotency receipts: keys accepted, with the epoch of their ack.
    pub receipts: Bag<(OKey, u8, u8), 6>,
}

impl AObl {
    pub fn complete(&self) -> bool {
        matches!(
            self.accepted,
            Some(Ack {
                state: RState::Complete,
                ..
            })
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ARoom {
    pub phase: APhase,
    pub obl: Option<AObl>,
}

/// A `quarantine_release` instruction (HW §14 D), bound to the manifest of
/// the accepted status report the human approved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Instr {
    pub id: u8,
    pub manifest: u8,
    pub approved_count: u8,
    pub issued_seq: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AuthCore {
    pub epoch: u8,
    pub seq: u8,
    pub rooms: [ARoom; ROOMS],
    pub instr: Option<Instr>,
    pub next_instr: u8,
    pub holder_rev: u8,
    pub release_declined: bool,
    pub final_instr: u8,
    pub superseded_mismatch: u8,
    pub status: Option<SKey>,
    pub status_receipts: Bag<(SKey, u8, Option<ReleaseDisposition>), 8>,
    /// H.16: each room holding's `root_pin` (terminating and retired keep
    /// their last pin) and the grow-only id-space history.
    pub pins: [Option<Pin>; ROOMS],
    pub pin_history: [Option<PinOwner>; DIRS],
}

/// Digest and decision identities are symbolic in this model; the typed body
/// stands for the retained status digest and is not exposed as resource data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeclineEntry {
    pub seq: u8,
    pub status_digest: SKey,
    pub declined_count: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FinalEntry {
    pub seq: u8,
    pub status_digest: SKey,
    pub executed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Auth {
    pub core: AuthCore,
    /// A state backup taken for a later supported restore.
    pub backup: Option<Box<AuthCore>>,
    /// H.19 custody projection: the unchanged restored record, including
    /// status/acks, the instruction and blocked decisions. Not protocol state.
    pub journal_lost_evidence: Option<Box<AuthCore>>,
    /// Bounded projection of the off-store H.1 journal. Room index represents
    /// the stable holder/holding/obligation identity, not a new allocation.
    pub journal_retired: u8,
    pub journal_declined: [Option<DeclineEntry>; ROOMS],
    pub journal_cleared: [u8; ROOMS],
    pub journal_finalized: [Option<FinalEntry>; INSTRS],
    pub journal_issued: [Option<(u8, Instr)>; INSTRS],
    /// H.16.3 `root_pin_attested` entries, in journal order.
    pub journal_pins: [Option<PinEntry>; PIN_ENTRIES],
    /// `pin_ref`s are authority-random and never reused, even across restores.
    pub next_pin_ref: u8,
    pub rollback_suspected: bool,
    pub failed_restore: bool,
    pub journal_lost_audited: bool,
}

// -------------------------------------------------------------------- lists

/// One list item as the holder sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Item {
    Absent,
    Active,
    /// `terminating`, with `obligation.last_accepted.state`.
    Terminating(Option<RState>),
    Blocked(Option<RState>),
    /// `retired` with `complete`.
    Retired,
}

impl Item {
    pub fn last_accepted(self) -> Option<RState> {
        match self {
            Item::Terminating(l) | Item::Blocked(l) => l,
            Item::Retired => Some(RState::Complete),
            _ => None,
        }
    }
    pub fn obligated(self) -> bool {
        matches!(
            self,
            Item::Terminating(_) | Item::Blocked(_) | Item::Retired
        )
    }
}

/// A complete verified list (all pages collapsed: page-level failures are
/// list errors).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListMsg {
    pub rev: Rev,
    pub items: [Item; ROOMS],
    pub holder_rev: u8,
    pub instr: Option<Instr>,
    /// H.16.4 `root_pin` of each room item (part of `state_digest`).
    pub pins: [Option<Pin>; ROOMS],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListErr {
    /// Network failure, deadline, `429`, `503 lifecycle_unavailable`, `410`.
    Transient,
    /// `503 authority_epoch_mismatch` (H10).
    EpochMismatch,
    /// `401`: `replay` or `invalid_holder_proof`.
    Unauthorized {
        replay: bool,
    },
    ProofExpired,
    BadRequest,
    /// Historical rev 10.1 mutation only. H.17 forbids 409 on the list route.
    LegacyStaleListRef,
    /// `403 holder_not_authorized`.
    Forbidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListResp {
    List(ListMsg),
    Err { code: ListErr, healthy: bool },
}

// ------------------------------------------------------------------ messages

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RespKind {
    /// `200` with an ack for the request's key (new or byte-identical replay).
    Accepted {
        epoch: u8,
        disposition: Option<ReleaseDisposition>,
        accepted_holder_reset: Option<bool>,
    },
    /// `200 already_complete` carrying the original ack (convergence).
    AlreadyComplete(Ack),
    /// `409 state_regression` carrying the current ack (convergence).
    StateRegression(Ack),
    /// `409 complete_refused_unattributed`.
    CompleteRefused,
    /// `409 stale_list_ref`.
    StaleListRef,
    /// `404 obligation_not_found`.
    NotFound,
    /// `409 idempotency_conflict`, `400 invalid_request`, `413`.
    Bug,
    /// `500`, accompanied by `lifecycle.invariant_violation` at the authority.
    InvariantViolation,
    /// `401` (`replay` or `invalid_holder_proof`).
    Unauthorized {
        replay: bool,
    },
    ProofExpired,
    /// `403 holder_not_authorized`.
    Forbidden,
    /// `429`, `503 lifecycle_unavailable`, network failure, deadline.
    Transient,
    /// `503 authority_epoch_mismatch`.
    EpochMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReleaseDisposition {
    Applied,
    UnknownInstruction,
    AlreadyFinal,
}

/// A report or status request on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Req {
    pub key: Key,
    /// A network duplicate: the authority answers `401 replay`.
    pub replay: bool,
    /// The JWS `exp` passed while the attempt sat in the transport queue.
    pub aged: bool,
    /// The authority received a timely proof, but the CAS was delayed.
    pub commit_late: bool,
    /// The holder's connection is still open (a response can be delivered).
    pub alive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Resp {
    pub key: Key,
    pub kind: RespKind,
    /// Ghost: produced by the authority's correct processing (not an
    /// injected fault).
    pub healthy: bool,
    /// Detached from its connection: it can still be delivered after the
    /// holder restarts (the late receipts R18/R19 anticipate).
    pub late: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Net {
    pub list: Option<ListResp>,
    pub reqs: [Bag<Req, 6>; CHANS],
    pub resps: [Bag<Resp, 4>; CHANS],
}

// ------------------------------------------------------------ holder durable

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DurSt {
    Ready,
    /// An attempt is durable. Whether it was handed off is volatile (`sent`
    /// is never written, R2h has no W).
    Armed,
    Invalidated,
    Rejected,
    Halted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pending {
    pub key: Key,
    pub st: DurSt,
    /// An earlier attempt of this key may have been transmitted.
    pub earlier: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Floor {
    pub epoch: u8,
    pub state: RState,
    /// The accepted report body, when known (a list's `last_accepted` does
    /// not carry it).
    pub key: Option<OKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Acceptance {
    pub from: Rev,
    pub to: u8,
}

/// Quarantine ledger owner as stored. Revision 8 stores the owner;
/// revision 9 stores only a release pin and derives the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QOwner {
    Unattributed,
    Obligation,
    Release(u8),
    Derived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UDur {
    Present,
    Quarantined(QOwner),
    Deleted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IDur {
    Unseen,
    Reserved(u8),
    Draining(u8),
    Deleted(u8),
    Done,
    Mismatch(u8),
    MismatchReported(u8),
}

impl IDur {
    pub fn reserved_units(self) -> u8 {
        match self {
            IDur::Reserved(u) | IDur::Draining(u) | IDur::Deleted(u) => u,
            _ => 0,
        }
    }
}

/// The published generation: layout and withheld rooms, and the units a
/// release reservation excludes from the withheld map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation {
    pub layout: u8,
    pub withheld: u8,
    pub excluded: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Durable {
    pub hw: Option<Rev>,
    pub tomb: u8,
    pub accept: Option<Acceptance>,
    pub floors: [Option<Floor>; OBLS],
    pub slots: [Option<Pending>; CHANS],
    pub uncertain: [Bag<Key, 5>; CHANS],
    pub status_acc: Option<SKey>,
    pub last_list_ref: Option<Rev>,
    pub pinned_keys: crate::wire::KeyPin,
    pub manifests: u8,
    pub root_holder: Option<u8>,
    pub reset_pending: bool,
    /// H.19: send status even at zero inventory, until accepted in this epoch.
    pub epoch_status_pending: bool,
    /// H.16 holder pin-history `{pin_id → resource.id}` from verified lists,
    /// grow-only, persisted in A1's W with the high-water.
    pub pin_history: [Option<u8>; DIRS],
    /// The `pin_ref` each room's current artifacts were derived under.
    pub derived: [Option<u8>; ROOMS],
    pub state_holder: Option<u8>,
    pub instr: [IDur; INSTRS],
    pub units: [UDur; UNITS],
    pub generation: Generation,
    /// `rollback.json` written and `current.v2.json` gone (markers settled).
    pub rollback_closed: bool,
    /// The `.reserve` preallocation is present (data root and holder
    /// directory reserves are merged into one slot in the model).
    pub reserve: bool,
}

// ----------------------------------------------------------- holder volatile

/// M1 phases the model distinguishes. P0–P2 collapse into `Down → P3`
/// (`Boot`); P3b collapses into P3a (no migration is modeled); P6 is the
/// instant between S12 and S13.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Down,
    P3,
    P3a,
    P5,
    P7,
}

/// M10.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Route {
    Normal,
    Halted401,
    Halted403,
}

/// M4b class in the in-memory snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    /// No list applied in this process yet.
    Pending,
    Unknown,
    Active,
    Obligated,
    RetiredComplete,
    TombUnlisted,
    /// Tombstoned but listed active (C9, `tombstone_conflict`): refused,
    /// kept, attributed to the listed holding.
    Conflict,
}

impl Class {
    pub fn obligated(self) -> bool {
        matches!(self, Class::Obligated | Class::RetiredComplete)
    }
    pub fn listed(self) -> bool {
        matches!(
            self,
            Class::Active | Class::Obligated | Class::RetiredComplete | Class::Conflict
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    Obligation(u8),
    Gc,
    Release(u8),
}

/// M5 registry state of a unit that is still in `objects/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UMem {
    Live,
    Revoked(Reason),
}

/// A purge pass's final-scan record (§5.6 steps 4–6), valid while `clean`
/// (the `obl_ver[o]`, `res_ver[r]` and `inv_ver` it read are unchanged).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scan {
    pub state: RState,
    pub remaining: bool,
    pub inv: u8,
    pub clean: bool,
}

/// The volatile side of a report slot whose durable state is `Armed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotVol {
    /// Nothing in this process. With durable `Armed`, the attempt is from an
    /// earlier process ("restored armed").
    Idle,
    /// R2 committed; awaiting the R2h handoff. `clean`: the versions R2
    /// recorded are unchanged.
    Armed { clean: bool },
    /// Handed to the transport queue, not yet on the wire.
    Queued { clean: bool, aged: bool },
    /// On the wire. `awaiting`: a response is still expected on an open
    /// connection; otherwise a retry is due.
    Sent { awaiting: bool },
}

/// M9 reserve state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rsv {
    Full,
    Partial,
    Insufficient,
    Exhausted,
}

/// One admitted governed request in the H.21 probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InFlightCapability {
    pub room: u8,
    pub deadline: i32,
}

/// Next step of the apply transaction (M4a).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApplyStep {
    A1,
    A2,
    A3,
    A4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Volatile {
    pub phase: Phase,
    pub route: Route,
    pub fetching: bool,
    /// A consecutive expiry has already consumed this exchange’s immediate retry.
    pub proof_retried: u8,
    pub proof_backoff: u8,
    pub lease_until: u8,
    pub admitted_until: u8,
    pub capability: Option<InFlightCapability>,
    /// The newest list admitted by an H row in this process (it is also the
    /// list an in-progress apply is applying).
    pub admitted: Option<ListMsg>,
    /// The newest list whose A4 completed in this process.
    pub applied: Option<ListMsg>,
    pub apply: Option<ApplyStep>,
    /// The object registry's reference sets (§4.3): the generation as of the
    /// last L3 swap (A4, L1's detach, startup). GC reads these, not the
    /// durable record.
    pub mem_gen: Generation,
    pub class: [Class; ROOMS],
    /// Revision 9 (A0): rooms refused in memory by an admitted narrowing whose
    /// apply has not completed.
    pub block: u8,
    /// H.16 AP24: rooms whose applied pin the holder's pin-history holds under
    /// another room (`root_pin_previously_listed`), set at A4.
    pub pin_conflict: u8,
    /// The unattributed inventory as of the last L3 update.
    pub inv: u8,
    pub umem: [UMem; UNITS],
    pub scans: [Option<Scan>; OBLS],
    pub slotv: [SlotVol; CHANS],
    pub rsv: Rsv,
    /// `unexpected_epoch` alert: the epoch an operator may accept.
    pub alert_epoch: Option<u8>,
    /// A relist must apply before re-deriving reports (R7 `stale_list_ref`,
    /// R8, R6e).
    pub relist: bool,
    /// Per instruction: its reservation is detached in this process.
    pub detached: u8,
    /// Per instruction: L6 ran in this process.
    pub resulted: u8,
    /// An L1 publisher transaction between its W and its G.
    pub rel_txn: Option<u8>,
}

impl Volatile {
    pub fn down() -> Self {
        Volatile {
            phase: Phase::Down,
            route: Route::Normal,
            fetching: false,
            proof_retried: 0,
            proof_backoff: 0,
            lease_until: u8::MAX,
            admitted_until: u8::MAX,
            capability: None,
            admitted: None,
            applied: None,
            apply: None,
            mem_gen: Generation {
                layout: 0,
                withheld: 0,
                excluded: 0,
            },
            class: [Class::Pending; ROOMS],
            block: 0,
            pin_conflict: 0,
            inv: 0,
            umem: [UMem::Live; UNITS],
            scans: [None; OBLS],
            slotv: [SlotVol::Idle; CHANS],
            rsv: Rsv::Insufficient,
            alert_epoch: None,
            relist: false,
            detached: 0,
            resulted: 0,
            rel_txn: None,
        }
    }
}

// ----------------------------------------------------------------- budgets

/// Remaining adversary and scenario budgets. Budgeted actions can never form
/// a cycle, and they are excluded from the liveness suffix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Budget {
    /// Rooms scenario actions may target (retire, unlist, relist, drop).
    pub retire_rooms: u8,
    pub change_rooms: u8,
    pub crashes: u8,
    pub faults: u8,
    pub changes: u8,
    pub retires: u8,
    pub drops: u8,
    pub restores: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Env {
    pub disk_full: bool,
    pub probe: Probe,
    pub open_unlinked: u8,
    pub late_published: u8,
    pub late_once: bool,
    pub holder: u8,
    pub recovering: bool,
    pub legacy_started: bool,
    pub clock: u8,
    pub envelope: Option<crate::wire::Envelope>,
    pub exposure_elapsed: u8,
    pub capability_skew: u8,
    /// H.21 refuses governed requests until clock correction is confirmed.
    pub clock_behind_authority: bool,
    pub bad_journal: bool,
    pub list_signing: Option<(u8, Option<crate::wire::Keyset>)>,
    /// H.16: the directory each room's configured root resolves to now.
    pub root_dir: [u8; ROOMS],
    /// H10.5: the bounded stale-service window after a root mismatch ended.
    pub stale_expired: bool,
}

/// Focused profiles opt in to independent boundaries without multiplying
/// every clock, filesystem and startup schedule into the core profiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Probe {
    #[default]
    None,
    Root,
    Lease,
    Evidence,
    Proof,
    Governance,
    Exposure,
    Journal,
    /// H.16 root pins: attestation, storage changes and rebuilds.
    Pin,
}

/// Ghost state read only by invariants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ghost {
    pub consumed: Bag<Acceptance, 2>,
    pub retired_at: [Option<Rev>; ROOMS],
    /// H.19: an epoch acceptance was consumed and no status has been
    /// accepted in the new epoch yet.
    pub status_due: bool,
    /// H.16 ground truth: the room whose bytes each directory holds.
    pub dir_room: [Option<u8>; DIRS],
    /// The room whose bytes each room's current artifacts were built from.
    pub derived_from: [Option<u8>; ROOMS],
    /// From this epoch on, `pin_ref`s below this were minted before a
    /// `restore_journal_lost` (the lost timeline).
    pub pin_floor: (u8, u8),
    /// From this epoch on, directories whose attestation's holding did not
    /// survive replay (the authority's refusal is the control; a holder that
    /// never saw the attestation cannot exclude them).
    pub orphan_dirs: (u8, u8),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub auth: Auth,
    pub env: Env,
    pub dur: Durable,
    pub vol: Volatile,
    pub net: Net,
    pub budget: Budget,
    pub ghost: Ghost,
}

impl State {
    /// Both rooms active and mapped, nothing listed yet, the holder down
    /// (about to boot). With `garbage`, `U1` is R0's superseded object (work
    /// for GC); without it, `U1` was already deleted.
    pub fn initial(budget: Budget, garbage: bool) -> State {
        let room = ARoom {
            phase: APhase::Active,
            obl: None,
        };
        State {
            auth: Auth {
                core: AuthCore {
                    epoch: 0,
                    seq: 1,
                    rooms: [room; ROOMS],
                    instr: None,
                    next_instr: 0,
                    holder_rev: 1,
                    release_declined: false,
                    final_instr: 0,
                    superseded_mismatch: 0,
                    status: None,
                    status_receipts: Bag::default(),
                    pins: [
                        Some(Pin { id: 0, pin_ref: 1 }),
                        Some(Pin { id: 1, pin_ref: 2 }),
                    ],
                    pin_history: [Some(PinOwner::Room(0)), Some(PinOwner::Room(1)), None],
                },
                backup: None,
                journal_lost_evidence: None,
                journal_retired: 0,
                journal_declined: [None; ROOMS],
                journal_cleared: [0; ROOMS],
                journal_finalized: [None; INSTRS],
                journal_issued: [None; INSTRS],
                journal_pins: [None; PIN_ENTRIES],
                next_pin_ref: 3,
                rollback_suspected: false,
                failed_restore: false,
                journal_lost_audited: false,
            },
            env: Env {
                root_dir: [0, 1],
                ..Env::default()
            },
            dur: Durable {
                hw: None,
                tomb: 0,
                accept: None,
                floors: [None; OBLS],
                slots: [None; CHANS],
                uncertain: [Bag::default(); CHANS],
                status_acc: None,
                last_list_ref: None,
                pinned_keys: crate::wire::KeyPin::ENROLLED,
                manifests: 1,
                root_holder: Some(0),
                reset_pending: false,
                epoch_status_pending: false,
                pin_history: [None; DIRS],
                derived: [Some(1), Some(2)],
                state_holder: Some(0),
                instr: [IDur::Unseen; INSTRS],
                units: [
                    UDur::Present,
                    if garbage {
                        UDur::Present
                    } else {
                        UDur::Deleted
                    },
                    UDur::Present,
                ],
                generation: Generation {
                    layout: bit(0) | bit(1),
                    withheld: 0,
                    excluded: 0,
                },
                rollback_closed: false,
                reserve: true,
            },
            vol: Volatile::down(),
            net: Net::default(),
            budget,
            ghost: Ghost {
                dir_room: [Some(0), Some(1), None],
                derived_from: [Some(0), Some(1)],
                ..Ghost::default()
            },
        }
    }
}
