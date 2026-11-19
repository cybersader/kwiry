//! When to list: periodic relists, early relists, backoff and the list-start
//! rate. Pure bookkeeping over monotonic milliseconds; the driver loop owns
//! the timers.
//!
//! - Counted list starts are at least 5 s apart. A restart after `410` and
//!   the single retry after `401 replay`/`proof_expired` happen inside one
//!   fetch and are not counted.
//! - Each periodic interval is drawn from `[0.8, 1.0] × relist_seconds`.
//! - An early relist (a capability naming an unlisted, untombstoned
//!   resource) never makes the request wait; at most one is pending and
//!   further triggers coalesce.
//! - Failures back off from 10 s, doubling, capped at `relist_seconds / 2`,
//!   and never earlier than a `Retry-After`. A list-route `400`/`413` caps
//!   at `relist_seconds` instead (v2 H.40).
//! - A clock probe runs at most once per 60 s and counts as a list start.

pub const LIST_START_SPACING_MS: u64 = 5_000;
pub const PROBE_SPACING_MS: u64 = 60_000;
pub const BACKOFF_START_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EarlyRelist {
    /// A relist is now pending; it may start at this instant.
    Scheduled { at_mono_ms: u64 },
    /// One is already pending or in flight.
    Coalesced,
}

#[derive(Clone, Debug)]
pub struct RelistScheduler {
    relist_ms: u64,
    last_counted_start: Option<u64>,
    next_periodic: u64,
    early_pending: bool,
    in_flight: bool,
    backoff_ms: u64,
    backoff_until: u64,
    last_probe: Option<u64>,
}

impl RelistScheduler {
    /// Startup lists at once.
    pub fn new(relist_seconds: u64) -> Self {
        Self {
            relist_ms: relist_seconds.saturating_mul(1000).max(1),
            last_counted_start: None,
            next_periodic: 0,
            early_pending: false,
            in_flight: false,
            backoff_ms: 0,
            backoff_until: 0,
            last_probe: None,
        }
    }

    /// Adopts the `relist_seconds` of the last applied list.
    pub fn set_relist_seconds(&mut self, relist_seconds: u64) {
        self.relist_ms = relist_seconds.saturating_mul(1000).max(1);
    }

    /// The earliest instant the next counted list start may happen.
    pub fn earliest_start(&self) -> u64 {
        self.last_counted_start
            .map_or(0, |last| last.saturating_add(LIST_START_SPACING_MS))
    }

    pub fn may_start(&self, now: u64) -> bool {
        !self.in_flight && now >= self.earliest_start()
    }

    /// Whether the driver should start a list now.
    pub fn due(&self, now: u64) -> bool {
        self.may_start(now)
            && now >= self.backoff_until
            && (self.early_pending || now >= self.next_periodic)
    }

    pub fn request_early(&mut self, now: u64) -> EarlyRelist {
        if self.early_pending || self.in_flight {
            return EarlyRelist::Coalesced;
        }
        self.early_pending = true;
        EarlyRelist::Scheduled {
            at_mono_ms: now.max(self.earliest_start()).max(self.backoff_until),
        }
    }

    pub fn on_start(&mut self, now: u64) {
        self.in_flight = true;
        self.early_pending = false;
        self.last_counted_start = Some(now);
    }

    /// `jitter` is any random value; it picks the interval inside
    /// `[0.8, 1.0] × relist_seconds`.
    pub fn on_success(&mut self, now: u64, jitter: u64) {
        self.in_flight = false;
        self.backoff_ms = 0;
        self.backoff_until = 0;
        let spread = self.relist_ms / 5;
        let interval = self.relist_ms - jitter % (spread + 1);
        self.next_periodic = now.saturating_add(interval);
    }

    pub fn on_failure(&mut self, now: u64, retry_after_ms: Option<u64>) {
        self.back_off(
            now,
            retry_after_ms,
            (self.relist_ms / 2).max(BACKOFF_START_MS),
        );
    }

    /// v2 H.40: a list-route `400`/`413` backs off from 10 s, doubling,
    /// capped at `relist_seconds`; every retry is a counted list start.
    pub fn on_request_invalid(&mut self, now: u64) {
        self.back_off(now, None, self.relist_ms.max(BACKOFF_START_MS));
    }

    /// The instant before which no list may start after the last failure.
    pub fn backoff_until(&self) -> u64 {
        self.backoff_until
    }

    /// The earliest instant any list may start: after the list-start
    /// spacing and after the current backoff (v2 H.8, H.40). Only the
    /// clock probe has its own spacing instead.
    pub fn next_start_at(&self) -> u64 {
        self.earliest_start().max(self.backoff_until)
    }

    fn back_off(&mut self, now: u64, retry_after_ms: Option<u64>, cap: u64) {
        self.in_flight = false;
        self.backoff_ms = if self.backoff_ms == 0 {
            BACKOFF_START_MS
        } else {
            self.backoff_ms.saturating_mul(2)
        }
        .min(cap);
        let wait = self.backoff_ms.max(retry_after_ms.unwrap_or(0));
        self.backoff_until = now.saturating_add(wait);
    }

    pub fn probe_due(&self, now: u64) -> bool {
        self.may_start(now)
            && self
                .last_probe
                .is_none_or(|last| now >= last.saturating_add(PROBE_SPACING_MS))
    }

    pub fn on_probe(&mut self, now: u64) {
        self.last_probe = Some(now);
        self.last_counted_start = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_starts_are_spaced_and_early_relists_coalesce() {
        let mut s = RelistScheduler::new(300);
        assert!(s.due(0));
        s.on_start(0);
        assert_eq!(s.request_early(1_000), EarlyRelist::Coalesced);
        s.on_success(2_000, 0);
        assert!(!s.due(2_000));
        assert_eq!(
            s.request_early(2_000),
            EarlyRelist::Scheduled { at_mono_ms: 5_000 }
        );
        assert_eq!(s.request_early(2_500), EarlyRelist::Coalesced);
        assert!(!s.due(4_999));
        assert!(s.due(5_000));
    }

    #[test]
    fn periodic_interval_stays_inside_the_jitter_band() {
        for jitter in [0, 1, 59_999, 60_000, 60_001, u64::MAX] {
            let mut s = RelistScheduler::new(300);
            s.on_start(0);
            s.on_success(0, jitter);
            assert!(!s.due(239_999), "{jitter}");
            assert!(s.due(300_000), "{jitter}");
        }
    }

    #[test]
    fn backoff_doubles_to_half_the_relist_interval_and_honours_retry_after() {
        let mut s = RelistScheduler::new(300);
        let mut waits = Vec::new();
        for _ in 0..6 {
            s.on_start(0);
            s.on_failure(0, None);
            waits.push(s.backoff_until);
        }
        assert_eq!(waits, [10_000, 20_000, 40_000, 80_000, 150_000, 150_000]);
        s.on_start(0);
        s.on_failure(0, Some(400_000));
        assert_eq!(s.backoff_until, 400_000);
    }

    #[test]
    fn probes_are_spaced_by_sixty_seconds_and_count_as_starts() {
        let mut s = RelistScheduler::new(300);
        assert!(s.probe_due(0));
        s.on_probe(0);
        assert!(!s.may_start(4_999));
        assert!(!s.probe_due(59_999));
        assert!(s.probe_due(60_000));
    }
}
