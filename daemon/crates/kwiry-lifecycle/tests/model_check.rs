//! The lifecycle model check.
//!
//! - `rev10_holds`: every CI profile of revision 11.1 explores its bounded
//!   quotient exhaustively (`step::canonical`; the reductions are in the
//!   crate docs) and every property (I1–I9, L1) holds; the union of rows the
//!   profiles exercise matches the table's coverage notes.
//! - `each_fix_is_necessary`: removing any original fix from revision 9
//!   reproduces the counterexample it closes, and the same profile holds
//!   with the fix.
//! - `round8_finding3_after_high_water`: the exact round-8 #3 shape, scripted.
//! - `deep_profiles` (ignored by default): larger bounds, run on demand with
//!   `cargo test -p kwiry-lifecycle --release -- --ignored --nocapture`.
//!
//! Run with `--nocapture` to see the statistics and the counterexample
//! traces.

use kwiry_lifecycle::action::{Action, faults};
use kwiry_lifecycle::checker::{Profile, Report, ScriptEnd, check, check_from, script};
use kwiry_lifecycle::design::{Design, Fix};
use kwiry_lifecycle::props::Prop;
use kwiry_lifecycle::row_id::RowId;
use kwiry_lifecycle::rows::{MACHINES, Model};
use kwiry_lifecycle::state::{Budget, State};

const R0: u8 = 0b01;
const R1: u8 = 0b10;
const BOTH: u8 = 0b11;

/// A budget with nothing but the given retirements.
fn budget(retires: u8, retire_rooms: u8) -> Budget {
    Budget {
        retire_rooms,
        change_rooms: 0,
        crashes: 0,
        faults: 0,
        changes: 0,
        retires,
        drops: 0,
        restores: 0,
    }
}

fn profile(name: &'static str, budget: Budget) -> Profile {
    Profile {
        name,
        design: Design::REV11_3,
        budget,
        faults: 0,
        garbage: false,
        same_state: false,
        liveness: true,
        skip: 0,
        max_states: 6_000_000,
    }
}

/// The main profiles every `cargo test` run proves.
fn ci_profiles() -> Vec<Profile> {
    vec![
        // Two concurrent obligations, GC work, healthy authority.
        Profile {
            garbage: true,
            ..profile("two-obligations", budget(2, BOTH))
        },
        // Up to two crashes anywhere, including between any two writes.
        Profile {
            garbage: true,
            ..profile(
                "crashes",
                Budget {
                    crashes: 2,
                    ..budget(1, R0)
                },
            )
        },
        // A crash and one fault from every family.
        Profile {
            garbage: true,
            faults: faults::ALL,
            ..profile(
                "crash-and-fault",
                Budget {
                    crashes: 1,
                    faults: 1,
                    ..budget(1, R0)
                },
            )
        },
        // A supported restore with a crash and one fault from every family.
        Profile {
            faults: faults::ALL,
            ..profile(
                "epoch-restore",
                Budget {
                    crashes: 1,
                    faults: 1,
                    restores: 1,
                    ..budget(1, R0)
                },
            )
        },
        // An obligation while another room is dropped from the projection:
        // unattributed data, status reports, a human release.
        profile(
            "release",
            Budget {
                changes: 1,
                change_rooms: R1,
                ..budget(1, R0)
            },
        ),
        // Releases under a crash and one fault from every family.
        Profile {
            garbage: true,
            faults: faults::ALL,
            ..profile(
                "release-crash-fault",
                Budget {
                    changes: 1,
                    change_rooms: R1,
                    crashes: 1,
                    faults: 1,
                    ..budget(0, 0)
                },
            )
        },
        // Releases and status reports across a supported restore.
        Profile {
            garbage: true,
            ..profile(
                "release-restore",
                Budget {
                    changes: 1,
                    change_rooms: R1,
                    restores: 1,
                    ..budget(0, 0)
                },
            )
        },
        // Rooms dropped and relisted: re-attribution and strict mismatch.
        Profile {
            garbage: true,
            ..profile(
                "relist",
                Budget {
                    changes: 2,
                    change_rooms: BOTH,
                    ..budget(0, 0)
                },
            )
        },
        // GC of a superseded object while its room changes, with a failed
        // deletion.
        Profile {
            garbage: true,
            faults: faults::DELETE_FAIL,
            ..profile(
                "gc",
                Budget {
                    changes: 1,
                    change_rooms: R0,
                    faults: 1,
                    ..budget(1, R0)
                },
            )
        },
        // Optional counts-only same-state reports.
        Profile {
            same_state: true,
            ..profile(
                "same-state",
                Budget {
                    changes: 1,
                    change_rooms: R1,
                    ..budget(1, R0)
                },
            )
        },
        // Two obligations, one of whose retired items drops off the list.
        profile(
            "retention-drop",
            Budget {
                drops: 1,
                ..budget(2, BOTH)
            },
        ),
    ]
}

fn print(r: &Report) {
    eprintln!("{}", r.render());
}

fn rows_named(rows: &[RowId]) -> String {
    rows.iter().map(|r| r.name()).collect::<Vec<_>>().join(" ")
}

#[test]
fn rev10_holds() {
    let mut seen = vec![false; RowId::ALL.len()];
    let mut total = 0;
    for p in ci_profiles() {
        let r = check(&p);
        print(&r);
        assert!(
            r.violation.is_none(),
            "profile {} failed:\n{}",
            p.name,
            r.render()
        );
        total += r.states;
        for (i, s) in r.rows_seen.iter().enumerate() {
            seen[i] |= *s;
        }
    }
    eprintln!("CI profiles: {total} states in total");
    let mut unexercised = Vec::new();
    let mut unexpected = Vec::new();
    for m in MACHINES {
        for row in m.rows {
            let hit = seen[row.id.index()];
            match row.model {
                Model::Step if !hit => unexercised.push(row.id),
                Model::Implicit(_) | Model::NotModeled(_) if hit => unexpected.push(row.id),
                _ => {}
            }
        }
    }
    assert!(
        unexercised.is_empty(),
        "rows marked as model steps that no CI profile exercised: {}",
        rows_named(&unexercised)
    );
    assert!(
        unexpected.is_empty(),
        "rows marked implicit or not modeled that a step cites: {}",
        rows_named(&unexpected)
    );
}

#[test]
fn room_symmetry_preserves_verdicts_and_reachable_rows() {
    let p = profile("room-symmetry-control", budget(2, BOTH));
    let full = check_from(
        &p,
        State::initial(p.budget, p.garbage),
        Action::candidates(),
    );
    let reduced = check(&p);
    for report in [&full, &reduced] {
        print(report);
        // Both complete with no skips: every safety and liveness verdict
        // agrees, including the absence of capacity/state-bound failures.
        assert!(report.violation.is_none(), "{}", report.render());
    }
    assert_eq!(reduced.rows_seen, full.rows_seen);
    assert!(reduced.states < full.states);
}

/// The profile that reproduces each finding, and the property it violates.
fn finding(fix: Fix) -> (Profile, Prop) {
    let base = |name, b| Profile {
        liveness: true,
        skip: Prop::I8AuthorityFidelity.bit(),
        ..profile(name, b)
    };
    match fix {
        Fix::RestartArmedResumes => (
            base(
                "finding-1",
                Budget {
                    crashes: 1,
                    ..budget(1, R0)
                },
            ),
            Prop::I7Deadlock,
        ),
        Fix::DispatchLinearized => (
            Profile {
                liveness: false,
                skip: Prop::I3StaleProof.bit() | Prop::I8AuthorityFidelity.bit(),
                ..base(
                    "finding-2",
                    Budget {
                        changes: 1,
                        change_rooms: R1,
                        ..budget(1, R0)
                    },
                )
            },
            Prop::I3CompleteDispatch,
        ),
        Fix::NarrowFirst => (
            Profile {
                faults: faults::DISK_FULL,
                ..base(
                    "finding-3",
                    Budget {
                        faults: 1,
                        ..budget(1, R0)
                    },
                )
            },
            Prop::I1Serving,
        ),
        Fix::ConvergenceEpochBound => (
            Profile {
                faults: faults::LATE_DELIVERY,
                ..base(
                    "finding-4",
                    Budget {
                        faults: 1,
                        restores: 1,
                        ..budget(1, R0)
                    },
                )
            },
            Prop::I5Floors,
        ),
        Fix::HaltOperatorResume => (
            Profile {
                faults: faults::REPORT_ERRORS,
                ..base(
                    "finding-5",
                    Budget {
                        faults: 1,
                        ..budget(1, R0)
                    },
                )
            },
            Prop::I7Deadlock,
        ),
        Fix::ReplayRetries => (
            Profile {
                faults: faults::NET_DUP,
                ..base(
                    "finding-6",
                    Budget {
                        faults: 1,
                        ..budget(1, R0)
                    },
                )
            },
            Prop::I8HealthyHalt,
        ),
        Fix::DerivedQuarantineOwner => (
            Profile {
                garbage: true,
                faults: faults::DELETE_FAIL,
                ..base(
                    "finding-7",
                    Budget {
                        faults: 1,
                        ..budget(0, 0)
                    },
                )
            },
            Prop::I9Attribution,
        ),
        Fix::GcClaimRevalidates => (
            Profile {
                garbage: true,
                ..base(
                    "finding-M1",
                    Budget {
                        changes: 1,
                        change_rooms: R0,
                        ..budget(0, 0)
                    },
                )
            },
            Prop::I2Deletion,
        ),
        Fix::ReleaseDetachRetries => (
            Profile {
                faults: faults::DISK_FULL,
                ..base(
                    "finding-M2",
                    Budget {
                        changes: 1,
                        change_rooms: R1,
                        faults: 1,
                        ..budget(0, 0)
                    },
                )
            },
            Prop::I7Deadlock,
        ),
        _ => unreachable!("rev-10 fixes are checked by rev10.rs"),
    }
}

#[test]
fn each_fix_is_necessary() {
    for fix in Fix::REV9 {
        let (p, expected) = finding(fix);
        let p = Profile {
            design: Design::REV9,
            ..p
        };
        let without = check(&Profile {
            design: Design::REV9.without(fix),
            skip: p.skip
                | Prop::I1Retirement.bit()
                | if p.budget.restores > 0 {
                    Prop::L1Liveness.bit()
                } else {
                    0
                },
            ..p
        });
        let cex = without
            .violation
            .as_ref()
            .unwrap_or_else(|| panic!("{fix:?}: no counterexample without the fix"));
        eprintln!(
            "== {} — {:?} removed ({})\n{}rows: {}\n",
            p.name,
            fix,
            fix.finding(),
            cex.render(),
            rows_named(&cex.rows())
        );
        assert_eq!(cex.prop, expected, "{fix:?}: unexpected property");
        let with = check(&Profile {
            skip: p.skip
                | Prop::I1Retirement.bit()
                | if p.budget.restores > 0 {
                    Prop::L1Liveness.bit()
                } else {
                    0
                },
            ..p
        });
        assert!(
            with.violation.is_none(),
            "{fix:?} with the fix:\n{}",
            with.render()
        );
        // Revision 8 as written fails the same profile.
        let rev8 = check(&Profile {
            design: Design::REV8,
            ..p
        });
        assert!(
            rev8.violation.is_some(),
            "{fix:?}: revision 8 passed its profile"
        );
    }
}

/// Round-8 #3 exactly: the authority drops R1 from the projection; A1
/// commits that list on the reserve (K2), so the high-water advances, then
/// A3's publication fails (K2x) and the apply aborts. Revision 8 keeps
/// serving R1 until its lease lapses; revision 9's A0 refused it at
/// admission. (A retired room is already refused once A1's tombstone is
/// durable, because `pin_and_lease` checks committed tombstones.)
#[test]
fn round8_finding3_after_high_water() {
    use Action::*;
    let steps = [
        Boot,
        Markers,
        Narrow,
        FetchStart,
        ListDeliver,
        Apply, // A1 (A2n, A3n run inside it)
        Apply, // A4
        AuthUnlist(1),
        DiskFull,
        FetchStart,
        ListDeliver,
        Apply, // A1 on the reserve: the high-water advances
        Apply, // A3: K2x, the apply aborts
    ];
    let p = Profile {
        faults: faults::DISK_FULL,
        ..profile(
            "finding-3-scripted",
            Budget {
                changes: 1,
                change_rooms: R1,
                faults: 1,
                ..budget(0, 0)
            },
        )
    };
    let rev8 = script(
        &Profile {
            design: Design::REV9.without(Fix::NarrowFirst),
            ..p
        },
        &steps,
    );
    assert_eq!(rev8.err(), Some(ScriptEnd::Violation(10, Prop::I1Serving)));
    let rev9 = script(&p, &steps).expect("revision 9 refuses the room at admission");
    assert!(!kwiry_lifecycle::step::servable(&rev9, 1));
    assert_eq!(
        rev9.dur.hw.map(|h| h.seq),
        Some(2),
        "the high-water advanced"
    );
    assert!(rev9.vol.apply.is_none(), "the apply aborted");
}

// Ignored tests normally run concurrently; their combined peak would
// exceed the memory budget. Keep an individual profile filter for RSS runs.
static DEEP_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
#[ignore = "deep run: minutes and a few GB; run with --release -- --ignored"]
fn deep_profiles() {
    let deep = [
        Profile {
            garbage: true,
            faults: faults::ALL,
            max_states: 40_000_000,
            ..profile(
                "deep-crashes-faults",
                Budget {
                    crashes: 2,
                    faults: 2,
                    ..budget(1, R0)
                },
            )
        },
        Profile {
            faults: faults::DELETE_FAIL,
            max_states: 40_000_000,
            ..profile(
                "deep-release-delete-fail",
                Budget {
                    changes: 1,
                    change_rooms: R1,
                    faults: 1,
                    ..budget(1, R0)
                },
            )
        },
    ];
    for p in deep {
        assert_deep_holds(&p);
    }
}

#[test]
#[ignore = "deep run: minutes and a few GB; run with --release -- --ignored"]
fn deep_two_obligations_crash() {
    assert_deep_holds(&Profile {
        max_states: 40_000_000,
        ..profile(
            "deep-two-obligations-crash",
            Budget {
                crashes: 1,
                ..budget(2, BOTH)
            },
        )
    });
}

fn assert_deep_holds(p: &Profile) {
    let _guard = DEEP_GATE.lock().unwrap_or_else(|e| e.into_inner());
    let r = check(p);
    print(&r);
    assert!(r.violation.is_none(), "{}", r.render());
}
