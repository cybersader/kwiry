//! `clock_behind_authority`: raised in memory then written, cleared by
//! writing first, decided by `(iat − now_recv) + rtt ≤ 30 s` alone.

use super::*;
use crate::verify::PageError;

fn raised(rig: &Rig) -> bool {
    rig.engine_gate().clock_hold()
}

fn durable(rig: &mut Rig) -> bool {
    rig.holder().state().engine.clock_behind_authority
}

/// Lists once with the authority 45 s ahead, raising the hold.
fn raise(rig: &mut Rig) {
    rig.authority.state().offset_ms = 45_000;
    applied(rig.relist());
    assert!(raised(rig));
    rig.authority.state().offset_ms = 0;
}

fn setup() -> (tempfile::TempDir, Rig) {
    let (dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    applied(rig.holder().list_once());
    (dir, rig)
}

#[test]
fn c19_h26_a_page_ahead_raises_the_hold_which_survives_restart() {
    let (_dir, mut rig) = setup();
    assert!(!raised(&rig));
    raise(&mut rig);
    assert!(durable(&mut rig));
    assert!(codes(&rig).contains(&"clock_behind_authority"));
    let now = now_s(&rig);
    assert_eq!(
        with_capability(&rig, "res-a", now, now + 60, Cut::Admission),
        Err(Refusal::Engine(EngineRefusal::ClockHold))
    );
    assert_eq!(
        with_capability(&rig, "res-a", now, now + 60, Cut::Release),
        Err(Refusal::Engine(EngineRefusal::ClockHold))
    );
    assert_eq!(plain(&rig, "res-a"), Ok(()), "lists and applies continue");
    rig.restart().unwrap();
    assert!(raised(&rig), "a restart is not a clock correction");
}

#[test]
fn h21_5_a_signed_page_far_ahead_raises_the_hold_even_when_it_fails_the_window() {
    let (_dir, mut rig) = setup();
    rig.authority.state().hooks.page = Some(Box::new(|page| {
        let iat = page["iat"].as_i64().unwrap();
        page.insert("iat".into(), (iat + 121).into());
    }));
    assert_eq!(
        failed(rig.relist()),
        crate::client::FetchFailure::Invalid(PageError::Time)
    );
    assert!(raised(&rig));
}

#[test]
fn c20_h34_clearing_writes_first_and_a_failed_clear_stays_raised() {
    let (_dir, mut rig) = setup();
    raise(&mut rig);
    // The clearing write is the first write of the next list.
    rig.io_faults.fail_writes(1);
    applied(rig.relist());
    assert!(raised(&rig));
    assert!(durable(&mut rig));
    assert!(codes(&rig).contains(&"clock_hold_write_failed"));
    // The next confirmed correction retries.
    applied(rig.relist());
    assert!(!raised(&rig));
    assert!(!durable(&mut rig));
    assert!(codes(&rig).contains(&"clock_behind_authority_cleared"));
    let now = now_s(&rig);
    assert_eq!(
        with_capability(&rig, "res-a", now, now + 60, Cut::Admission),
        Ok(())
    );
}

#[test]
fn h29_the_inequality_alone_decides_whatever_the_round_trip() {
    // Engine 40 s ahead on a 60 s round trip: (−70) + 60 = −10 ≤ 30.
    let (_dir, mut rig) = setup();
    raise(&mut rig);
    {
        let mut authority = rig.authority.state();
        authority.offset_ms = -40_000;
        authority.latency_ms = 60_000;
    }
    applied(rig.relist());
    assert!(!raised(&rig));

    // Boundary: (20 − 10) + 20 = 30 clears; (20 − 11) + 22 = 31 does not.
    for (latency, clears) in [(20_000, true), (22_000, false)] {
        let (_dir, mut rig) = setup();
        raise(&mut rig);
        {
            let mut authority = rig.authority.state();
            authority.offset_ms = 20_000;
            authority.latency_ms = latency;
        }
        applied(rig.relist());
        assert_eq!(!raised(&rig), clears, "{latency}");
        if !clears {
            assert!(codes(&rig).contains(&"clock_behind_authority_unconfirmed"));
        }
    }
}

#[test]
fn h23_nothing_but_a_confirmed_correction_clears_and_persistence_never_locks_out() {
    let (_dir, mut rig) = setup();
    raise(&mut rig);
    let now = now_s(&rig);
    rig.holder().observe_capability_iat(now);
    rig.clock.step_wall(-5_000);
    rig.advance(3_600_000);
    rig.restart().unwrap();
    assert!(raised(&rig));
    // Every start lists at once; agreeing clocks clear on the first page.
    applied(rig.relist());
    assert!(!raised(&rig));
}

#[test]
fn c19_a_capability_dated_more_than_30_s_ahead_raises_the_hold() {
    let (_dir, mut rig) = setup();
    let now_ms = {
        use crate::clock::Clock;
        rig.clock.wall_ms()
    };
    rig.holder().observe_capability_iat(now_ms / 1000 + 30);
    assert!(!raised(&rig));
    rig.holder().observe_capability_iat(now_ms / 1000 + 31);
    assert!(raised(&rig));
    assert!(durable(&mut rig));
}

#[test]
fn h26_a_failed_raising_write_keeps_the_refusal_in_memory_only() {
    let (_dir, mut rig) = setup();
    rig.authority.state().offset_ms = 45_000;
    rig.io_faults.fail_writes(1);
    applied(rig.relist());
    assert!(raised(&rig));
    assert!(!durable(&mut rig));
    assert!(codes(&rig).contains(&"clock_hold_write_failed"));
    // A crash here loses only the flag: nothing serves until a list of the
    // next process applies, and that list measures the clock again.
    rig.restart().unwrap();
    assert!(!raised(&rig));
    assert_eq!(plain(&rig, "res-a"), Err(Refusal::NoListThisProcess));
    applied(rig.relist());
    assert!(raised(&rig));
}

#[test]
fn c21_probes_run_only_while_raised_at_most_every_60_s_and_are_not_applied() {
    let (_dir, mut rig) = setup();
    rig.advance(5_000);
    assert!(rig.holder().probe_clock().is_none(), "no hold, no probe");
    rig.authority.state().offset_ms = 45_000;
    applied(rig.relist());
    rig.advance(5_000);
    let published = rig.store.view().published.len();
    assert_eq!(rig.holder().probe_clock(), Some(Ok(())));
    assert!(raised(&rig), "probe page still 45 s ahead");
    let last = rig.authority.state().list_claims().pop().unwrap();
    assert_eq!(last["page_size"], 1);
    assert_eq!(rig.store.view().published.len(), published, "never applied");
    rig.advance(30_000);
    assert!(rig.holder().probe_clock().is_none(), "within 60 s");
    rig.authority.state().offset_ms = 0;
    rig.advance(30_000);
    assert_eq!(rig.holder().probe_clock(), Some(Ok(())));
    assert!(!raised(&rig));
    assert_eq!(rig.store.view().published.len(), published);
}
