//! The durable intake boundary shared by the two outbound channels.
use crate::client::{FetchFailure, HttpResponse, Transport, TransportError, problem_code};
use crate::events::HolderEvent;
use crate::holder::Holder;
use crate::obligations::{ObligationBook, PendingStatus};
use crate::report::{Answer, invalidate};
use crate::status::{StatusBook, cancel_status_on_403};
use crate::store::ResourceStore;
use serde::{Deserialize, Serialize};

/// Only constructed after validating the signed current ack and its binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Convergence {
    pub ob: String,
    pub key: String,
    pub receipt_jti: String,
    pub receipt: String,
    pub epoch: String,
    pub floor: crate::wire::ReportState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum Owed {
    StatusHalt {
        code: u16,
        // Keyless debts on older disks (and ack-strike halts) remain readable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    StatusStale {
        key: String,
    },
    ReportHalt {
        ob: String,
        code: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    ReportRejected {
        ob: String,
        key: String,
        code: u16,
        relist: bool,
    },
    Converge(Convergence),
    Cancel,
}

impl Owed {
    pub(crate) fn for_status(&self) -> bool {
        matches!(self, Self::StatusHalt { .. } | Self::StatusStale { .. })
    }
    pub(crate) fn for_report(&self) -> bool {
        matches!(
            self,
            Self::ReportHalt { .. } | Self::ReportRejected { .. } | Self::Converge(_)
        )
    }
    pub(crate) fn blocks_report(&self, id: &str) -> bool {
        match self {
            Self::ReportHalt { ob, .. } | Self::ReportRejected { ob, .. } => ob == id,
            Self::Converge(c) => c.ob == id,
            Self::Cancel => true,
            _ => false,
        }
    }
}

pub(crate) enum ReceiptFailure {
    Unverifiable,
    PinWrite,
    RefreshDeferred,
    Refused(Answer),
}

impl<S: ResourceStore, T: Transport> Holder<S, T> {
    fn apply_authority_refusal(&mut self, refusal: crate::client::AuthorityRefusal) -> Answer {
        use crate::client::{AuthorityRefusal, FetchFailure};
        match refusal {
            AuthorityRefusal::Replay => Answer::RetryReplay,
            AuthorityRefusal::Expired => Answer::RetryExpired,
            AuthorityRefusal::Stop(failure) => {
                self.apply_failure(&failure);
                if matches!(
                    failure,
                    FetchFailure::NotAuthorized | FetchFailure::Unauthorized
                ) {
                    Answer::AuthHalted
                } else {
                    Answer::BackOff
                }
            }
        }
    }

    /// Narrow new release authority before any refusal write. The request's
    /// own list reference (also retained by its delivery handle) still binds
    /// the refusal even if the business key has already left durable state.
    pub(crate) fn observe_list_refusal(
        &mut self,
        cited: &crate::wire::ListRef,
        response: &HttpResponse,
    ) {
        if response.status == 409
            && self.applied.as_ref().is_some_and(|a| &a.list_ref == cited)
            && problem_code(response).as_deref() == Some("stale_list_ref")
        {
            self.release_list_rejected = true;
        }
    }

    /// Same intake for a synchronous POST and a delivery on either channel.
    /// A timeout does not end a run of proof refusals: the next HTTP answer
    /// may still arrive through the dispatch's retained delivery handle.
    pub(crate) fn classify_post(
        &mut self,
        origin: crate::client::Origin,
        key: &str,
        response: Result<HttpResponse, TransportError>,
        route_answer: impl FnOnce(&mut Self, HttpResponse) -> Answer,
    ) -> Answer {
        let response = match response {
            Ok(response) => response,
            Err(TransportError::Unreachable) => {
                self.apply_failure(&FetchFailure::Unreachable {
                    retry_after_s: None,
                });
                return Answer::BackOff;
            }
            Err(TransportError::Timeout) => return Answer::BackOff,
        };
        let answer = match crate::client::authority_refusal(&response) {
            Some(refusal) => self.apply_authority_refusal(refusal),
            None => route_answer(self, response),
        };
        // A late refusal from a superseded key has no proof to retry. It must
        // not overwrite the current key's retry state on the same channel.
        let current_key = match &origin {
            crate::client::Origin::Status => self
                .writer
                .state()
                .status
                .pending
                .as_ref()
                .filter(|p| p.live())
                .map(|p| &p.key),
            crate::client::Origin::Report(ob) => self
                .writer
                .state()
                .obligations
                .channels
                .get(ob)
                .and_then(|c| c.pending.as_ref())
                .filter(|p| p.live())
                .map(|p| &p.key),
        };
        if current_key.is_some_and(|k| k != key) {
            return answer;
        }
        let previous = self
            .proof_retry
            .get(&origin)
            .filter(|(k, _)| k == key)
            .map(|(_, a)| *a);
        if matches!(answer, Answer::RetryExpired | Answer::RetryReplay) {
            if previous == Some(answer) {
                self.proof_retry.remove(&origin);
                if answer == Answer::RetryExpired {
                    self.events.emit(HolderEvent::ProofExpiredClockAlert);
                }
                return Answer::BackOff;
            }
            self.proof_retry.insert(origin, (key.to_owned(), answer));
        } else if previous.is_some() {
            self.proof_retry.remove(&origin);
        }
        answer
    }
    pub(crate) fn verify_with_refresh<A>(
        &mut self,
        compact: &str,
        verify: fn(&Self, &str) -> Result<A, &'static str>,
        back_off: fn(&mut Self, Option<u64>) -> u64,
    ) -> Result<A, ReceiptFailure> {
        use crate::client::KeysetFetchError;
        use crate::holder::RouteHealth;
        use ReceiptFailure::*;

        match verify(self, compact) {
            Err("kid") => {
                if self.clock.mono_ms() < self.ack_keyset_backoff_until {
                    return Err(RefreshDeferred);
                }
                let enrollment = self.writer.enrollment();
                let profile = &self.config.profile;
                let statement = match crate::client::fetch_route_statement(
                    &self.transport,
                    &enrollment.issuer,
                    profile,
                    || {
                        (self.route == RouteHealth::Normal && !self.fail_stopped())
                            .then(|| self.inbox.admit(self.inbox.ticket()))
                            .flatten()
                    },
                ) {
                    Ok(statement) => statement,
                    Err(KeysetFetchError::DispatchBlocked) => return Err(RefreshDeferred),
                    Err(KeysetFetchError::Timeout) => {
                        self.ack_keyset_backoff_until = back_off(self, None);
                        return Err(RefreshDeferred);
                    }
                    Err(KeysetFetchError::RetryableHttp { retry_after_s }) => {
                        self.ack_keyset_backoff_until = back_off(self, retry_after_s);
                        return Err(RefreshDeferred);
                    }
                    Err(KeysetFetchError::Refusal(r)) => {
                        return Err(Refused(self.apply_authority_refusal(r)));
                    }
                    Err(KeysetFetchError::Invalid) => return Err(Unverifiable),
                };
                let pins = crate::keys::verify_keyset(
                    &statement,
                    &enrollment.pinned,
                    profile,
                    &enrollment.issuer,
                    self.clock.wall_ms(),
                )
                .map_err(|_| Unverifiable)?;
                let seq = pins.keyset_seq;
                self.commit_then(
                    |writer| writer.pin_keys(pins),
                    |holder| {
                        if let Some(keyset_seq) = seq {
                            holder
                                .events
                                .emit(HolderEvent::KeysetAccepted { keyset_seq });
                        }
                    },
                )
                .ok_or(PinWrite)?;
                // H.6: fetch once; a kid still unknown after refresh is a strike.
                verify(self, compact).map_err(|_| Unverifiable)
            }
            result => result.map_err(|_| Unverifiable),
        }
    }

    pub(crate) fn receipt_failure(
        &mut self,
        failure: ReceiptFailure,
        strike: impl FnOnce(&mut Self) -> Answer,
    ) -> Answer {
        match failure {
            ReceiptFailure::Unverifiable => strike(self),
            ReceiptFailure::PinWrite | ReceiptFailure::RefreshDeferred => Answer::RetryReceipt,
            ReceiptFailure::Refused(answer) => answer,
        }
    }

    pub(crate) fn drain_deliveries(
        &mut self,
    ) -> (
        bool,
        crate::status::StatusSummary,
        crate::report::ReportSummary,
    ) {
        let batch = std::mem::take(&mut self.inbox.lock().deliveries);
        self.intake(batch)
    }

    /// Refusals narrow immediately on their originating route. Signed answers
    /// are deferred to their guarded channel pass: intake never fetches keys.
    pub(crate) fn intake(
        &mut self,
        mut batch: Vec<crate::client::Delivery>,
    ) -> (
        bool,
        crate::status::StatusSummary,
        crate::report::ReportSummary,
    ) {
        use crate::client::{AuthorityRefusal, Origin, authority_refusal};
        use crate::holder::{AuthHalt, RouteHealth};
        batch.sort_by_cached_key(|(_, _, _, r)| match authority_refusal(r) {
            Some(AuthorityRefusal::Stop(FetchFailure::NotAuthorized)) => 0,
            Some(AuthorityRefusal::Stop(
                FetchFailure::LifecycleUnavailable { .. }
                | FetchFailure::RollbackSuspected
                | FetchFailure::Unreachable { .. },
            )) => 1,
            Some(_) => 2,
            None => 3,
        });
        let mut refusal = false;
        let mut status = crate::status::StatusSummary::default();
        let mut report = crate::report::ReportSummary::default();
        for (origin, key, cited, response) in batch {
            refusal |= response.status != 200;
            self.observe_list_refusal(&cited, &response);
            if response.status == 401
                && self.route == RouteHealth::AuthHalt(AuthHalt::NotAuthorized)
            {
                continue;
            }
            match origin {
                Origin::Status if response.status == 200 => {
                    self.late_acks.push_back((key, response))
                }
                Origin::Report(ob)
                    if response.status == 200
                        || (response.status == 409
                            && problem_code(&response).as_deref() == Some("state_regression")) =>
                {
                    self.late_reports.push_back((ob, key, response))
                }
                Origin::Report(ob) => report.record(self.delayed_report(&ob, &key, response)),
                Origin::Status => {
                    let answer = self.classify_status(&key, Ok(response));
                    status.record(answer);
                    if answer == Answer::BackOff
                        && self.clock.mono_ms() >= self.status_backoff_until
                    {
                        self.status_back_off(None);
                    }
                }
            }
        }
        (refusal, status, report)
    }

    pub(crate) fn commit_books_then<R>(
        &mut self,
        book: Option<ObligationBook>,
        status: Option<StatusBook>,
        clear_reset: bool,
        after: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        self.commit_then(
            |writer| writer.update_lifecycle(book, status, clear_reset),
            after,
        )
    }

    /// Commit-then-emit: a durable transition's success (event, count, return
    /// value, pruning evidence) is observable only in or downstream of `after`,
    /// which cannot run unless the commit returned `Ok`. Storage and authority
    /// failure alerts describe observations, not successful writes. Waiting and
    /// actual sends are transport observations, not durable transitions either.
    pub(crate) fn commit_then<R>(
        &mut self,
        write: impl FnOnce(&mut crate::state::StateWriter) -> Result<(), crate::state::WriteError>,
        after: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        match write(&mut self.writer) {
            Ok(()) => Some(after(self)),
            Err(error) => {
                crate::holder::report_write_failure(self.events.as_ref(), &error);
                None
            }
        }
    }

    /// Once durably received, a refusal is retried as a *write*, never as a
    /// POST. The completion W consumes the debt atomically with its effect.
    /// If intake itself cannot commit, fail-stop. After restart the disk then
    /// describes a possibly-sent request, exactly as for a lost response:
    /// H.50 applies and any retry uses the same key and identical business
    /// claims (fresh `jti`/`iat`/`exp`, §4.4). No refusal is fabricated.
    pub(crate) fn record_refusal(&mut self, owed: Owed) -> Answer {
        let mut status = self.writer.state().status.clone();
        if !status.owed.contains(&owed) {
            status.owed.push(owed.clone());
            if !self.commit_status(status) {
                self.writer.mark_unwritable();
                return Answer::BackOff;
            }
        }
        self.apply_owed(owed)
    }

    pub(crate) fn apply_owed(&mut self, owed: Owed) -> Answer {
        let mut status = self.writer.state().status.clone();
        let mut book = self.writer.state().obligations.clone();
        let mut prune = Vec::new();
        let mut relist = false;
        let Some(index) = status.owed.iter().position(|o| o == &owed) else {
            return Answer::BackOff;
        };
        status.owed.remove(index);
        let (answer, event) = match owed {
            Owed::StatusHalt { code, key } => {
                if code == 400
                    && let Some(key) = key
                    && let std::collections::btree_map::Entry::Occupied(entry) =
                        status.uncertain.entry(key)
                    && entry.get().before_resume
                {
                    // A delayed 400 for a pre-resume key answers only the old request.
                    prune.extend(entry.remove().attempts);
                    (
                        Answer::Rejected,
                        Some(HolderEvent::StatusRejected { status: code }),
                    )
                } else {
                    status.halted = true;
                    book.status_gate.status_halted = true;
                    (
                        Answer::Halted,
                        Some(HolderEvent::StatusBugHalt { status: code }),
                    )
                }
            }
            Owed::StatusStale { key } => {
                relist = true;
                if let Some(record) = status.pending.as_mut().filter(|p| p.key == key) {
                    prune.append(&mut record.attempts);
                    record.status = PendingStatus::Rejected;
                }
                if let Some(u) = status.uncertain.remove(&key) {
                    prune.extend(u.attempts);
                }
                (
                    Answer::Rejected,
                    Some(HolderEvent::StatusRejected { status: 409 }),
                )
            }
            Owed::ReportHalt { ob, code, key } => {
                if code == 400
                    && let Some(key) = key
                    && let std::collections::btree_map::Entry::Occupied(entry) =
                        book.uncertain.entry(key)
                    && entry.get().obligation_id == ob
                    && entry.get().before_resume
                {
                    prune.extend(entry.remove().attempts);
                    (
                        Answer::Rejected,
                        Some(HolderEvent::ReportRejected { status: code }),
                    )
                } else {
                    if let Some(channel) = book.channels.get_mut(&ob) {
                        channel.halted = true;
                    }
                    (
                        Answer::Halted,
                        Some(HolderEvent::ReportBugHalt { status: code }),
                    )
                }
            }
            Owed::ReportRejected {
                ob,
                key,
                code,
                relist: needs_list,
            } => {
                relist = needs_list;
                if let Some(record) = book
                    .channels
                    .get_mut(&ob)
                    .and_then(|c| c.pending.as_mut())
                    .filter(|p| p.key == key)
                {
                    prune.append(&mut record.attempts);
                    record.status = PendingStatus::Rejected;
                }
                if let Some(u) = book.uncertain.remove(&key) {
                    prune.extend(u.attempts);
                }
                (
                    Answer::Rejected,
                    Some(HolderEvent::ReportRejected { status: code }),
                )
            }
            Owed::Converge(c) => return self.commit_convergence(c, book, status),
            Owed::Cancel => {
                let mut count = usize::from(cancel_status_on_403(&mut status));
                for ob in book.channels.keys().cloned().collect::<Vec<_>>() {
                    let channel = book.channels.get_mut(&ob).expect("key from this map");
                    if let Some(p) = channel.pending.as_ref().filter(|p| p.live()) {
                        count += 1;
                        if p.attempts.is_empty() {
                            channel.pending = None;
                        } else {
                            invalidate(&mut book, &ob);
                        }
                    }
                }
                (
                    Answer::AuthHalted,
                    (count > 0).then_some(HolderEvent::ReportsCancelled { count }),
                )
            }
        };
        for attempt in &prune {
            status.manifests.remove(&attempt.digest);
        }
        self.commit_books_then(Some(book), Some(status), false, |holder| {
            holder.relist_before_report |= relist;
            holder.prune_sent_attempts(&prune);
            if let Some(event) = event {
                holder.events.emit(event);
            }
            answer
        })
        .unwrap_or(Answer::BackOff)
    }
}
