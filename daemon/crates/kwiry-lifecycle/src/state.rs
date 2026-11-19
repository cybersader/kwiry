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
/// Engine holders of the id-space besides the modeled one (HW §15 H.30,
/// H.31), abstracted to what the epoch-change mint gate reads. Only the H.25
/// focused probe gives them a role; elsewhere they stay unenrolled.
pub const PEERS: usize = 2;

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
    /// H.25, H.30: a new epoch closed minting for the id-space. `None`: open.
    pub mint_gate: Option<MintGate>,
    /// H.30.2 `lists_stopped_at` (`T_stop`) in the epoch link: the instant the
    /// old lineage stopped issuing earlier-epoch lists (H.33). Recorded in the
    /// H.25 probe only, which models time.
    pub lists_stopped_at: Option<u16>,
    /// H.15.1, H.33.2 (H.25 probe): a restore began. The restore tooling
    /// confirmed that the old authority instance is stopped and its start-up
    /// disabled; nothing is listed or minted until the restored epoch's first
    /// transaction.
    pub restoring: bool,
    /// H.44 (H.25 probe): this instance was raw-restored, bypassing the
    /// restore tooling, and has not left `authority_rollback_suspected` yet.
    pub raw: Raw,
    /// HW v2 H.48.1 (H.25 probe): the restore session's audited record of a
    /// failed source check. It is the session's, so the exit clears it.
    pub source_check: SourceCheck,
    /// HW v2 H.48, H.49 (H.25 probe): whether the old-instance-stopped
    /// confirmation was the `old_instance_stopped` attestation, and whether
    /// the exit it allowed has happened.
    pub attest: Attest,
}

/// HW v2 H.48.1: the restore tooling's record of a failed check of the
/// source deployment. The audited record names the targets tried (host,
/// store-lock path, lifecycle URL), each error and the time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SourceCheck {
    /// No failed check is recorded in this restore session: the tooling may
    /// still verify the source (H.44.3(a)).
    #[default]
    None,
    /// A failed check no older than 1 h.
    Fresh,
    /// A failed check older than 1 h at the attestation.
    Stale,
}

/// HW v2 H.48.3, H.49.3: where the attestation path stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Attest {
    /// The confirmation, if any, was the tooling's verification (H.44.3(a)).
    #[default]
    No,
    /// The `old_instance_stopped` attestation was accepted; the restore
    /// procedure has not exited suspicion yet.
    Pending,
    /// The procedure exited by attestation. Its first transaction issued the
    /// lifecycle key-set statement (H.48.3) and rotated the search-capability
    /// signing key (H.49.3), each where the design has it.
    Exited,
}

/// H.44: where a raw-restored instance stands on its way out of
/// `authority_rollback_suspected`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Raw {
    /// Not a raw-restored instance, or it has left suspicion.
    #[default]
    No,
    /// Started from a raw restore and suspected. It cannot see the source
    /// deployment, never infers that the old instance stopped, issues no list
    /// and no access grant, and answers `503 authority_epoch_mismatch` on
    /// every lifecycle route. Without a confirmation it stays here
    /// indefinitely: expected non-progress, not an L1 violation.
    Unconfirmed,
    /// Still suspected, but the old-instance-stopped confirmation has been
    /// recorded ((a) the tooling verified the source, or (b) the audited
    /// `old_instance_stopped` attestation), with `T_stop` = its instant. The
    /// supported restore procedure or `restore_journal_lost` may now proceed.
    Confirmed,
}

/// H.30: one holder's seat at the epoch-change mint gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Seat {
    /// Not a member: not in the roster in the new epoch's first transaction,
    /// or refused on every route since long before `T_stop` (H.30.1, H.32.2).
    #[default]
    Absent,
    /// A member that has not cleared yet.
    Pending,
    /// A member that cleared: an accepted current-epoch report (H.30.2(a)) or
    /// the lease timeout (H.30.2(b)).
    Cleared,
}

/// H.25 and H.30: the mint gate of the id-space after a new epoch. The
/// peers' seats are in [`Peer::seat`]. It opens (becomes `None`) once no
/// seat is pending and, after `restore_journal_lost`, the id-space-wide
/// lease timeout has passed (H.30.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MintGate {
    /// The modeled holder's seat (always enrolled and enabled).
    pub holder: Seat,
    /// H.30.4: minting also waits for the id-space-wide lease timeout.
    pub idspace_wait: bool,
}

/// A holder's roster entry at the authority (HW §2.1): enabled, and the key
/// generation it is enrolled with. Absent means not enrolled. A
/// decommissioned holder stays in the roster and is refused like a disabled
/// one, so the model folds it into `enabled: false` (H.32.2 makes both
/// members).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Roster {
    pub enabled: bool,
    pub key: u8,
    /// H.32.2, H.43: the authority has refused the holder's lifecycle
    /// credential on every route since an instant `t_r` with `t_r +
    /// serve_seconds_ceiling + 150 s` already past. `t_r` is the later of the
    /// disable (or decommission) and the closing of the holder's last open
    /// obligation, so a holder with any open obligation has none. The
    /// authority records it durably as `refused_since`, derived from the
    /// journaled `holder_disabled` and the obligations, so it is backed up and
    /// replayed with the entry. When the current lineage's durable state
    /// cannot establish it (its source was lost in a restore), it is false
    /// and the holder is a member.
    pub refused_aged: bool,
}

/// Another engine holder of the id-space (HW §15 H.30, H.31). Its own
/// lifecycle is the modeled holder's machine; the gate reads it only through
/// its roster entry, whether its current-epoch reports are accepted, and
/// whether it still serves an earlier-epoch list. Everything about one peer
/// is in this record, so peers are interchangeable and the checker sorts
/// them (`step::canonical`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Peer {
    /// The authority's live roster entry.
    pub roster: Option<Roster>,
    /// Its entry in the authority backup (taken with `AuthBackup`).
    pub backup: Option<Roster>,
    /// H.31: its latest roster entry journaled after the backup
    /// (`holder_enrolled`, `holder_key_rotated`, `holder_disabled`,
    /// `holder_enabled`); replay applies it.
    pub journaled: Option<Roster>,
    /// H.30: its seat at the current mint gate.
    pub seat: Seat,
    /// Ground truth: the key generation it signs with.
    pub key: u8,
    /// Ground truth: the epoch of the list it serves under a live lease
    /// (`None`: serves nothing).
    pub lease: Option<u8>,
    /// Ground truth (rejected H.33, H.41, H.44 and H.48.3 readings only):
    /// its earlier-epoch lease was renewed by a list or page issued after
    /// `T_stop`, so the lease timeout does not bound it.
    pub late: bool,
    /// H.33.1: a list in flight, `(epoch, holder_rev)` as sealed in its
    /// cursor at page 0; later pages are still to be fetched.
    pub cursor: Option<(u8, u8)>,
    /// Ground truth: it will never report again (gone or stuck).
    pub silent: bool,
    /// HW v2 H.48.3: it applied the key-set statement of an exit by
    /// attestation, as it must before it verifies any new-epoch list. Tracked
    /// only while the old instance still runs (a false attestation): otherwise
    /// no artefact under the retired key exists, so nothing reads it.
    pub lc_rotated: bool,
    /// HW v2 H.49.3: it applied the search-capability key rotation of an exit
    /// by attestation and refuses the old key. Tracked like `lc_rotated`.
    pub cap_rotated: bool,
    /// Ground truth, the named residual of a false attestation (H.44.4,
    /// H.48.3): the old instance renewed its earlier-epoch list after
    /// `T_stop`, before it applied the key-set statement. The lease timeout
    /// does not bound that lease.
    pub residual: bool,
    /// HW v2 H.52.4: its operator confirmed `unchanged` at `accept-epoch`
    /// after an exit by attestation, so it accepts the old capability key
    /// under its new-epoch lease. Audited by the engine and detectable from
    /// the restore record; a named residual, not a gate guarantee.
    pub unchanged: bool,
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
    /// Ghost (HW v2 H.50): for an obligation report, the states (`1 <<
    /// state`) of higher-state attempts of the same obligation that were
    /// still possibly sent, unresolved, when these bytes left. Always zero
    /// under H.50.1.
    pub retracts: u8,
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
    /// H.21.5, H.23, H.26: `clock_behind_authority`, written through the
    /// single holder-state writer when raised (C19) and when cleared (C20).
    pub clock_behind: bool,
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
    /// H.23: a `page_size: 1` clock-probe list request of this process is
    /// outstanding (its nonce and send instant live only in this process).
    pub clock_probe: bool,
    /// H.26: the in-memory `clock_behind_authority` refusal. Raised at once
    /// by C19; loaded from holder state at startup.
    pub clock_hold: bool,
    /// H.26: C19 raised the hold and its durable write has not committed.
    pub clock_unpersisted: bool,
    /// Rejected H.27 reading only: the status-first gate was released in
    /// memory by a status bug halt in this process.
    pub gate_released: bool,
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
    /// Rejected H.24 reading only: rooms whose artifacts this process took as
    /// current from the in-process baseline instead of the durable record.
    pub adopted: u8,
    /// Rejected H.28 reading only: rooms whose previously validated artifacts
    /// of an earlier `pin_ref` this process keeps serving under the H10.5
    /// stale bound.
    pub stale_carry: u8,
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
            clock_probe: false,
            clock_hold: false,
            clock_unpersisted: false,
            gate_released: false,
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
            adopted: 0,
            stale_carry: 0,
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
    /// The focused probes' clock, in seconds.
    pub exposure_elapsed: u16,
    pub capability_skew: u8,
    /// Ground truth: how far (seconds) the engine clock lags the authority
    /// clock; negative when it runs ahead (H.29). Only a local clock step
    /// changes it.
    pub lag: i8,
    /// H.23: the one clock probe of the current 60 s window has been sent.
    pub probe_window_used: bool,
    pub bad_journal: bool,
    pub list_signing: Option<(u8, Option<crate::wire::Keyset>)>,
    /// H.16: the directory each room's configured root resolves to now.
    pub root_dir: [u8; ROOMS],
    /// H10.5: the bounded stale-service window after a root mismatch ended.
    pub stale_expired: bool,
    /// H.30.2(b): the authority clock passed `T_stop + serve_seconds_ceiling
    /// + 150 s`, so every earlier-epoch lease has lapsed.
    pub lease_timeout: bool,
    /// Ground truth: the old authority instance may still be running, issuing
    /// lists and minting. A raw restore starts with it set (the source may
    /// still run, H.44.1); a true old-instance-stopped confirmation clears it,
    /// and a false attestation leaves it set (the named residual, H.44.4,
    /// narrowed by H.48.3 and H.49.3). Under the rejected H.33.2 and H.44
    /// readings a restore proceeds while it is set, so the old instance
    /// issues earlier-epoch lists after `T_stop`.
    pub old_running: bool,
    /// Checker reduction switch: peers are sorted in the canonical state
    /// (`step::canonical`). Fixed for a run.
    pub peer_symmetry: bool,
    /// HW v2 H.52.3: the capability verification key set an exit by
    /// attestation requires can't be supplied at `accept-epoch` (the restore
    /// record's new key is unavailable to the operators). Fixed for a run.
    pub key_set_unavailable: bool,
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
    /// H.25: capabilities minted around a new epoch, against a holder that
    /// still serves its earlier-epoch list.
    EpochMint,
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
    /// The `pin_ref` each room's current artifacts were actually built under
    /// (H.24: the holder's durable record must agree).
    pub derived_ref: [Option<u8>; ROOMS],
    /// H.23: verified evidence proved the engine clock more than 30 s behind,
    /// and no clock step has brought it back within 30 s since.
    pub lag_proven: bool,
    /// H.25 liveness: the epoch changed while the holder held a high-water,
    /// and minting has not reopened yet.
    pub search_due: bool,
    /// H.25 probe: the instant minting stopped (rollback suspicion or a
    /// restore began).
    pub mint_stop: Option<u16>,
    /// From this epoch on, `pin_ref`s below this were minted before a
    /// `restore_journal_lost` (the lost timeline).
    pub pin_floor: (u8, u8),
    /// From this epoch on, directories whose attestation's holding did not
    /// survive replay (the authority's refusal is the control; a holder that
    /// never saw the attestation cannot exclude them).
    pub orphan_dirs: (u8, u8),
    /// HW v2 H.50, per obligation: states (`1 << state`) the holder no longer
    /// claims, because the authority accepted a lower report the holder sent
    /// while attempts of those states were unresolved. Committing one of them
    /// afterwards leaves a stale higher claim at the authority. A state the
    /// holder derives again (R1) is claimed again and leaves the set.
    pub stale_claims: [u8; OBLS],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub auth: Auth,
    /// H.30, H.31: the other engine holders of the id-space.
    pub peers: [Peer; PEERS],
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
                mint_gate: None,
                lists_stopped_at: None,
                restoring: false,
                raw: Raw::No,
                source_check: SourceCheck::None,
                attest: Attest::No,
            },
            peers: [Peer::default(); PEERS],
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
                clock_behind: false,
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
                derived_ref: [Some(1), Some(2)],
                ..Ghost::default()
            },
        }
    }
}
