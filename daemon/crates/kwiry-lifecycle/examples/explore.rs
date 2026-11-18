//! Run one bounded profile and print the report (and the counterexample, if
//! any).
//!
//! `cargo run -p kwiry-lifecycle --example explore -- <design> key=value ...`
//!
//! `<design>`: `rev8`, `rev9`, or `rev9-<Fix>` (revision 9 without one fix,
//! by `Fix` name, e.g. `rev9-NarrowFirst`). Keys: `retires`, `changes`,
//! `crashes`, `faults`, `drops`, `restores` (budgets); `rr`, `cr` (room masks
//! for retirements and list changes); `families` (a `faults::` bitmask);
//! `garbage`, `same`, `liveness` (0/1); `skip` (`Prop::bit` mask); `max`
//! (state limit).
//!
//! Example: `cargo run -q --release -p kwiry-lifecycle --example explore --
//! rev9-NarrowFirst retires=1 rr=1 faults=1 families=32`

use kwiry_lifecycle::action::faults;
use kwiry_lifecycle::checker::{Profile, check};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::state::Budget;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let design = match args.first().map(String::as_str) {
        Some("rev8") => Design::REV8,
        Some("rev9") | None => Design::REV9,
        Some(other) => {
            let name = other
                .strip_prefix("rev9-")
                .expect("rev8, rev9 or rev9-<Fix>");
            let fix = Fix::ALL
                .into_iter()
                .find(|f| format!("{f:?}") == name)
                .expect("unknown fix name");
            Design::REV9.without(fix)
        }
    };
    let mut budget = Budget {
        retire_rooms: 0b11,
        change_rooms: 0b11,
        crashes: 0,
        faults: 0,
        changes: 0,
        retires: 1,
        drops: 0,
        restores: 0,
    };
    let mut families = faults::ALL;
    let mut liveness = true;
    let mut max = 20_000_000usize;
    let mut garbage = true;
    let mut skip = 0u32;
    let mut same_state = false;
    for kv in args.iter().skip(1) {
        let (k, v) = kv.split_once('=').expect("key=value");
        let n: usize = v.parse().expect("number");
        let small = || u8::try_from(n).expect("a small number");
        match k {
            "crashes" => budget.crashes = small(),
            "faults" => budget.faults = small(),
            "changes" => budget.changes = small(),
            "retires" => budget.retires = small(),
            "drops" => budget.drops = small(),
            "restores" => budget.restores = small(),
            "rr" => budget.retire_rooms = small(),
            "cr" => budget.change_rooms = small(),
            "families" => families = u16::try_from(n).expect("a fault mask"),
            "liveness" => liveness = n != 0,
            "max" => max = n,
            "garbage" => garbage = n != 0,
            "skip" => skip = u32::try_from(n).expect("a property mask"),
            "same" => same_state = n != 0,
            _ => panic!("unknown key {k}"),
        }
    }
    let report = check(&Profile {
        name: "explore",
        design,
        budget,
        faults: families,
        garbage,
        same_state,
        liveness,
        skip,
        max_states: max,
    });
    print!("{}", report.render());
    if let Some(c) = &report.violation {
        let rows: Vec<_> = c.rows().iter().map(|r| r.name()).collect();
        println!("rows: {}", rows.join(" "));
    }
}
