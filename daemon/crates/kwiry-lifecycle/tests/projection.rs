//! The checker explores the quotient [`step::canonical`]. That is sound only
//! if a state and its projection have the same future. This test checks the
//! bisimulation condition on unprojected reachable states: for every action,
//! the state and its projection agree on enabledness, the rows cited, the
//! observations, every safety property, the goal and settledness, and their
//! successors project to the same state.
//!
//! States come from a breadth-first prefix and from seeded random walks deep
//! into each profile, because the receipts and floor keys the projection
//! erases only accumulate late in a run.

use kwiry_lifecycle::action::{Action, faults};
use kwiry_lifecycle::props;
use kwiry_lifecycle::state::*;
use kwiry_lifecycle::step::{Config, Outcome, canonical, exec};

const R0: u8 = 0b01;
const R1: u8 = 0b10;

fn budget() -> Budget {
    Budget {
        retire_rooms: R0,
        change_rooms: R1,
        crashes: 0,
        faults: 0,
        changes: 1,
        retires: 1,
        drops: 0,
        restores: 0,
    }
}

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n
    }
}

fn successors(s: &State, cfg: &Config, cands: &[Action]) -> Vec<(Action, Outcome)> {
    cands
        .iter()
        .filter_map(|&a| exec(s, a, cfg).ok().flatten().map(|o| (a, o)))
        .collect()
}

/// The bisimulation condition at `s`. Returns whether the projection erased
/// anything there, and whether it erased authority receipts.
fn agree(s: &State, cfg: &Config, cands: &[Action]) -> (bool, bool) {
    let mut c = s.clone();
    canonical(&mut c, cfg);
    assert_eq!(props::check_state(s, 0), props::check_state(&c, 0));
    assert_eq!(props::goal(s), props::goal(&c));
    assert_eq!(props::settled(s), props::settled(&c));
    for &a in cands {
        let (x, y) = (exec(s, a, cfg), exec(&c, a, cfg));
        match (x, y) {
            (Ok(None), Ok(None)) => {}
            (Err(e), Err(f)) => assert_eq!(e, f, "{a:?}"),
            (Ok(Some(x)), Ok(Some(y))) => {
                assert_eq!(x.rows, y.rows, "{a:?}");
                assert_eq!(x.obs, y.obs, "{a:?}");
                assert_eq!(
                    props::check_step(s, &x, 0),
                    props::check_step(&c, &y, 0),
                    "{a:?}"
                );
                let (mut nx, mut ny) = (x.next, y.next);
                canonical(&mut nx, cfg);
                canonical(&mut ny, cfg);
                assert!(nx == ny, "{a:?}: successors differ after projection");
            }
            (x, y) => panic!(
                "{a:?}: enabledness differs: {:?} vs {:?}",
                x.map(|o| o.is_some()),
                y.map(|o| o.is_some())
            ),
        }
    }
    let receipts = |x: &State| {
        let core = &x.auth.core;
        (
            core.status_receipts,
            core.rooms.map(|r| r.obl.map(|o| o.receipts)),
        )
    };
    (c != *s, receipts(&c) != receipts(s))
}

fn check_profile(name: &str, budget: Budget, fam: u16, same_state: bool, seed: u64) {
    let cfg = Config {
        design: kwiry_lifecycle::design::Design::REV11_3,
        faults: fam,
        same_state,
    };
    let cands = Action::candidates();
    let init = State::initial(budget, false);
    let (mut checked, mut erased, mut receipts) = (0usize, 0usize, 0usize);
    let mut tally = |(e, r): (bool, bool)| {
        checked += 1;
        erased += usize::from(e);
        receipts += usize::from(r);
    };
    // Breadth-first prefix.
    let mut seen = std::collections::HashSet::new();
    let mut layer = vec![init.clone()];
    seen.insert(init.clone());
    while !layer.is_empty() && seen.len() < 10_000 {
        let mut next = Vec::new();
        for s in &layer {
            tally(agree(s, &cfg, &cands));
            for (_, o) in successors(s, &cfg, &cands) {
                if seen.insert(o.next.clone()) {
                    next.push(o.next);
                }
            }
        }
        layer = next;
    }
    // Deep random walks.
    let mut rng = Rng(seed);
    for _ in 0..300 {
        let mut s = init.clone();
        for _ in 0..200 {
            let succ = successors(&s, &cfg, &cands);
            if succ.is_empty() {
                break;
            }
            tally(agree(&s, &cfg, &cands));
            s = succ[rng.below(succ.len())].1.next.clone();
        }
    }
    eprintln!(
        "{name}: {checked} states checked; the projection changed {erased}, \
         {receipts} of them by pruning receipts"
    );
    assert!(
        erased > checked / 20,
        "{name}: the projection was barely exercised"
    );
    assert!(receipts > 0, "{name}: no receipt was pruned");
}

#[test]
fn projection_is_a_bisimulation_on_reachable_states() {
    check_profile("release", budget(), 0, false, 1);
    check_profile(
        "release-delete-fail",
        Budget {
            faults: 1,
            ..budget()
        },
        faults::DELETE_FAIL,
        false,
        2,
    );
    check_profile(
        "crash-fault-restore",
        Budget {
            crashes: 1,
            faults: 1,
            restores: 1,
            ..budget()
        },
        faults::ALL,
        false,
        3,
    );
    check_profile("same-state", budget(), 0, true, 4);
}
