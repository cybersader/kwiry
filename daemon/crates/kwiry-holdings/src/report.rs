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
// Positive permission to recover the live queue after an operator restart.
// Removing an existing file does not need space for another lifecycle record.
const QUEUE_SEAL: &str = "report-queue.seal";
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

impl ReportSummary {
    fn record(&mut self, answer: Answer) {
        match answer {
            Answer::Receipt => self.receipts += 1,
            Answer::Converged => self.converged += 1,
            Answer::Rejected => self.rejected += 1,
            Answer::Halted => self.halted += 1,
            _ => {}
        }
    }
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
    Duplicate,
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
        self.list_failed = false;
        self.apply_failed_since_admit = false;
        self.lifecycle_unavailable = false;
        self.rollback_suspected = false;
        self.prune_sent(prune);
        self.resolve_status_uncertain();
    }

    /// Removes sent bytes nothing refers to any more (best effort).
    pub(crate) fn prune_sent(&mut self, attempts: &[Attempt]) {
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
        let (received, _) = self.drain_deliveries();
        let mut summary = ReportSummary::default();
        if received || self.traffic_halted() {
            return Ok(summary);
        }
        let debts: Vec<_> = self
            .writer
            .state()
            .status
            .owed
            .iter()
            .filter(|o| o.for_report())
            .cloned()
            .collect();
        if !debts.is_empty() {
            for owed in debts {
                summary.record(self.apply_owed(owed));
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
        self.drop_stale_scans(&inventory);
        let targets = self.targets_for_report(&inventory);
        self.invalidate_failing(&targets, &inventory, &mut summary);
        for target in &targets {
            self.obligations_allowed()?;
            self.derive_and_store(target, &inventory, &mut summary);
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
        let mut changed = 0;
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
                changed += 1;
            }
        }
        if changed != 0 {
            self.commit_books_then(Some(next), None, false, |_| summary.invalidated += changed);
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

    /// R1 with its enqueue guard.
    fn derive_and_store(
        &mut self,
        target: &Target,
        inventory: &Inventory,
        summary: &mut ReportSummary,
    ) {
        if self.relist_before_report {
            return;
        }
        let Some(epoch) = self.high_water_epoch() else {
            return;
        };
        let Some(body) = self.derive(target, inventory) else {
            return;
        };
        let book = self.writer.state().obligations.clone();
        let channel = book
            .channels
            .get(&target.obligation_id)
            .cloned()
            .unwrap_or_else(|| Channel::new(&target.holding_id, target.resource.clone()));
        if channel.halted {
            return;
        }
        let state = body.state;
        // §4.4: never below a state that may already have been sent.
        if Self::possibly_sent_max(&book, &target.obligation_id).is_some_and(|max| state < max) {
            return;
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
                    return;
                }
                true
            }
            Some(_) => return,
        };
        let mut uncertain_from_pending = None;
        let mut answered: Vec<Attempt> = Vec::new();
        if let Some(pending) = &channel.pending {
            if pending.status == PendingStatus::Rejected {
                answered.clone_from(&pending.attempts);
            }
            if pending.live() {
                if state <= pending.body.state {
                    return;
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
            return;
        }
        if self
            .writer
            .write_side_file(
                &format!("manifest.{key}.json"),
                &manifest_bytes(&holder_id, &inventory.unattributed),
            )
            .is_err()
        {
            self.events.emit(HolderEvent::StorageExhausted);
            return;
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
        self.commit_books_then(Some(next), None, false, |holder| {
            holder.prune_sent(&answered);
            summary.derived += 1;
        });
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
            self.drain_deliveries();
            if self.fail_stopped() || self.route != RouteHealth::Normal || self.relist_before_report
            {
                return;
            }
            let book = self.writer.state().obligations.clone();
            let Some(channel) = book.channels.get(&ob) else {
                return;
            };
            let Some(pending) = channel.pending.clone().filter(Pending::live) else {
                return;
            };
            if channel.halted
                || self
                    .writer
                    .state()
                    .status
                    .owed
                    .iter()
                    .any(|o| o.blocks_report(&ob))
            {
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
                    self.commit_books_then(Some(next), None, false, |_| summary.invalidated += 1);
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
                })
                && !self.inbox.has_refusal();
            if !first_byte_ok {
                drop(freeze);
                self.cancel_attempt(&ob, &sent.attempt, summary);
                self.drain_deliveries();
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
            summary.record(answer);
            match answer {
                Answer::Receipt => {
                    self.report_backoff_ms = 0;
                    return;
                }
                Answer::Converged | Answer::Rejected | Answer::Halted => return,
                Answer::Duplicate | Answer::Unverifiable | Answer::AuthHalted => return,
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
    fn cancel_attempt(&mut self, ob: &str, attempt: &Attempt, summary: &mut ReportSummary) {
        let mut next = self.writer.state().obligations.clone();
        if let Some(pending) = next.channels.get_mut(ob).and_then(|c| c.pending.as_mut()) {
            pending.attempts.retain(|a| a != attempt);
        }
        invalidate(&mut next, ob);
        self.commit_books_then(Some(next), None, false, |holder| {
            holder.prune_sent(std::slice::from_ref(attempt));
            summary.cancelled += 1;
        });
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
                self.record_refusal(crate::transition::Owed::ReportRejected {
                    ob: ob.into(),
                    key: pending.key.clone(),
                    code: response.status,
                    relist: self.relist_before_report,
                })
            }
            (409, Some("idempotency_conflict")) | (400 | 413, _) => {
                self.record_refusal(crate::transition::Owed::ReportHalt {
                    ob: ob.into(),
                    code: response.status,
                })
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
            return self.record_refusal(crate::transition::Owed::ReportHalt {
                ob: ob.into(),
                code: 0,
            });
        }
        Answer::Unverifiable
    }

    /// M10: `403` on any route. Nothing is servable, no traffic runs, and
    /// every queued unsent report is cancelled; attempts that may have been
    /// sent are kept as uncertain.
    pub(crate) fn on_not_authorized(&mut self) {
        self.route = RouteHealth::AuthHalt(AuthHalt::NotAuthorized);
        self.gate.set_auth_halt();
        self.events.emit(HolderEvent::HolderNotAuthorized);
        self.record_refusal(crate::transition::Owed::Cancel);
    }

    pub(crate) fn recover_cancellation(&mut self) -> Result<(), crate::holder::ConfigError> {
        use crate::holder::ConfigError::CancellationRecovery;
        let seal = self
            .writer
            .dir()
            .read(QUEUE_SEAL)
            .map_err(|_| CancellationRecovery)?;
        if seal.as_deref() != Some(self.writer.enrollment().holder_id.as_bytes()) {
            if self.record_refusal(crate::transition::Owed::Cancel) == Answer::BackOff {
                return Err(CancellationRecovery);
            }
            let id = self.writer.enrollment().holder_id.clone();
            self.writer
                .write_side_file(QUEUE_SEAL, id.as_bytes())
                .map_err(|failure| {
                    crate::holder::report_write_failure(
                        self.events.as_ref(),
                        &crate::state::WriteError::Io(failure.error),
                    );
                    CancellationRecovery
                })?;
        }
        Ok(())
    }

    /// Traffic stops immediately on 403, but cancellation is only observable
    /// as complete after the queue commit. Halted passes retry the write, not HTTP.
    pub(crate) fn traffic_halted(&mut self) -> bool {
        if self
            .writer
            .state()
            .status
            .owed
            .contains(&crate::transition::Owed::Cancel)
            && !self.fail_stopped()
        {
            self.apply_owed(crate::transition::Owed::Cancel);
        }
        self.route != RouteHealth::Normal
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
        self.commit_books_then(Some(next), None, false, |holder| {
            holder.unverifiable.remove(obligation_id);
            holder.events.emit(HolderEvent::ReportResumed);
        })
        .is_some()
    }

    /// Stores an ack's bytes under `name` in the holder directory.
    fn store_receipt(&mut self, name: &str, ack: &Ack) -> bool {
        self.writer
            .write_side_file(name, ack.compact.as_bytes())
            .is_ok()
    }

    fn on_ack(&mut self, ob: &str, pending: &Pending, ack: Ack, convergence: bool) -> Answer {
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
            return self.commit_ack(ob, &epoch, (next, None), &prune, None, Answer::Receipt);
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
            }
            let audit = (ack.rev.epoch != epoch || uncertain.epoch != epoch)
                .then_some(HolderEvent::AckOtherEpoch);
            return self.commit_ack(
                ob,
                &epoch,
                (next, None),
                &uncertain.attempts,
                audit,
                Answer::Receipt,
            );
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
        self.record_refusal(crate::transition::Owed::Converge(
            crate::transition::Convergence {
                ob: ob.into(),
                key: pending.key.clone(),
                receipt_jti: ack.jti,
                receipt: ack.compact,
                epoch: ack.rev.epoch,
                floor: ack.accepted_state,
            },
        ))
    }

    pub(crate) fn commit_convergence(
        &mut self,
        c: crate::transition::Convergence,
        mut next: ObligationBook,
        status: crate::status::StatusBook,
    ) -> Answer {
        let Some(epoch) = self.high_water_epoch() else {
            return Answer::BackOff;
        };
        if self
            .writer
            .write_side_file(&format!("conv.{}.jws", c.receipt_jti), c.receipt.as_bytes())
            .is_err()
        {
            self.events.emit(HolderEvent::StorageExhausted);
            return Answer::BackOff;
        }
        let ob = c.ob.as_str();
        let mut prune = Vec::new();
        if c.epoch == epoch {
            if let Some(channel) = next.channels.get_mut(ob) {
                channel.raise_floor(c.floor, &epoch);
                if let Some(record) = channel.pending.as_ref()
                    && record.body.state <= c.floor
                {
                    prune = record.attempts.clone();
                    channel.pending = None;
                }
            }
            prune_covered_uncertain(&mut next, ob, &epoch);
        } else {
            // R6e: audit only; no floor change; relist before re-deriving.
            if let Some(record) = next
                .channels
                .get_mut(ob)
                .and_then(|c| c.pending.as_mut())
                .filter(|p| p.key == c.key)
            {
                record.status = PendingStatus::Rejected;
            }
            self.relist_before_report = true;
        }
        let audit = if c.epoch == epoch {
            HolderEvent::ReportConverged
        } else {
            HolderEvent::AckOtherEpoch
        };
        self.commit_ack(
            ob,
            &epoch,
            (next, Some(status)),
            &prune,
            Some(audit),
            Answer::Converged,
        )
    }

    /// Reconfirmation, the receipt floor and consuming the key are one W.
    fn commit_ack(
        &mut self,
        ob: &str,
        epoch: &str,
        (mut next, status): (ObligationBook, Option<crate::status::StatusBook>),
        prune: &[Attempt],
        event: Option<HolderEvent>,
        answer: Answer,
    ) -> Answer {
        let mut complete = false;
        if let Some(channel) = next.channels.get_mut(ob) {
            complete = channel.floor_in(epoch) == Some(ReportState::Complete);
            if complete && channel.pending.is_none() {
                channel.reconfirm_pending = false;
            }
        }
        self.commit_books_then(Some(next), status, false, |holder| {
            if complete {
                holder.verified.remove(ob);
            }
            if let Some(event) = event {
                holder.events.emit(event);
            }
            holder.unverifiable.remove(ob);
            holder.prune_sent(prune);
            answer
        })
        .unwrap_or(Answer::BackOff)
    }

    /// v2 H.22, H.27 hook (S5 calls it): an epoch acceptance was consumed,
    /// so first sends wait for a status ack minted in `epoch`.
    pub fn arm_status_gate(&mut self, epoch: &str) -> bool {
        let mut next = self.writer.state().obligations.clone();
        next.status_gate.armed_epoch = Some(epoch.to_owned());
        next.status_gate.status_halted = self.writer.state().status.halted;
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
pub(crate) fn invalidate(book: &mut ObligationBook, ob: &str) {
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
