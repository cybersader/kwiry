//! Lists the holder must refuse whole, keeping its previous state.

use super::*;
use crate::client::FetchFailure;
use crate::testkit::pin_id;
use crate::verify::PageError;
use serde_json::{Map, Value, json};

type ItemHook = Box<dyn Fn(&mut Map<String, Value>) + Send>;

/// A pinned, attested item and a plain item, each mutated by `hook`; the
/// list must be refused with `expected` and nothing applied.
#[track_caller]
fn item_rejects(hook: ItemHook, expected: PageError) {
    let (_dir, mut rig) = rig();
    let a = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&a, "dir-a");
    rig.authority.state().add_active(PLAIN, "res-b");
    rig.authority.state().hooks.item = Some(hook);
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::Invalid(expected)
    );
    assert_eq!(rig.holder().state().high_water, None);
    assert!(rig.store.view().published.is_empty());
}

fn on_kind(
    kind: &'static str,
    change: impl Fn(&mut Map<String, Value>) + Send + 'static,
) -> ItemHook {
    Box::new(move |item| {
        if item["resource"]["kind"] == kind {
            change(item);
        }
    })
}

#[test]
fn ap17_h16_4_storage_pin_shapes_outside_the_golden_set() {
    let pin = |value: Value| {
        on_kind(PINNED, move |i| {
            i.insert("root_pin".into(), value.clone());
        })
    };
    let good_ref = "pr_0000000000000000000001";
    // Missing, unknown-member, half-null, half-absent and on-a-non-pinned
    // item shapes are golden must-reject vectors; these are not.
    for half in [json!({"pin_id": null, "pin_ref": null}), json!({})] {
        item_rejects(
            pin(half),
            PageError::RootPin("pin_id and pin_ref must both be present"),
        );
    }
    let short = pin_id("x")[..42].to_owned();
    let padded = format!("{}=", &pin_id("x")[..42]);
    for bad in [short, padded, format!("{}a", pin_id("x"))] {
        item_rejects(
            pin(json!({"pin_id": bad, "pin_ref": good_ref})),
            PageError::RootPin("pin_id"),
        );
    }
    for bad in [
        "pr_000000000000000000001",
        "px_0000000000000000000001",
        "pr_00000000000000000000/1",
    ] {
        item_rejects(
            pin(json!({"pin_id": pin_id("x"), "pin_ref": bad})),
            PageError::RootPin("pin_ref"),
        );
    }
    item_rejects(pin(json!("pinned")), PageError::RootPin("not an object"));
}

#[test]
fn phase_and_obligation_combinations_outside_the_table_are_refused() {
    let obligation = |state: &str| {
        json!({
            "obligation_id": "ob_0000000000000000000009",
            "reason": "retire",
            "required": "purge",
            "state": state,
            "created_seq": 1,
            "created_at": "2026-11-17T11:00:00Z",
            "deadline": "2026-11-24T11:00:00Z",
            "last_accepted": null,
        })
    };
    let set = move |phase: &'static str, state: Option<&'static str>| {
        let value = state.map_or(Value::Null, obligation);
        on_kind(PLAIN, move |i| {
            i.insert("phase".into(), phase.into());
            i.insert("obligation".into(), value.clone());
        })
    };
    // `terminating` with no obligation is a golden must-reject vector.
    for (phase, state) in [
        ("active", Some("open")),
        ("terminating", Some("complete")),
        ("retired", Some("open")),
        ("retired", Some("quarantined")),
        ("retired", None),
    ] {
        item_rejects(set(phase, state), PageError::Combination);
    }
}

#[test]
fn item_and_obligation_grammar_is_closed() {
    // An unknown item member is a golden must-reject vector.
    let cases: Vec<(ItemHook, &str)> = vec![
        (
            on_kind(PLAIN, |i| {
                i["resource"]["kind"] = "spaceship".into();
            }),
            "resource.kind",
        ),
        (
            on_kind(PLAIN, |i| {
                i["resource"]["kind"] = "volume".into();
            }),
            "resource.kind not accepted by this holder",
        ),
        (
            on_kind(PLAIN, |i| {
                i["resource"]["id"] = "r".repeat(129).into();
            }),
            "resource.id",
        ),
        (
            on_kind(PLAIN, |i| {
                i["resource"]["id"] = "a.b".into();
            }),
            "resource.id",
        ),
        (
            on_kind(PLAIN, |i| {
                i["holding_id"] = "hl_short".into();
            }),
            "holding_id",
        ),
        (
            on_kind(PLAIN, |i| {
                i["phase"] = "paused".into();
            }),
            "phase",
        ),
        (
            on_kind(PLAIN, |i| {
                i["quarantine_after"] = "2026-11-17T12:15:00+00:00".into();
            }),
            "quarantine_after",
        ),
    ];
    for (hook, reason) in cases {
        item_rejects(hook, PageError::Grammar(reason));
    }

    let obligated = |change: fn(&mut Map<String, Value>), reason: &'static str| {
        let (_dir, mut rig) = rig();
        let b = rig.authority.state().add_active(PLAIN, "res-b");
        rig.authority.state().retire(&b);
        rig.authority.state().hooks.item = Some(Box::new(move |item| {
            if let Some(obligation) = item.get_mut("obligation").and_then(Value::as_object_mut) {
                change(obligation);
            }
        }));
        assert_eq!(
            failed(rig.holder().list_once()),
            FetchFailure::Invalid(PageError::Grammar(reason)),
            "{reason}"
        );
    };
    obligated(
        |o| {
            o.insert("reason".into(), 7.into());
        },
        "obligation.reason",
    );
    obligated(
        |o| {
            o.insert("required".into(), "crypto_erase".into());
        },
        "obligation.required",
    );
    obligated(
        |o| {
            o.insert("state".into(), "lost".into());
        },
        "obligation.state",
    );
    obligated(
        |o| {
            o.insert("created_seq".into(), 1_000_000.into());
        },
        "obligation.created_seq above rev",
    );
    obligated(
        |o| {
            o.insert("state".into(), "blocked".into());
        },
        "blocked member must accompany state blocked",
    );
    obligated(
        |o| {
            o.insert(
                "blocked".into(),
                json!({"by": "release_declined", "at": "2026-11-17T11:00:00Z", "status_report_digest": "sha256:00"}),
            );
        },
        "blocked member must accompany state blocked",
    );
    obligated(
        |o| {
            o.insert(
                "last_accepted".into(),
                json!({"state": "done", "at": "2026-11-17T11:00:00Z"}),
            );
        },
        "last_accepted.state",
    );
    obligated(
        |o| {
            o.insert("extra".into(), 1.into());
        },
        "obligation members",
    );
}

type PageHook = Box<dyn Fn(&mut Map<String, Value>) + Send>;

#[track_caller]
fn page_rejects(items: usize, page_size: u16, hook: PageHook, expected: PageError) {
    let (_dir, mut rig) = rig();
    rig.config.page_size = page_size;
    rig.restart().unwrap();
    for n in 0..items {
        rig.authority.state().add_active(PLAIN, &format!("res-{n}"));
    }
    rig.authority.state().hooks.page = Some(hook);
    assert_eq!(
        failed(rig.holder().list_once()),
        FetchFailure::Invalid(expected)
    );
    assert_eq!(rig.holder().state().high_water, None);
}

fn on_page(index: u64, change: impl Fn(&mut Map<String, Value>) + Send + 'static) -> PageHook {
    Box::new(move |page| {
        if page["page"]["index"] == index {
            change(page);
        }
    })
}

#[test]
fn h9_and_page_grammar_refuse_the_whole_list() {
    // An impossible page index, a repeated holding and an unknown page
    // member are golden must-reject vectors.
    page_rejects(
        2,
        500,
        on_page(0, |p| {
            p["page"]["items_total"] = 3.into();
        }),
        PageError::Consistency("items_total"),
    );
    page_rejects(
        2,
        500,
        on_page(0, |p| {
            let mut items = p["items"].as_array().unwrap().clone();
            items.reverse();
            p.insert("items".into(), items.into());
        }),
        PageError::Consistency("holding ids not strictly ascending"),
    );
    page_rejects(
        2,
        500,
        on_page(0, |p| {
            p["items"][1]["resource"]["id"] = "res-0".into();
        }),
        PageError::Consistency("resource id listed twice"),
    );
    page_rejects(
        2,
        1,
        on_page(1, |p| {
            let seq = p["rev"]["seq"].as_u64().unwrap();
            p["rev"]["seq"] = (seq + 1).into();
        }),
        PageError::Consistency("pages disagree"),
    );
    page_rejects(
        2,
        1,
        on_page(1, |p| {
            let mut items = p["items"].as_array().unwrap().clone();
            items[0]["holding_id"] = "hl_0000000000000000000000".into();
            p.insert("items".into(), items.into());
        }),
        PageError::Consistency("holding ids not strictly ascending"),
    );
    page_rejects(
        2,
        1,
        on_page(0, |p| {
            p.insert("next_cursor".into(), Value::Null);
        }),
        PageError::Grammar("next_cursor"),
    );
    page_rejects(
        2,
        1,
        on_page(1, |p| {
            p["page"]["count"] = 3.into();
        }),
        PageError::Grammar("next_cursor"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p.insert("next_cursor".into(), "c".repeat(513).into());
        }),
        PageError::Grammar("next_cursor"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p.insert(
                "state_digest".into(),
                format!("hs2.{}", "d".repeat(43)).into(),
            );
        }),
        PageError::Grammar("state_digest"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p.insert("list_tag".into(), format!("lt1.{}", "t".repeat(44)).into());
        }),
        PageError::Grammar("list_tag"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p.insert("list_id".into(), "ls_x".into());
        }),
        PageError::Grammar("list_id"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p["holder"]["kind"] = "device".into();
        }),
        PageError::Grammar("holder.kind"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p["holder"]["lease"]["serve_seconds"] = 0.into();
        }),
        PageError::Grammar("lease"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p["rev"]["epoch"] = "ep_bad".into();
        }),
        PageError::Grammar("rev.epoch"),
    );
    page_rejects(
        1,
        500,
        on_page(0, |p| {
            p.insert("float".into(), json!(1.5));
        }),
        PageError::Jws(crate::jws::JwsError::Payload),
    );
    page_rejects(
        2,
        1,
        on_page(1, |p| {
            p.insert("instructions".into(), json!([]));
        }),
        PageError::Grammar("instructions off page 0"),
    );
    page_rejects(
        2,
        1,
        on_page(1, |p| {
            p.insert("keyset".into(), "x.y.z".into());
        }),
        PageError::Keyset,
    );
}

#[test]
fn instructions_unknown_kinds_are_skipped_and_known_ones_checked() {
    let (_dir, mut rig) = rig();
    rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().instructions = vec![
        json!({"kind": "future_instruction", "anything": true}),
        json!({"instruction_id": "ins_0000000000000000000001", "kind": "quarantine_release",
               "bound_report_digest": "sha256:ab", "approved_count": 3, "issued_seq": 1}),
    ];
    let report = applied(rig.holder().list_once());
    assert_eq!(report.instructions, 1);
    assert!(codes(&rig).contains(&"lifecycle_instruction_skipped"));

    for bad in [
        json!({"instruction_id": "ins_0000000000000000000001", "kind": "quarantine_release",
               "bound_report_digest": "sha256:ab", "approved_count": 3, "issued_seq": 999_999}),
        json!({"instruction_id": "ins_0000000000000000000001", "kind": "quarantine_release",
               "bound_report_digest": "md5:ab", "approved_count": 3, "issued_seq": 1}),
        json!({"kind": "quarantine_release"}),
        json!({"no_kind": true}),
    ] {
        let (_dir, mut rig) = super::rig();
        rig.authority.state().instructions = vec![bad];
        assert!(matches!(
            failed(rig.holder().list_once()),
            FetchFailure::Invalid(PageError::Grammar(_))
        ));
    }
}

#[test]
fn a_list_that_validates_after_a_refused_one_applies_normally() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-a", true, 1);
    rig.authority.state().add_active(PLAIN, "res-a");
    rig.authority.state().hooks.page = Some(Box::new(|p| {
        p.insert("extra".into(), 1.into());
    }));
    failed(rig.holder().list_once());
    rig.authority.state().hooks = Default::default();
    applied(rig.relist());
    assert_eq!(plain(&rig, "res-a"), Ok(()));
}

#[test]
fn h7_an_unrecognised_reason_is_opaque_and_never_rejects_the_list() {
    let (_dir, mut rig) = rig();
    rig.store.hold(PLAIN, "res-b", true, 1);
    let b = rig.authority.state().add_active(PLAIN, "res-b");
    rig.authority.state().retire(&b);
    for reason in ["access-lost", "key-rotation", "a-reason-from-the-future"] {
        rig.authority.state().items.get_mut(&b).unwrap()["obligation"]["reason"] = reason.into();
        rig.authority.state().commit();
        let report = applied(rig.relist());
        assert!(matches!(
            report.plan.dispositions.values().next(),
            Some(crate::store::Disposition::Obligated { .. })
        ));
        assert_eq!(plain(&rig, "res-b"), Err(Refusal::Tombstoned));
    }
    assert_eq!(
        codes(&rig)
            .iter()
            .filter(|c| **c == "lifecycle_reason_unrecognised")
            .count(),
        3
    );
}
