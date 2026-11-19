//! Obligation reports (v2 Part I §4, §4.5, §C.0, §12.1 Q3/Q7, H.3, H.5,
//! H.8, H.12, H.13, H.20, H.22, H.27, H.37, H.45). Kwiry's rows: Annex K.1
//! R1–R19 and M10.
//!
//! **Derivation (R1).** From the list applied in this process and the
//! purger's independent census:
//! - `revoked` while units of the resource remain and no final scan of this
//!   process covers it;
//! - `incomplete` after a final scan, while anything of the resource remains
//!   or the holder's unattributed inventory is non-empty;
//! - `complete` only from a clean final scan of this process whose observed
//!   inventory is unchanged, with an empty unattributed inventory.
//!
//! **Monotonicity.** A state is derived only above the floor (or equal to
//! it, as a counts update or a residual reconfirmation at most once per
//! 300 s), and never below any attempt that may have been sent and has not
//! been resolved: a refused report counts as never sent (Q3), a possibly
//! sent one is kept as an uncertain attempt until an ack, a list's
//! `last_accepted` or page-0 absence inference resolves it.
//!
//! **Transmission (R2, R2d).** Every first send and retry passes the
//! transmission guard against the current list and census. The exact bytes
//! are written to the holder directory and the attempt recorded before
//! anything reaches the wire. Immediately before the first byte the census,
//! route health and fail-stop are checked again: a late builder output, a
//! handle, a reclassification or an auth halt cancels the attempt as never
//! transmitted.
//!
//! **Answers.** Only a verified ack moves a floor: a replay receipt for one
//! of the holder's own keys, or a convergence receipt (`409 state_regression`
//! with `current_ack`, `200` with the outcome header `already_complete`, or
//! an ack under another key). An ack minted in another epoch is audit only.
//! Error classes follow their H.12 position: refusals placed before the
//! commit are definitive (R7, R8), bug classes halt the channel (R9), `401`
//! and `403` are auth halts, everything else is treated as unreachable.

use crate::canonical::{is_sha256_tagged, sha256_tagged};
use crate::client::{HttpResponse, LIST_TIMEOUTS, Transport, TransportError, problem_code};
use crate::events::HolderEvent;
use crate::holder::{AuthHalt, Holder, RouteHealth};
use crate::obligations::{
    Attempt, Channel, Completion, Counts, Envelope, ObligationBook, Pending, PendingStatus,
    ReportBody, Uncertain, manifest_bytes, manifest_digest,
};
use crate::profile::ROUTE_REPORT;
use crate::purge::{Inventory, ObligationsRefused, Target};
use crate::store::{ArtifactPurger, ResourceStore};
use crate::verify::VerifiedList;
use crate::wire::{ObligationState, Phase, ReportState, Rev};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Report transmissions per holder per second (H.8).
pub const REPORTS_PER_SECOND: usize = 10;
/// An attempt held this long before its first byte is cancelled (R2q).
pub const QUEUE_CAP_MS: u64 = 30_000;
/// Absence inference (H.13, H.50.3): an attempt is proven not committed by
/// a verified list whose page-0 `iat` is greater than `D = exp + 120 s`.
/// Both sides are authority-clock values.
pub const ABSENCE_BOUND_S: i64 = 120;
/// Unverifiable acks in a row before the channel halts (R17).
pub const UNVERIFIABLE_HALT: u8 = 3;
const BACKOFF_START_MS: u64 = 10_000;
const BACKOFF_MAX_MS: u64 = 300_000;

/// A verified obligation ack (v2 §4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ack {
    pub compact: String,
    pub jti: String,
    pub idempotency_key: String,
    pub report_jti: String,
    pub report_digest: String,
    pub obligation_id: String,
    pub holding_id: String,
    pub accepted_state: ReportState,
    pub obligation_state: ObligationState,
    pub phase: Phase,
    pub rev: Rev,
}

const ACK_MEMBERS: [&str; 15] = [
    "iss",
    "aud",
    "sub",
    "iat",
    "jti",
    "idempotency_key",
    "report_jti",
    "report_digest",
    "obligation_id",
    "holding_id",
    "accepted_state",
    "obligation_state",
    "phase",
    "rev",
    "receipt_label",
];

/// What one report pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportSummary {
    pub derived: usize,
    pub sent: usize,
    pub receipts: usize,
    pub converged: usize,
    pub rejected: usize,
    pub invalidated: usize,
    pub halted: usize,
    /// Attempts cancelled at their first byte (never transmitted).
    pub cancelled: usize,
    /// Reports that wait: the status-first gate, a rate limit, a back-off,
    /// or a `complete` whose proof has not been made in this process.
    pub waiting: usize,
}

enum Guard {
    Pass,
    /// The claim may still hold, but its proof is not available yet.
    Wait,
    Fail,
}

/// How an answer was classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Receipt,
    Converged,
    Rejected,
    Halted,
    Unverifiable,
    RetryReplay,
    RetryExpired,
    BackOff,
    AuthHalted,
}

fn text<'a>(claims: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    claims.get(name).and_then(Value::as_str)
}

fn obligation_state(text: &str) -> Option<ObligationState> {
    Some(match text {
        "open" => ObligationState::Open,
        "retrieval_revoked" => ObligationState::RetrievalRevoked,
        "quarantined" => ObligationState::Quarantined,
        "blocked" => ObligationState::Blocked,
        "complete" => ObligationState::Complete,
        "escalated" => ObligationState::Escalated,
        _ => return None,
    })
}

/// v2 §4.2: the obligation state and phase an accepted report can leave.
fn valid_pair(accepted: ReportState, state: ObligationState, phase: Phase) -> bool {
    match (accepted, phase) {
        (ReportState::Complete, Phase::Retired) => state == ObligationState::Complete,
        (ReportState::Complete, _) => false,
        (_, Phase::Terminating) => matches!(
            state,
            ObligationState::RetrievalRevoked
                | ObligationState::Quarantined
                | ObligationState::Blocked
                | ObligationState::Escalated
        ),
        (_, Phase::Retired) => state == ObligationState::Escalated,
        (_, Phase::Active) => false,
    }
}

pub(crate) fn sent_file(digest: &str) -> String {
    format!("sent.{}.jws", digest.trim_start_matches("sha256:"))
}

fn token(bytes: usize) -> Option<String> {
    crate::b64::random_token(bytes).ok()
}

impl<S: ResourceStore, T: Transport> Holder<S, T> {
    fn envelope_strings(&self) -> (String, String, String) {
        let enrollment = self.writer.enrollment();
        (
            enrollment.holder_id.clone(),
            enrollment.issuer.clone(),
            format!("{}{ROUTE_REPORT}", enrollment.issuer),
        )
    }

    pub(crate) fn high_water_epoch(&self) -> Option<String> {
        self.writer
            .state()
            .high_water
            .as_ref()
            .map(|hw| hw.epoch.clone())
    }

    /// Verifies an obligation ack: closed header and members, exact `typ`,
    /// ES256 under a live pinned key, exact `iss`, `aud` and `sub`, the
    /// grammar of every member, `receipt_label: "confirmed"` and no `exp`.
    pub fn verify_ack(&self, compact: &str) -> Result<Ack, &'static str> {
        let profile = &self.config.profile;
        let enrollment = self.writer.enrollment();
        let parsed = crate::jws::parse(compact).map_err(|_| "jws")?;
        if parsed.typ != profile.typ_report_ack {
            return Err("typ");
        }
        if parsed.alg != "ES256" {
            return Err("alg");
        }
        // v2 H.37.3, H.48: a retired kid's artefacts fail, replays included.
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
        if claims.len() != ACK_MEMBERS.len()
            || claims.keys().any(|k| !ACK_MEMBERS.contains(&k.as_str()))
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
        if text(claims, "receipt_label") != Some("confirmed") {
            return Err("receipt_label");
        }
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
        let obligation_id = checked("obligation_id", crate::grammar::obligation_id)?;
        let holding_id = checked("holding_id", crate::grammar::holding_id)?;
        let accepted_state = text(claims, "accepted_state")
            .and_then(ReportState::from_wire)
            .ok_or("accepted_state")?;
        let obligation_state = text(claims, "obligation_state")
            .and_then(obligation_state)
            .ok_or("obligation_state")?;
        let phase = match text(claims, "phase") {
            Some("active") => Phase::Active,
            Some("terminating") => Phase::Terminating,
            Some("retired") => Phase::Retired,
            _ => return Err("phase"),
        };
        let rev = crate::verify::parse_rev(claims.get("rev").ok_or("rev")?).map_err(|_| "rev")?;
        Ok(Ack {
            compact: compact.to_owned(),
            jti,
            idempotency_key,
            report_jti,
            report_digest,
            obligation_id,
            holding_id,
            accepted_state,
            obligation_state,
            phase,
            rev,
        })
    }

    /// The obligation records a verified list implies (R12, R13, R15),
    /// computed without writing: the apply commits them with the high-water
    /// and tombstones in its one A1 write, so a floor learned from the list
    /// is durable before anything can be reported against it. Returns the
    /// book and the attempts whose sent bytes are no longer needed.
    pub(crate) fn adopted_book(&self, list: &VerifiedList) -> (ObligationBook, Vec<Attempt>) {
        let epoch = list.head.rev.epoch.clone();
        let page0_iat_s = list.page0_iat_ms.div_euclid(1000);
        let listed: std::collections::HashMap<
            String,
            (crate::wire::Item, crate::wire::Obligation),
        > = list
            .items
            .iter()
            .filter_map(|listed| {
                let item = &listed.item;
                item.obligation
                    .clone()
                    .map(|o| (o.obligation_id.clone(), (item.clone(), o)))
            })
            .collect();
        let mut book = self.writer.state().obligations.clone();
        let mut prune: Vec<Attempt> = Vec::new();
        // R12: an obligation the list no longer carries ends its channel;
        // its uncertain attempts stay for R15.
        let ended: Vec<String> = book
            .channels
            .keys()
            .filter(|ob| !listed.contains_key(*ob))
            .cloned()
            .collect();
        for ob in ended {
            if let Some(pending) = book.channels.get(&ob).and_then(|c| c.pending.clone()) {
                move_to_uncertain(&mut book, &ob, &pending);
            }
            book.channels.remove(&ob);
        }
        // R13: floor adoption from `last_accepted`.
        for (ob, (item, obligation)) in &listed {
            let Some((accepted, _)) = obligation.last_accepted else {
                continue;
            };
            let channel = book
                .channels
                .entry(ob.clone())
                .or_insert_with(|| Channel::new(&item.holding_id, item.resource.clone()));
            channel.raise_floor(accepted, &epoch);
            if let Some(pending) = &channel.pending
                && accepted > pending.body.state
            {
                prune.extend(pending.attempts.iter().cloned());
                channel.pending = None;
            }
        }
        // R15, H.50.2: an uncertain attempt is resolved by a list whose
        // `last_accepted` covers it, or whose page-0 `iat` is past its `D`
        // while `last_accepted` is still below it.
        book.uncertain.retain(|_, uncertain| {
            let Some((_, obligation)) = listed.get(&uncertain.obligation_id) else {
                return true;
            };
            let accepted = obligation.last_accepted.map(|(state, _)| state);
            let covered = accepted.is_some_and(|a| a >= uncertain.state);
            let not_committed = page0_iat_s > uncertain.latest_exp_s() + ABSENCE_BOUND_S;
            let resolved = covered || not_committed;
            if resolved {
                prune.extend(uncertain.attempts.iter().cloned());
            }
            !resolved
        });
        (book, prune)
    }

    /// After a successful apply: sent bytes nothing refers to are removed,
    /// and reports may run again.
    pub(crate) fn after_apply(&mut self, prune: &[Attempt]) {
        self.relist_before_report = false;
        self.apply_failed_since_admit = false;
        self.rollback_suspected = false;
        self.prune_sent(prune);
        self.resolve_status_uncertain();
    }

    /// Removes sent bytes nothing refers to any more (best effort).
    fn prune_sent(&mut self, attempts: &[Attempt]) {
        for attempt in attempts {
            let _ = self.writer.remove_side_file(&sent_file(&attempt.digest));
        }
    }

    /// One pass of the report channels: invalidate stale reports, derive new
    /// ones, transmit what is due and classify every answer.
    pub fn report_once(
        &mut self,
        purger: &dyn ArtifactPurger,
    ) -> Result<ReportSummary, ObligationsRefused> {
        self.obligations_allowed()?;
        let mut summary = ReportSummary::default();
        if self.route != RouteHealth::Normal || self.apply_failed_since_admit {
            return Ok(summary);
        }
        let Ok(inventory) = self.inventory(purger) else {
            self.events
                .emit(HolderEvent::PurgeStepFailed { step: "census" });
            return Ok(summary);
        };
        self.drop_stale_scans(&inventory);
        let targets = self.targets_for_report(&inventory);
        self.invalidate_failing(&targets, &inventory, &mut summary);
        for target in &targets {
            self.obligations_allowed()?;
            if self.derive_and_store(target, &inventory) {
                summary.derived += 1;
            }
        }
        for target in &targets {
            self.obligations_allowed()?;
            if self.route != RouteHealth::Normal {
                break;
            }
            self.transmit(target, purger, &mut summary);
        }
        self.emit_reserve_uses();
        Ok(summary)
    }

    /// Every obligation the applied list carries that this holder reports on:
    /// purge targets, plus obligated items with nothing held.
    fn targets_for_report(&self, inventory: &Inventory) -> Vec<Target> {
        let mut targets = self.targets(inventory);
        if let Some(applied) = &self.applied {
            for item in &applied.items {
                let Some(obligation) = &item.obligation else {
                    continue;
                };
                if item.is_obligated()
                    && self.writer.state().is_tombstoned(&item.resource.id)
                    && !targets
                        .iter()
                        .any(|t| t.obligation_id == obligation.obligation_id)
                {
                    targets.push(Target {
                        obligation_id: obligation.obligation_id.clone(),
                        holding_id: item.holding_id.clone(),
                        resource: item.resource.clone(),
                        residual: false,
                    });
                }
            }
        }
        targets
    }

    /// A final scan proves nothing once the observed inventory moved.
    fn drop_stale_scans(&mut self, inventory: &Inventory) {
        let Some(applied) = &self.applied else {
            return;
        };
        let stale: Vec<String> = self
            .verified
            .iter()
            .filter(|(ob, scan)| {
                let resource = applied.items.iter().find_map(|item| {
                    item.obligation
                        .as_ref()
                        .filter(|o| &o.obligation_id == *ob)
                        .map(|_| &item.resource)
                });
                resource.is_none_or(|r| inventory.binding(r) != scan.binding)
            })
            .map(|(ob, _)| ob.clone())
            .collect();
        for ob in stale {
            self.verified.remove(&ob);
            self.events.emit(HolderEvent::PurgeScanInvalidated);
        }
    }

    fn derive(&self, target: &Target, inventory: &Inventory) -> Option<ReportBody> {
        let applied = self.applied.as_ref()?;
        let book = &self.writer.state().obligations;
        let purged = book
            .channels
            .get(&target.obligation_id)
            .map_or(0, |c| c.purge.deleted.len());
        let remaining = inventory.remaining(&target.resource);
        let unattributed = inventory.unattributed_count();
        let counts = Counts {
            units_purged: u64::try_from(purged).unwrap_or(u64::MAX),
            units_remaining: remaining,
            quarantined_unattributed: unattributed,
            quarantine_manifest_digest: manifest_digest(
                &self.writer.enrollment().holder_id,
                &inventory.unattributed,
            ),
        };
        let scan = self
            .verified
            .get(&target.obligation_id)
            .filter(|scan| scan.binding == inventory.binding(&target.resource));
        let (state, completion) = match scan {
            Some(scan) if scan.clean() && unattributed == 0 => (
                ReportState::Complete,
                Some(Completion {
                    method: "logical_delete".into(),
                    completed_at: scan.scanned_at.clone(),
                    evidence_digest: scan.evidence_digest(),
                }),
            ),
            Some(_) => (ReportState::Incomplete, None),
            None if remaining > 0 => (ReportState::Revoked, None),
            None => return None,
        };
        Some(ReportBody {
            obligation_id: target.obligation_id.clone(),
            holding_id: target.holding_id.clone(),
            state,
            list_ref: applied.list_ref.clone(),
            counts,
            completion,
        })
    }

    /// R2's transmission guard: re-derive the claim from the current list
    /// and census.
    fn guard(&self, target: &Target, pending: &Pending, inventory: &Inventory) -> Guard {
        if self.high_water_epoch().as_deref() != Some(pending.epoch.as_str()) {
            return Guard::Fail;
        }
        let body = &pending.body;
        let Some(current) = self.derive(target, inventory) else {
            // Nothing of the resource remains and no scan of this process
            // covers it yet. That falsifies `revoked`; any other claim waits
            // for a scan rather than being invalidated by absent bytes.
            return if body.state == ReportState::Revoked {
                Guard::Fail
            } else {
                Guard::Wait
            };
        };
        if current.counts != body.counts {
            return Guard::Fail;
        }
        match body.state {
            ReportState::Revoked => {
                if current.counts.units_remaining > 0 {
                    Guard::Pass
                } else {
                    Guard::Fail
                }
            }
            ReportState::Incomplete => {
                if current.state == ReportState::Complete {
                    Guard::Fail
                } else if body.counts.units_remaining > 0
                    || body.counts.quarantined_unattributed > 0
                    || current.state == ReportState::Incomplete
                {
                    Guard::Pass
                } else {
                    Guard::Fail
                }
            }
            // `complete` needs a clean scan made in this process; after a
            // restart it waits for one rather than changing its body.
            ReportState::Complete => match current.state {
                ReportState::Complete => Guard::Pass,
                _ if !self.verified.contains_key(&target.obligation_id)
                    && body.counts.units_remaining == 0
                    && body.counts.quarantined_unattributed == 0 =>
                {
                    Guard::Wait
                }
                _ => Guard::Fail,
            },
        }
    }

    /// R3: invalidate every live report whose claim no longer holds.
    fn invalidate_failing(
        &mut self,
        targets: &[Target],
        inventory: &Inventory,
        summary: &mut ReportSummary,
    ) {
        let book = self.writer.state().obligations.clone();
        let mut next = book.clone();
        let mut changed = false;
        for (ob, channel) in &book.channels {
            let Some(pending) = channel.pending.as_ref().filter(|p| p.live()) else {
                continue;
            };
            let failing = match targets.iter().find(|t| &t.obligation_id == ob) {
                Some(target) => matches!(self.guard(target, pending, inventory), Guard::Fail),
                None => true,
            };
            if failing {
                invalidate(&mut next, ob);
                summary.invalidated += 1;
                changed = true;
            }
        }
        if changed {
            self.commit_book(next);
        }
    }

    /// The highest state of any attempt for `ob` that may have been sent and
    /// is not resolved.
    fn possibly_sent_max(book: &ObligationBook, ob: &str) -> Option<ReportState> {
        let pending = book
            .channels
            .get(ob)
            .and_then(|c| c.pending.as_ref())
            .filter(|p| !p.attempts.is_empty() && p.status != PendingStatus::Rejected)
            .map(|p| p.body.state);
        book.uncertain
            .values()
            .filter(|u| u.obligation_id == ob)
            .map(|u| u.state)
            .chain(pending)
            .max()
    }

    /// R1 with its enqueue guard. Returns whether a new report was stored.
    fn derive_and_store(&mut self, target: &Target, inventory: &Inventory) -> bool {
        if self.relist_before_report {
            return false;
        }
        let Some(epoch) = self.high_water_epoch() else {
            return false;
        };
        let Some(body) = self.derive(target, inventory) else {
            return false;
        };
        let book = self.writer.state().obligations.clone();
        let channel = book
            .channels
            .get(&target.obligation_id)
            .cloned()
            .unwrap_or_else(|| Channel::new(&target.holding_id, target.resource.clone()));
        if channel.halted {
            return false;
        }
        let state = body.state;
        // §4.4: never below a state that may already have been sent.
        if Self::possibly_sent_max(&book, &target.obligation_id).is_some_and(|max| state < max) {
            return false;
        }
        let now = self.clock.wall_ms();
        let same_state = match channel.floor_in(&epoch) {
            None => false,
            Some(floor) if state > floor => false,
            Some(floor) if state == floor => {
                let update = channel.last_accepted_counts.as_ref() != Some(&body.counts)
                    || (target.residual && state == ReportState::Complete);
                let window_open = channel
                    .last_same_state_ms
                    .is_none_or(|last| now - last >= crate::obligations::SAME_STATE_WINDOW_MS);
                if !update || !window_open {
                    return false;
                }
                true
            }
            Some(_) => return false,
        };
        let mut uncertain_from_pending = None;
        let mut answered: Vec<Attempt> = Vec::new();
        if let Some(pending) = &channel.pending {
            if pending.status == PendingStatus::Rejected {
                answered.clone_from(&pending.attempts);
            }
            if pending.live() {
                if state <= pending.body.state {
                    return false;
                }
                uncertain_from_pending = Some(pending.clone());
            } else if !pending.attempts.is_empty() && pending.status == PendingStatus::Invalidated {
                uncertain_from_pending = Some(pending.clone());
            }
        }

        let (holder_id, audience, htu) = self.envelope_strings();
        let envelope = Envelope {
            holder_id: &holder_id,
            audience: &audience,
            htu: &htu,
        };
        let derivation = channel.derivations + 1;
        let key = body.idempotency_key(&envelope, derivation);
        // R1's side files, in order, before the pending record.
        if let Some(scan) = self.verified.get(&target.obligation_id).cloned()
            && state == ReportState::Complete
            && self
                .writer
                .write_side_file(&format!("scan.{key}.json"), &scan.record)
                .is_err()
        {
            return false;
        }
        let units: BTreeSet<String> = inventory.unattributed.clone();
        if self
            .writer
            .write_side_file(
                &format!("manifest.{key}.json"),
                &manifest_bytes(&holder_id, &units),
            )
            .is_err()
        {
            self.events.emit(HolderEvent::StorageExhausted);
            return false;
        }
        let mut next = book;
        if let Some(previous) = uncertain_from_pending {
            move_to_uncertain(&mut next, &target.obligation_id, &previous);
        }
        let entry = next
            .channels
            .entry(target.obligation_id.clone())
            .or_insert(channel);
        entry.derivations = derivation;
        if same_state {
            entry.last_same_state_ms = Some(now);
        }
        entry.pending = Some(Pending {
            key,
            body,
            status: PendingStatus::Ready,
            attempts: Vec::new(),
            epoch,
        });
        let stored = self.commit_book(next);
        if stored {
            self.prune_sent(&answered);
        }
        stored
    }

    fn rate_allows(&mut self) -> bool {
        let now = self.clock.mono_ms();
        while self
            .report_sends
            .front()
            .is_some_and(|sent| now.saturating_sub(*sent) >= 1000)
        {
            self.report_sends.pop_front();
        }
        self.report_sends.len() < REPORTS_PER_SECOND
    }

    fn back_off(&mut self, retry_after_s: Option<u64>) {
        let now = self.clock.mono_ms();
        self.report_backoff_ms = self
            .report_backoff_ms
            .saturating_mul(2)
            .clamp(BACKOFF_START_MS, BACKOFF_MAX_MS);
        let wait = retry_after_s
            .map_or(0, |s| s.saturating_mul(1000))
            .max(self.report_backoff_ms);
        self.report_backoff_until = now.saturating_add(wait);
    }

    /// R2 → R2d → the answer, for one obligation's live report.
    fn transmit(
        &mut self,
        target: &Target,
        purger: &dyn ArtifactPurger,
        summary: &mut ReportSummary,
    ) {
        let ob = target.obligation_id.clone();
        let mut replay_retried = false;
        let mut expiry_retried = false;
        loop {
            if self.fail_stopped() || self.route != RouteHealth::Normal {
                return;
            }
            let book = self.writer.state().obligations.clone();
            let Some(channel) = book.channels.get(&ob) else {
                return;
            };
            let Some(pending) = channel.pending.clone().filter(Pending::live) else {
                return;
            };
            if channel.halted {
                return;
            }
            // H.22, H.27: a first send waits while the status-first gate is
            // shut; a report already sent while it was open is retried.
            if pending.attempts.is_empty() && !book.status_gate.open() {
                summary.waiting += 1;
                return;
            }
            if self.clock.mono_ms() < self.report_backoff_until || !self.rate_allows() {
                summary.waiting += 1;
                return;
            }
            let Ok(inventory) = self.inventory(purger) else {
                return;
            };
            match self.guard(target, &pending, &inventory) {
                Guard::Pass => {}
                Guard::Wait => {
                    summary.waiting += 1;
                    return;
                }
                Guard::Fail => {
                    let mut next = book;
                    invalidate(&mut next, &ob);
                    self.commit_book(next);
                    summary.invalidated += 1;
                    return;
                }
            }
            let Some(sent) = self.arm_attempt(&ob, &pending) else {
                return;
            };
            // The record as armed, with every attempt including this one.
            let Some(pending) = self
                .writer
                .state()
                .obligations
                .channels
                .get(&ob)
                .and_then(|c| c.pending.clone())
            else {
                return;
            };
            // R2d: the linearization point, immediately before the first
            // byte. New publications are frozen from this census until the
            // send completes, so nothing can appear in between. A moved
            // inventory, an auth halt, a fail-stop, an aged attempt or a
            // freeze the adapter cannot give cancels it as never transmitted.
            let freeze = purger.freeze_publications().ok();
            let first_byte_ok = freeze.is_some()
                && !self.fail_stopped()
                && self.route == RouteHealth::Normal
                && self.clock.mono_ms().saturating_sub(sent.armed_mono) < QUEUE_CAP_MS
                && self.inventory(purger).is_ok_and(|now| {
                    now.binding(&target.resource) == inventory.binding(&target.resource)
                        && self
                            .derive(target, &now)
                            .is_some_and(|c| c.counts == pending.body.counts)
                });
            if !first_byte_ok {
                drop(freeze);
                self.cancel_attempt(&ob, &sent.attempt);
                summary.cancelled += 1;
                return;
            }
            self.report_sends.push_back(self.clock.mono_ms());
            summary.sent += 1;
            let url = format!("{}{ROUTE_REPORT}", self.writer.enrollment().issuer);
            let response = self.transport.post(
                &url,
                &self.config.profile.report_request_content_type(),
                sent.compact.as_bytes(),
                LIST_TIMEOUTS,
            );
            drop(freeze);
            let answer = self.classify(&ob, &pending, response);
            match answer {
                Answer::Receipt => {
                    summary.receipts += 1;
                    self.report_backoff_ms = 0;
                    return;
                }
                Answer::Converged => {
                    summary.converged += 1;
                    return;
                }
                Answer::Rejected => {
                    summary.rejected += 1;
                    return;
                }
                Answer::Halted => {
                    summary.halted += 1;
                    return;
                }
                Answer::Unverifiable | Answer::AuthHalted => return,
                Answer::RetryReplay if !replay_retried => replay_retried = true,
                Answer::RetryExpired if !expiry_retried => expiry_retried = true,
                Answer::RetryExpired => {
                    self.events.emit(HolderEvent::ProofExpiredClockAlert);
                    self.back_off(None);
                    return;
                }
                Answer::RetryReplay | Answer::BackOff => {
                    self.back_off(None);
                    return;
                }
            }
        }
    }

    /// R2: sign a fresh attempt, write its exact bytes, then record it.
    fn arm_attempt(&mut self, ob: &str, pending: &Pending) -> Option<Armed> {
        let (holder_id, audience, htu) = self.envelope_strings();
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
            &self.config.profile.typ_report_request,
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
            digest,
            exp_s: iat_s + crate::wire::REQUEST_LIFETIME_SECONDS,
        };
        let mut next = self.writer.state().obligations.clone();
        let channel = next.channels.get_mut(ob)?;
        let record = channel.pending.as_mut()?;
        record.attempts.push(attempt.clone());
        record.status = PendingStatus::Armed;
        // G.3: a release may bind to these exact bytes; the manifest their
        // body names is indexed by their digest in the same commit.
        let mut status = self.writer.state().status.clone();
        status.manifests.insert(
            attempt.digest.clone(),
            crate::status::ManifestRef {
                key: pending.key.clone(),
                manifest_digest: pending.body.counts.quarantine_manifest_digest.clone(),
                recorded_ms: self.clock.wall_ms(),
            },
        );
        if !self.commit_lifecycle(Some(next), Some(status), false) {
            return None;
        }
        Some(Armed {
            compact,
            attempt,
            armed_mono: self.clock.mono_ms(),
        })
    }

    /// R2q → R2x: the attempt never left; prune it and invalidate the report.
    fn cancel_attempt(&mut self, ob: &str, attempt: &Attempt) {
        let mut next = self.writer.state().obligations.clone();
        if let Some(pending) = next.channels.get_mut(ob).and_then(|c| c.pending.as_mut()) {
            pending.attempts.retain(|a| a != attempt);
        }
        invalidate(&mut next, ob);
        if self.commit_book(next) {
            self.prune_sent(std::slice::from_ref(attempt));
        }
    }

    pub(crate) fn classify(
        &mut self,
        ob: &str,
        pending: &Pending,
        response: Result<HttpResponse, TransportError>,
    ) -> Answer {
        let response = match response {
            Ok(response) => response,
            // R10 / R2c: sent, outcome unknown; the attempt stays recorded.
            Err(_) => return Answer::BackOff,
        };
        let code = problem_code(&response);
        match (response.status, code.as_deref()) {
            (200, _) => {
                let outcome = response
                    .header(&self.config.profile.outcome_header)
                    .map(str::to_owned);
                if response.content_type.as_deref()
                    != Some(self.config.profile.report_ack_content_type().as_str())
                {
                    return self.unverifiable(ob);
                }
                let Ok(compact) = String::from_utf8(response.body) else {
                    return self.unverifiable(ob);
                };
                match self.verify_ack(&compact) {
                    Ok(ack) => self.on_ack(
                        ob,
                        pending,
                        ack,
                        outcome.as_deref() == Some("already_complete"),
                    ),
                    Err(_) => self.unverifiable(ob),
                }
            }
            (409, Some("state_regression")) => {
                let current = crate::strict_json::parse(&response.body)
                    .ok()
                    .and_then(|v| {
                        v.get("current_ack")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    });
                match current.map(|c| self.verify_ack(&c)) {
                    Some(Ok(ack)) => self.on_ack(ob, pending, ack, true),
                    _ => self.unverifiable(ob),
                }
            }
            (409, Some("complete_refused_unattributed" | "stale_list_ref")) | (404, _) => {
                if matches!(code.as_deref(), Some("stale_list_ref")) || response.status == 404 {
                    self.relist_before_report = true;
                }
                self.reject(ob, pending, response.status);
                Answer::Rejected
            }
            (409, Some("idempotency_conflict")) | (400 | 413, _) => {
                self.halt(ob, response.status);
                Answer::Halted
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
            (503, Some("authority_epoch_mismatch")) => {
                // H.12 step 2 or 4: no purge until a verified list applies.
                self.rollback_suspected = true;
                self.events.emit(HolderEvent::AuthorityRollbackSuspected);
                Answer::BackOff
            }
            _ => {
                if response.status == 429 {
                    self.back_off(response.retry_after_s);
                }
                Answer::BackOff
            }
        }
    }

    fn unverifiable(&mut self, ob: &str) -> Answer {
        self.events.emit(HolderEvent::AckUnverifiable);
        let count = self.unverifiable.entry(ob.to_owned()).or_insert(0);
        *count += 1;
        if *count >= UNVERIFIABLE_HALT {
            self.halt(ob, 0);
            return Answer::Halted;
        }
        Answer::Unverifiable
    }

    /// R9 (and R17 after repeated unverifiable acks): bug halt.
    fn halt(&mut self, ob: &str, status: u16) {
        let mut next = self.writer.state().obligations.clone();
        if let Some(channel) = next.channels.get_mut(ob) {
            channel.halted = true;
        }
        self.commit_book(next);
        self.events.emit(HolderEvent::ReportBugHalt { status });
    }

    /// R7, R8: definitively not accepted; the key's bytes are pruned.
    fn reject(&mut self, ob: &str, pending: &Pending, status: u16) {
        let mut next = self.writer.state().obligations.clone();
        let mut prune = Vec::new();
        if let Some(record) = next
            .channels
            .get_mut(ob)
            .and_then(|c| c.pending.as_mut())
            .filter(|p| p.key == pending.key)
        {
            prune = std::mem::take(&mut record.attempts);
            record.status = PendingStatus::Rejected;
        }
        if self.commit_book(next) {
            self.prune_sent(&prune);
        }
        self.events.emit(HolderEvent::ReportRejected { status });
    }

    /// M10: `403` on any route. Nothing is servable, no traffic runs, and
    /// every queued unsent report is cancelled; attempts that may have been
    /// sent are kept as uncertain.
    pub(crate) fn on_not_authorized(&mut self) {
        self.route = RouteHealth::AuthHalt(AuthHalt::NotAuthorized);
        self.gate.set_auth_halt();
        self.events.emit(HolderEvent::HolderNotAuthorized);
        let mut next = self.writer.state().obligations.clone();
        let mut cancelled = 0;
        let obligations: Vec<String> = next.channels.keys().cloned().collect();
        for ob in obligations {
            let Some(pending) = next.channels.get(&ob).and_then(|c| c.pending.clone()) else {
                continue;
            };
            if !pending.live() {
                continue;
            }
            cancelled += 1;
            if pending.attempts.is_empty() {
                if let Some(channel) = next.channels.get_mut(&ob) {
                    channel.pending = None;
                }
            } else {
                invalidate(&mut next, &ob);
            }
        }
        let mut status = self.writer.state().status.clone();
        if crate::status::cancel_status_on_403(&mut status) {
            cancelled += 1;
        }
        // The route halt above already stops every send in memory; the
        // durable record only matters across the operator restart.
        if cancelled > 0 {
            self.commit_lifecycle(Some(next), Some(status), false);
            self.events
                .emit(HolderEvent::ReportsCancelled { count: cancelled });
        }
    }

    /// N5: an audited operator resume of a bug-halted report channel. Its
    /// report moves to `invalidated` (attempts that may have been sent
    /// become uncertain) and the next pass re-derives it under a new key.
    /// Returns whether the channel was halted.
    pub fn resume_report(&mut self, obligation_id: &str) -> bool {
        let mut next = self.writer.state().obligations.clone();
        let Some(channel) = next.channels.get_mut(obligation_id).filter(|c| c.halted) else {
            return false;
        };
        channel.halted = false;
        invalidate(&mut next, obligation_id);
        if !self.commit_book(next) {
            return false;
        }
        self.unverifiable.remove(obligation_id);
        self.events.emit(HolderEvent::ReportResumed);
        true
    }

    /// Stores an ack's bytes under `name` in the holder directory.
    fn store_receipt(&mut self, name: &str, ack: &Ack) -> bool {
        self.writer
            .write_side_file(name, ack.compact.as_bytes())
            .is_ok()
    }

    fn on_ack(&mut self, ob: &str, pending: &Pending, ack: Ack, convergence: bool) -> Answer {
        let answer = self.on_ack_inner(ob, pending, ack, convergence);
        let epoch = self.high_water_epoch().unwrap_or_default();
        let channel = self.writer.state().obligations.channels.get(ob);
        let complete = channel.and_then(|c| c.floor_in(&epoch)) == Some(ReportState::Complete);
        let owed = channel.is_some_and(|c| c.reconfirm_pending);
        if complete {
            self.verified.remove(ob);
            if owed && channel.is_some_and(|c| c.pending.is_none()) {
                let mut next = self.writer.state().obligations.clone();
                if let Some(channel) = next.channels.get_mut(ob) {
                    channel.reconfirm_pending = false;
                }
                self.commit_book(next);
            }
        }
        answer
    }

    fn on_ack_inner(&mut self, ob: &str, pending: &Pending, ack: Ack, convergence: bool) -> Answer {
        let Some(epoch) = self.high_water_epoch() else {
            return self.unverifiable(ob);
        };
        let book = self.writer.state().obligations.clone();
        // H.5: a replay path is taken only when both the key and the
        // obligation are the ones this request reported; anything else is
        // convergence (which itself requires the reported obligation).
        let own_obligation = ack.obligation_id == ob && pending.body.obligation_id == ob;
        if !convergence && own_obligation && ack.idempotency_key == pending.key {
            // Replay path (R5): a receipt for this key's own claims.
            let matches = pending
                .attempts
                .iter()
                .any(|a| a.digest == ack.report_digest)
                && ack.obligation_id == pending.body.obligation_id
                && ack.holding_id == pending.body.holding_id
                && ack.accepted_state == pending.body.state;
            if !matches {
                // V2: the attempt the ack names is not on record.
                return self.unverifiable(ob);
            }
            if !self.store_receipt(&format!("receipt.{}.jws", pending.key), &ack) {
                return Answer::BackOff;
            }
            let mut next = book;
            let prune = pending.attempts.clone();
            if let Some(channel) = next.channels.get_mut(ob) {
                if ack.rev.epoch == epoch {
                    channel.raise_floor(ack.accepted_state, &epoch);
                    channel.last_accepted_counts = Some(pending.body.counts.clone());
                    channel.pending = None;
                } else if let Some(record) = channel.pending.as_mut() {
                    // Minted in another epoch: audit only (H.5).
                    record.status = PendingStatus::Rejected;
                    self.relist_before_report = true;
                }
            }
            prune_covered_uncertain(&mut next, ob, &epoch);
            self.unverifiable.remove(ob);
            if self.commit_book(next) {
                self.prune_sent(&prune);
            }
            return Answer::Receipt;
        }
        if !convergence
            && own_obligation
            && let Some(uncertain) = book
                .uncertain
                .get(&ack.idempotency_key)
                .filter(|u| u.obligation_id == ob)
                .cloned()
        {
            // R18, R19: a late receipt for one of this holder's earlier keys.
            let matches = uncertain.obligation_id == ack.obligation_id
                && uncertain.holding_id == ack.holding_id
                && uncertain.state == ack.accepted_state
                && uncertain
                    .attempts
                    .iter()
                    .any(|a| a.digest == ack.report_digest);
            if !matches {
                return self.unverifiable(ob);
            }
            if !self.store_receipt(&format!("receipt.{}.jws", ack.idempotency_key), &ack) {
                return Answer::BackOff;
            }
            let mut next = book;
            next.uncertain.remove(&ack.idempotency_key);
            if ack.rev.epoch == epoch
                && uncertain.epoch == epoch
                && let Some(channel) = next.channels.get_mut(&uncertain.obligation_id)
            {
                channel.raise_floor(ack.accepted_state, &epoch);
                if channel
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.body.state <= ack.accepted_state)
                {
                    channel.pending = None;
                }
            } else {
                self.events.emit(HolderEvent::AckOtherEpoch);
            }
            self.unverifiable.remove(ob);
            if self.commit_book(next) {
                self.prune_sent(&uncertain.attempts);
            }
            return Answer::Receipt;
        }
        // Convergence path (R6, R6e): never a receipt for the sent claims.
        let Some(channel) = book.channels.get(ob) else {
            return self.unverifiable(ob);
        };
        if ack.obligation_id != ob
            || ack.holding_id != channel.holding_id
            || !valid_pair(ack.accepted_state, ack.obligation_state, ack.phase)
            || !crate::grammar::jti(&ack.jti)
        {
            return self.unverifiable(ob);
        }
        if !self.store_receipt(&format!("conv.{}.jws", ack.jti), &ack) {
            return Answer::BackOff;
        }
        let mut next = book;
        let mut prune = Vec::new();
        if ack.rev.epoch == epoch {
            if let Some(channel) = next.channels.get_mut(ob) {
                channel.raise_floor(ack.accepted_state, &epoch);
                if let Some(record) = channel.pending.as_ref()
                    && record.body.state <= ack.accepted_state
                {
                    prune = record.attempts.clone();
                    channel.pending = None;
                }
            }
            prune_covered_uncertain(&mut next, ob, &epoch);
            self.events.emit(HolderEvent::ReportConverged);
        } else {
            // R6e: audit only; no floor change; relist before re-deriving.
            if let Some(record) = next
                .channels
                .get_mut(ob)
                .and_then(|c| c.pending.as_mut())
                .filter(|p| p.key == pending.key)
            {
                record.status = PendingStatus::Rejected;
            }
            self.relist_before_report = true;
            self.events.emit(HolderEvent::AckOtherEpoch);
        }
        self.unverifiable.remove(ob);
        if self.commit_book(next) {
            self.prune_sent(&prune);
        }
        Answer::Converged
    }

    /// v2 H.22, H.27 hook (S5 calls it): an epoch acceptance was consumed,
    /// so first sends wait for a status ack minted in `epoch`.
    pub fn arm_status_gate(&mut self, epoch: &str) -> bool {
        let mut next = self.writer.state().obligations.clone();
        next.status_gate.armed_epoch = Some(epoch.to_owned());
        next.status_gate.status_halted = false;
        self.commit_book(next)
    }

    /// v2 H.27: a verified status ack minted in `epoch`. The status channel
    /// records this itself, in the receipt's own commit; the hook stays for
    /// hosts and tests.
    pub fn record_status_ack(&mut self, epoch: &str) -> bool {
        let mut next = self.writer.state().obligations.clone();
        next.status_gate.acked_epoch = Some(epoch.to_owned());
        self.commit_book(next)
    }

    /// v2 H.22.3, H.27: the status channel entered a bug halt (opens the
    /// gate) or an operator resume invalidated it (closes it again until a
    /// current-epoch ack). `status` records both itself, in the same commit
    /// as the halt or resume.
    pub fn record_status_halted(&mut self, halted: bool) -> bool {
        let mut next = self.writer.state().obligations.clone();
        next.status_gate.status_halted = halted;
        if !halted {
            next.status_gate.acked_epoch = None;
        }
        self.commit_book(next)
    }
}

struct Armed {
    compact: String,
    attempt: Attempt,
    armed_mono: u64,
}

fn move_to_uncertain(book: &mut ObligationBook, ob: &str, pending: &Pending) {
    if pending.attempts.is_empty() {
        return;
    }
    let holding_id = book
        .channels
        .get(ob)
        .map(|c| c.holding_id.clone())
        .unwrap_or_else(|| pending.body.holding_id.clone());
    book.uncertain.insert(
        pending.key.clone(),
        Uncertain {
            obligation_id: ob.to_owned(),
            holding_id,
            state: pending.body.state,
            epoch: pending.epoch.clone(),
            attempts: pending.attempts.clone(),
        },
    );
}

/// R3: the report becomes invalidated; any attempt that may have been sent
/// moves to `uncertain`.
fn invalidate(book: &mut ObligationBook, ob: &str) {
    let Some(pending) = book.channels.get(ob).and_then(|c| c.pending.clone()) else {
        return;
    };
    move_to_uncertain(book, ob, &pending);
    if let Some(record) = book.channels.get_mut(ob).and_then(|c| c.pending.as_mut()) {
        record.attempts.clear();
        record.status = PendingStatus::Invalidated;
    }
}

/// Uncertain attempts for `ob` at or below the floor are resolved.
fn prune_covered_uncertain(book: &mut ObligationBook, ob: &str, epoch: &str) {
    let Some(floor) = book.channels.get(ob).and_then(|c| c.floor_in(epoch)) else {
        return;
    };
    book.uncertain
        .retain(|_, u| !(u.obligation_id == ob && u.epoch == epoch && u.state <= floor));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convergence_pairs_follow_section_4_2() {
        use ObligationState as O;
        use ReportState as R;
        assert!(valid_pair(R::Complete, O::Complete, Phase::Retired));
        assert!(!valid_pair(R::Complete, O::Quarantined, Phase::Terminating));
        assert!(valid_pair(
            R::Revoked,
            O::RetrievalRevoked,
            Phase::Terminating
        ));
        assert!(valid_pair(R::Incomplete, O::Blocked, Phase::Terminating));
        assert!(valid_pair(R::Incomplete, O::Escalated, Phase::Retired));
        assert!(!valid_pair(R::Incomplete, O::Open, Phase::Terminating));
        assert!(!valid_pair(R::Revoked, O::Complete, Phase::Retired));
    }
}
