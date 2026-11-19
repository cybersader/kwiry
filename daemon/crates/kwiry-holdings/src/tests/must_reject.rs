//! Lists the holder must refuse whole, keeping its previous state.

use super::*;
use crate::client::FetchFailure;
use crate::testkit::{TEST_ISSUER, neutral_profile, pin_id};
use crate::verify::{PageError, PageExpectations, SignedPage, VerifyContext, check_page};
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
fn ap17_h16_4_storage_pin_shapes() {
    let pin = |value: Value| {
        on_kind(PINNED, move |i| {
            i.insert("root_pin".into(), value.clone());
        })
    };
    let good_ref = "pr_0000000000000000000001";
    item_rejects(
        on_kind(PINNED, |i| {
            i.remove("root_pin");
        }),
        PageError::RootPin("member missing"),
    );
    item_rejects(
        on_kind(PLAIN, |i| {
            i.insert("root_pin".into(), Value::Null);
        }),
        PageError::RootPin("member on an unpinned item"),
    );
    item_rejects(
        pin(json!({"pin_id": pin_id("x"), "pin_ref": good_ref, "cause": "enroll"})),
        PageError::RootPin("unknown member"),
    );
    for half in [
        json!({"pin_id": null, "pin_ref": good_ref}),
        json!({"pin_id": pin_id("x"), "pin_ref": null}),
        json!({"pin_ref": good_ref}),
        json!({"pin_id": pin_id("x")}),
        json!({"pin_id": null, "pin_ref": null}),
        json!({}),
    ] {
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

fn minimal_page(item: Value) -> Map<String, Value> {
    json!({
        "iss": TEST_ISSUER,
        "aud": "example-lifecycle:hd_TESTholder000000000001",
        "sub": "hd_TESTholder000000000001",
        "iat": 1_794_916_800,
        "exp": 1_794_917_100,
        "jti": "lp_0000000000000000000001",
        "list_id": "ls_0000000000000000000001",
        "nonce": "n".repeat(43),
        "rev": {"epoch": "ep_TESTepoch0000000000001", "seq": 5},
        "holder": {"holder_id": "hd_TESTholder000000000001", "kind": "device",
                   "lease": {"serve_seconds": 900, "relist_seconds": 300}},
        "page": {"index": 0, "count": 1, "items_total": 1},
        "state_digest": format!("hs1.{}", "d".repeat(43)),
        "list_tag": format!("lt1.{}", "t".repeat(43)),
        "items": [item],
        "next_cursor": null,
    })
    .as_object()
    .unwrap()
    .clone()
}

#[test]
fn ap17_h16_4_a_pin_member_for_a_non_pinned_holder_kind_is_refused() {
    let profile = neutral_profile();
    let ctx = VerifyContext {
        profile: &profile,
        issuer: TEST_ISSUER,
        holder_id: "hd_TESTholder000000000001",
        holder_kind: "device",
    };
    let nonce = "n".repeat(43);
    let expect = PageExpectations {
        nonce: &nonce,
        high_water: None,
    };
    let item = |root_pin: Option<Value>| {
        let mut item = json!({
            "holding_id": "hl_0000000000000000000001",
            "resource": {"kind": PINNED, "id": "res-a"},
            "phase": "active",
            "quarantine_after": "2026-11-17T12:15:00Z",
            "obligation": null,
        });
        if let Some(pin) = root_pin {
            item["root_pin"] = pin;
        }
        item
    };
    let check = |claims| {
        check_page(
            SignedPage {
                claims,
                iat_ms: 1_794_916_800_000,
            },
            1_794_916_800_000,
            &ctx,
            &expect,
        )
        .map(|_| ())
    };
    assert_eq!(check(minimal_page(item(None))), Ok(()));
    assert_eq!(
        check(minimal_page(item(Some(Value::Null)))),
        Err(PageError::RootPin("member on an unpinned item"))
    );
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
    for (phase, state) in [
        ("active", Some("open")),
        ("terminating", None),
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
    let cases: Vec<(ItemHook, &str)> = vec![
        (
            on_kind(PLAIN, |i| {
                i.insert("note".into(), "x".into());
            }),
            "item members",
        ),
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
            o.insert("reason".into(), "access-lost".into());
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
            p["page"]["index"] = 1.into();
        }),
        PageError::Grammar("page.index"),
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
            p.insert("extra".into(), 1.into());
        }),
        PageError::Grammar("page members"),
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
        PageError::Grammar("keyset"),
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
