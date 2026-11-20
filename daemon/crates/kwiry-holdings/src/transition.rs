//! The durable intake boundary shared by the two outbound channels.
use crate::client::Transport;
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
    },
    StatusStale {
        key: String,
    },
    ReportHalt {
        ob: String,
        code: u16,
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

impl<S: ResourceStore, T: Transport> Holder<S, T> {
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
            Owed::StatusHalt { code } => {
                status.halted = true;
                book.status_gate.status_halted = true;
                (
                    Answer::Halted,
                    Some(HolderEvent::StatusBugHalt { status: code }),
                )
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
            Owed::ReportHalt { ob, code } => {
                if let Some(channel) = book.channels.get_mut(&ob) {
                    channel.halted = true;
                }
                (
                    Answer::Halted,
                    Some(HolderEvent::ReportBugHalt { status: code }),
                )
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
