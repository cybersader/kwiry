//! List vectors: keys, JCS inputs, the fixed lists, the must-reject set,
//! V14 and the interim refusal of new-epoch lists.

use super::*;
use crate::canonical::canonical;
use crate::engine::EngineRefusal;
use crate::holder::ApplyReport;
use crate::jws::JwsError;
use crate::keys::{PinnedKeys, ed25519_public_from_jwk, okp_thumbprint, parse_authority_jwk};
use crate::profile::ROUTE_KEYSET;
use crate::store::Disposition as Placement;
use crate::verify::PageError;

/// The items of every page of a list vector, decoded.
fn items(list: &'static Value) -> Vec<Value> {
    GoldenList::of(list)
        .pages
        .iter()
        .flat_map(|page| claims(page)["items"].as_array().expect("items").clone())
        .collect()
}

fn resource(item: &Value) -> ResourceKey {
    ResourceKey {
        kind: text(&item["resource"]["kind"]).to_owned(),
        id: text(&item["resource"]["id"]).to_owned(),
    }
}

/// Holds every resource the list names, configured, so that what serves is
/// decided by the list alone.
fn hold_listed(rig: &GoldenRig, list: &'static Value) {
    for item in items(list) {
        let key = resource(&item);
        rig.store.hold(&key.kind, &key.id, true, 1);
    }
}

#[track_caller]
fn applied(outcome: ListOutcome) -> ApplyReport {
    match outcome {
        ListOutcome::Applied(report) => report,
        other => panic!("expected the list to apply, got {other:?}"),
    }
}

/// A resource the holder has and no list names (§3.4 rule 1).
const UNLISTED: &str = "unlistedResource000000x";
const UNLISTED_UNITS: u64 = 7;

/// A fresh engine holder for the list's subject, every listed resource
/// held plus one unlisted resource, the list delivered at its page-0 `iat`.
fn held_and_applied(list: &'static Value) -> (GoldenRig, ApplyReport) {
    let golden = GoldenList::of(list);
    let mut rig = GoldenRig::engine(&golden.holder_id(), golden.iat_s());
    hold_listed(&rig, list);
    rig.store
        .hold(engine_kind_resource(), UNLISTED, true, UNLISTED_UNITS);
    let report = applied(rig.deliver(list));
    // Unknown data is withheld and counted, never deleted (§3.4 rules 1, 5).
    let unknown = ResourceKey {
        kind: engine_kind_resource().to_owned(),
        id: UNLISTED.to_owned(),
    };
    assert_eq!(
        report.plan.dispositions[&unknown],
        Placement::WithheldUnknown
    );
    assert_eq!(report.plan.unattributed_units, UNLISTED_UNITS);
    assert_eq!(rig.serve_unstamped(UNLISTED), Err(Refusal::NotListedActive));
    assert!(rig.store.bytes_present(&unknown.kind, UNLISTED));
    assert_eq!(
        rig.store.view().published.len(),
        1,
        "one atomic publication"
    );
    (rig, report)
}

/// The resource kind the engine profile pins (the kind every golden list
/// item carries).
fn engine_kind_resource() -> &'static str {
    text(&provenance()["wire_profile"]["engine_pinned_resource_kind"])
}

/// §3.4 and H.16.5 as the applied list states them: the high-water and the
/// last list reference are the list's; an `active` room serves only with a
/// pin and only from artifacts stamped with its `pin_ref`; everything not
/// `active` is tombstoned and refused; nothing is deleted.
fn assert_serving_follows(rig: &GoldenRig, report: &ApplyReport, list: &'static Value) {
    let golden = GoldenList::of(list);
    let state = rig.holder.state();
    assert_eq!(report.rev, golden.rev());
    assert_eq!(state.high_water.as_ref(), Some(&golden.rev()));
    assert_eq!(state.last_list_ref.as_ref(), Some(&golden.list_ref()));
    for item in items(list) {
        let key = resource(&item);
        let id = key.id.as_str();
        assert!(
            rig.store.bytes_present(&key.kind, id),
            "{id}: nothing is deleted"
        );
        let placement = &report.plan.dispositions[&key];
        if item["phase"] == "active" {
            assert!(
                matches!(placement, Placement::Mapped { .. }),
                "{id}: {placement:?}"
            );
            match item["root_pin"].get("pin_ref").and_then(Value::as_str) {
                None => assert_eq!(
                    rig.serve_unstamped(id),
                    Err(Refusal::Engine(EngineRefusal::RootPinMissing)),
                    "{id}: a null pin never serves"
                ),
                Some(pin_ref) => {
                    assert_eq!(rig.serve_pinned(id, pin_ref), Ok(()), "{id}");
                    assert_eq!(
                        rig.serve_pinned(id, "pr_0000000000000000000000"),
                        Err(Refusal::Engine(EngineRefusal::ArtifactPinRef)),
                        "{id}: another ref never serves"
                    );
                }
            }
        } else {
            assert!(
                state
                    .tombstones
                    .iter()
                    .any(|t| t.resource_id == id && t.holding_id == text(&item["holding_id"])),
                "{id}: tombstoned"
            );
            assert!(
                matches!(
                    placement,
                    Placement::Obligated { .. } | Placement::RetiredComplete { .. }
                ),
                "{id}: {placement:?}"
            );
            assert_eq!(rig.serve_unstamped(id), Err(Refusal::Tombstoned), "{id}");
        }
    }
}

/// The holder's own list request carries every member the recorded request
/// carries, with the same values where they are list-bound; it adds only
/// members §3.1 marks optional.
fn assert_request_shape(sent: &Map<String, Value>, recorded: &Map<String, Value>) {
    for name in recorded.keys() {
        assert!(sent.contains_key(name), "the holder's request lacks {name}");
    }
    for name in sent.keys() {
        assert!(
            recorded.contains_key(name) || ["page_size", "last_list_ref"].contains(&name.as_str()),
            "the holder's request adds {name}"
        );
    }
    for name in [
        "iss",
        "sub",
        "aud",
        "htu",
        "op",
        "nonce",
        "high_water",
        "cursor",
    ] {
        assert_eq!(sent[name], recorded[name], "{name}");
    }
    // §2.2: holders SHOULD sign with `exp = iat + 300`.
    let lifetime = sent["exp"].as_i64().unwrap() - sent["iat"].as_i64().unwrap();
    assert_eq!(lifetime, 300, "§2.2 lifetime");
    if recorded["high_water"].is_null() {
        // First contact: no witness (§3.1).
        assert!(
            !sent.contains_key("last_list_ref"),
            "no witness on first contact"
        );
    }
}

// ------------------------------------------------------------ keys, JCS

#[test]
fn keys_pin_the_authority_set_and_the_holder_thumbprint_signs_every_request() {
    let prefix = profile(true).authority_key_id_prefix;
    let pinned = PinnedKeys::from_enrollment_jwks(&authority_jwks(), &prefix).expect("pins");
    let (unknown_kid, unknown_key) =
        parse_authority_jwk(&fixture()["keys"]["unknown_authority_jwk"], &prefix).expect("jwk");
    assert!(!pinned.keys.contains_key(&unknown_kid));
    // Every authority answer verifies under the published key its kid
    // names; only the unknown key (V14) is outside the pinned set.
    let authority_key = |kid: &str| {
        pinned
            .live(kid, &prefix, 0)
            .or_else(|| (kid == unknown_kid).then_some(unknown_key))
    };
    // Holder kid = RFC 7638 thumbprint of the holder's public JWK.
    let holders: Vec<(String, ed25519_dalek::VerifyingKey)> =
        ["holder_public_jwk", "secondary_holder_public_jwk"]
            .iter()
            .map(|name| {
                let jwk = &fixture()["keys"][*name];
                (
                    okp_thumbprint(text(&jwk["x"])),
                    ed25519_public_from_jwk(jwk).expect("Ed25519 public JWK"),
                )
            })
            .collect();
    let mut exchanges = Vec::new();
    collect_exchanges(fixture(), &mut exchanges);
    let mut verified_answers = 0;
    for exchange in exchanges {
        let request = crate::jws::parse(exchange_request(exchange)).expect("closed and strict");
        assert_eq!(request.alg, "EdDSA");
        let (_, key) = holders
            .iter()
            .find(|(kid, _)| *kid == request.kid)
            .expect("the request kid is a holder thumbprint");
        assert!(request.verify_eddsa(key), "holder signature");
        assert_eq!(request.claims["iss"], request.claims["sub"]);
        let response = &exchange["response"];
        let mut answers: Vec<String> = Vec::new();
        if response["status"] == 200 {
            answers.push(text(&response["body"]).to_owned());
        } else if let Some(current) = crate::parse_strict_json(text(&response["body"]).as_bytes())
            .ok()
            .and_then(|problem| {
                problem
                    .get("current_ack")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        {
            answers.push(current);
        }
        for answer in answers {
            let parsed = crate::jws::parse(&answer).expect("closed and strict");
            let key = authority_key(&parsed.kid).expect("a published authority kid");
            assert!(parsed.verify_es256(&key), "authority signature");
            verified_answers += 1;
        }
    }
    assert!(verified_answers > 20, "{verified_answers}");
}

#[test]
fn every_jcs_input_is_canonical_and_matches_its_signed_list() {
    let lists = every_list();
    assert!(lists.len() >= 20, "{} lists", lists.len());
    for (path, value) in lists {
        for name in ["digest_jcs", "tag_jcs"] {
            let jcs = text(&value[name]);
            let parsed = crate::parse_strict_json(jcs.as_bytes()).expect("strict JSON");
            assert_eq!(canonical(&parsed), jcs, "{path}.{name} is canonical");
        }
        // §3.5: the digest input is the projection of every signed page,
        // sorted by holding id, with each class-defined member as on the
        // wire; §3.6: the tag input binds it to the list.
        let golden = GoldenList::of(value);
        let first = golden.page0();
        let mut entries: Vec<Value> = items(value)
            .iter()
            .map(|item| {
                let obligation = &item["obligation"];
                let mut entry = json!({
                    "holding_id": item["holding_id"],
                    "resource": item["resource"],
                    "phase": item["phase"],
                    "obligation_id": obligation.get("obligation_id").cloned().unwrap_or(Value::Null),
                    "obligation_state": obligation.get("state").cloned().unwrap_or(Value::Null),
                });
                if let Some(pin) = item.get("root_pin") {
                    entry["root_pin"] = pin.clone();
                }
                entry
            })
            .collect();
        entries.sort_by(|a, b| {
            text(&a["holding_id"])
                .as_bytes()
                .cmp(text(&b["holding_id"]).as_bytes())
        });
        let content = json!({
            "holder_id": first["holder"]["holder_id"],
            "rev": first["rev"],
            "list_id": first["list_id"],
            "nonce": first["nonce"],
            "items": entries,
        });
        assert_eq!(
            canonical(&content),
            text(&value["digest_jcs"]),
            "{path}: digest input"
        );
        let tag = json!({
            "holder_id": first["holder"]["holder_id"],
            "list_id": first["list_id"],
            "rev": first["rev"],
            "nonce": first["nonce"],
            "state_digest": first["state_digest"],
        });
        assert_eq!(
            canonical(&tag),
            text(&value["tag_jcs"]),
            "{path}: tag input"
        );
        if let Some(reference) = value.get("list") {
            assert_eq!(
                list_ref_of(reference),
                golden.list_ref(),
                "{path}: list reference"
            );
        }
        if let Some(stated) = value.get("claims") {
            assert_eq!(&Value::Object(first), stated, "{path}: stated claims");
        }
    }
}

// --------------------------------------------------------- fixed lists

#[test]
fn section_3_2_signed_page() {
    let list = at("fixed_lists.section_3_2");
    let (rig, report) = held_and_applied(list);
    assert_serving_follows(&rig, &report, list);
}

#[test]
fn section_3_2_with_pin() {
    let list = at("fixed_lists.section_3_2_with_pin");
    let (rig, report) = held_and_applied(list);
    assert_serving_follows(&rig, &report, list);
}

#[test]
fn section_3_2_route() {
    let list = at("fixed_lists.section_3_2_route");
    let (rig, report) = held_and_applied(list);
    assert_serving_follows(&rig, &report, list);
    let sent = rig.transport.posted_claims();
    assert_eq!(sent.len(), 1);
    assert_request_shape(&sent[0], &claims(exchange_request(&list["pages"][0])));
}

#[test]
fn mixed_pins_null_and_pinned() {
    let list = at("fixed_lists.mixed_pins");
    let (rig, report) = held_and_applied(list);
    let pins: Vec<bool> = items(list)
        .iter()
        .map(|i| i["root_pin"].is_null())
        .collect();
    assert!(
        pins.contains(&true) && pins.contains(&false),
        "H.16.10: null and pinned"
    );
    assert_serving_follows(&rig, &report, list);
}

#[test]
fn mixed_pins_alternate() {
    let list = at("fixed_lists.mixed_pins_alternate");
    let (rig, report) = held_and_applied(list);
    assert_serving_follows(&rig, &report, list);
}

#[test]
fn two_pages_share_one_nonce() {
    let list = at("fixed_lists.two_pages");
    let (rig, report) = held_and_applied(list);
    assert_serving_follows(&rig, &report, list);
    let golden = GoldenList::of(list);
    let sent = rig.transport.posted_claims();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|c| text(&c["nonce"]) == golden.nonce()));
    assert_eq!(sent[0]["cursor"], Value::Null);
    assert_eq!(sent[1]["cursor"], golden.page0()["next_cursor"]);
    for (page, sent) in list["pages"].as_array().unwrap().iter().zip(&sent) {
        assert_request_shape(sent, &claims(exchange_request(page)));
    }
}

#[test]
fn h16_10_cursor_across_attestation_and_reattestation() {
    let case = at("H16_10");
    let first = &case["cursor_across_attestation"]["first_page"];
    let continuation = &case["cursor_across_attestation"]["continuation"];
    let relist = GoldenList::of(&case["reattestation"]["list"]);
    let first_page = claims(text(&first["response"]["body"]));
    let first_nonce = text(&first_page["nonce"]).to_owned();
    let mut rig = GoldenRig::engine(&relist.holder_id(), relist.iat_s());
    hold_listed(&rig, &case["reattestation"]["list"]);
    let (relist_nonce, relist_iat) = (relist.nonce(), relist.iat_s());
    let mut answers = vec![scripted(first), scripted(continuation)];
    answers.extend(GoldenList::of(&case["reattestation"]["list"]).answers);
    let report = applied(rig.list_with(&[&first_nonce, &relist_nonce], answers, relist_iat));
    // H8: the 410 discards page 0 and relists from page 1 with a fresh nonce.
    let sent = rig.transport.posted_claims();
    assert_eq!(sent.len(), 3);
    assert_eq!(
        (text(&sent[0]["nonce"]), &sent[0]["cursor"]),
        (first_nonce.as_str(), &Value::Null)
    );
    assert_eq!(
        (text(&sent[1]["nonce"]), &sent[1]["cursor"]),
        (first_nonce.as_str(), &first_page["next_cursor"])
    );
    assert_eq!(
        (text(&sent[2]["nonce"]), &sent[2]["cursor"]),
        (relist.nonce().as_str(), &Value::Null)
    );
    assert_ne!(first_nonce, relist.nonce());
    assert_eq!(report.rev, relist.rev());
    assert_eq!(rig.store.view().published.len(), 1, "no partial apply");
    assert_serving_follows(&rig, &report, &case["reattestation"]["list"]);
    // The re-attested pin serves; the backup's ref never does again.
    let expect = &case["reattestation"]["expect"];
    assert_eq!(&case["cursor_across_attestation"]["new_pin"], expect);
    let room = items(&case["reattestation"]["list"])
        .into_iter()
        .find(|i| i["root_pin"]["pin_id"] == expect["pin_id"])
        .expect("the re-attested room");
    assert_eq!(room["root_pin"]["pin_ref"], expect["pin_ref"]);
    let id = text(&room["resource"]["id"]);
    assert_eq!(rig.serve_pinned(id, text(&expect["pin_ref"])), Ok(()));
    assert_eq!(
        rig.serve_pinned(id, text(&case["reattestation"]["backup_pin"]["pin_ref"])),
        Err(Refusal::Engine(EngineRefusal::ArtifactPinRef))
    );
}

// ------------------------------------------------------------------ V14

#[test]
fn v14() {
    let case = at("cases.V14");
    let golden = GoldenList::of(&case["list"]);
    let mut rig = GoldenRig::engine(&golden.holder_id(), golden.iat_s());
    hold_listed(&rig, &case["list"]);
    let pinned: Vec<&String> = rig.holder.pinned_keys().keys.keys().collect();
    let expected: Vec<&str> = case["pinned_kids"]
        .as_array()
        .unwrap()
        .iter()
        .map(text)
        .collect();
    assert_eq!(pinned, expected);
    assert_eq!(
        text(&header(&golden.pages[0])["kid"]),
        text(&case["signing_kid"])
    );
    let before = rig.holder.state().clone();
    let pins_before = rig.holder.pinned_keys().clone();
    assert_eq!(failed(rig.deliver(&case["list"])), FetchFailure::UnknownKid);
    assert_eq!(text(&case["expect"]["holder"]), "unknown_kid");
    // H.6: one fetch of the key-set route, which offers no valid statement.
    assert_eq!(
        rig.transport.wire().gets,
        [format!("{}{ROUTE_KEYSET}", issuer())]
    );
    assert!(rig.codes().contains(&"unknown_kid"));
    // Not admitted: nothing changed, nothing serves.
    assert_eq!(case["expect"]["admit"], false);
    assert_eq!(rig.holder.state(), &before);
    assert_eq!(rig.holder.pinned_keys(), &pins_before);
    for item in items(&case["list"]) {
        assert!(rig.serve_unstamped(text(&item["resource"]["id"])).is_err());
    }
}

// ---------------------------------------------------------- must-reject

/// The holder's refusal class for each must-reject `check`, in the §3.8
/// checklist order (the first failing step names the class).
fn refusal_class(check: &str, failure: &FetchFailure, accept_every_kind: bool) -> bool {
    use PageError as P;
    let page = match failure {
        FetchFailure::Invalid(error) => Some(error),
        _ => None,
    };
    match check {
        // §C.3, H.16.4: the class-defined member's presence and grammar.
        "engine_room_missing_root_pin" => page == Some(&P::RootPin("member missing")),
        "pin_id_grammar" => page == Some(&P::RootPin("pin_id")),
        "pin_ref_grammar" => page == Some(&P::RootPin("pin_ref")),
        "closed_root_pin" => page == Some(&P::RootPin("unknown member")),
        "pin_pair_grammar" => page == Some(&P::RootPin("pin_id and pin_ref must both be present")),
        "root_pin_on_non_engine" => page == Some(&P::RootPin("member on an unpinned item")),
        // Profile S receives only `room` (S.0), so for it the kind is the
        // first item-grammar step that fails; a holder that accepts the
        // kind refuses the member itself (§C.3).
        "root_pin_on_non_room" if accept_every_kind => {
            page == Some(&P::RootPin("member on an unpinned item"))
        }
        "root_pin_on_non_room" => {
            page == Some(&P::Grammar("resource.kind not accepted by this holder"))
        }
        // H.35: closed members, closed headers, duplicate keys.
        "closed_list" => page == Some(&P::Grammar("page members")),
        "closed_list_item" => page == Some(&P::Grammar("item members")),
        "duplicate_json_member" => page == Some(&P::Jws(JwsError::Payload)),
        "duplicate_jws_header_member" => page == Some(&P::Jws(JwsError::Header)),
        // A missing `sub` fails the `sub = own holder_id` step (§3.8), which
        // runs before the closed-member check.
        "missing_required_claim" => page == Some(&P::Subject),
        // §3.8 in order.
        "wrong_jws_type" => page == Some(&P::Typ),
        "signature_invalid" => page == Some(&P::Signature),
        "issuer_mismatch" => page == Some(&P::Issuer),
        "audience_mismatch" => page == Some(&P::Audience),
        "subject_mismatch" => page == Some(&P::Subject),
        "nonce_mismatch" => page == Some(&P::Nonce),
        "expired" | "iat_outside_window" => page == Some(&P::Time),
        // §3.7: same epoch, `seq` below the high-water.
        "authority_rollback" => *failure == FetchFailure::Regressed,
        // §3.7 page consistency: an index at or past `count`; a holding id
        // that does not strictly ascend.
        "page_consistency" => page == Some(&P::Grammar("page.index")),
        "duplicate_holding" => page == Some(&P::Consistency("holding ids not strictly ascending")),
        // §3.3 combinations.
        "phase_obligation_combination" => page == Some(&P::Combination),
        _ => false,
    }
}

/// The valid list a must-reject page was made from (same holder, list and
/// revision), or the list at the vector's stated high-water.
fn previous_list(vector: &Value, page: &Map<String, Value>) -> Option<&'static Value> {
    let holder = text(&page["holder"]["holder_id"]);
    let wanted = vector["input"].get("high_water").map(rev_of);
    every_list().into_iter().map(|(_, list)| list).find(|list| {
        let golden = GoldenList::of(list);
        golden.holder_id() == holder
            && match &wanted {
                Some(rev) => golden.rev() == *rev,
                None => {
                    golden.page0()["list_id"] == page["list_id"]
                        && golden.rev() == rev_of(&page["rev"])
                }
            }
    })
}

/// What serves right now, for every resource the list names.
fn serving(rig: &GoldenRig, list: &'static Value) -> Vec<(String, Result<(), Refusal>)> {
    items(list)
        .iter()
        .map(|item| {
            let id = text(&item["resource"]["id"]).to_owned();
            let answer = match item["root_pin"]["pin_ref"].as_str() {
                Some(pin_ref) => rig.serve_pinned(&id, pin_ref),
                None => rig.serve_unstamped(&id),
            };
            (id, answer)
        })
        .collect()
}

/// Delivers a must-reject page through `list_once` after the valid list it
/// was made from, and checks the refusal class and that the previous state
/// and serving stand. `accept_every_kind` widens the holder's accepted
/// resource kinds to the whole wire set.
fn must_reject(name: &str, accept_every_kind: bool) {
    let vector = at(&format!("must_reject.{name}"));
    assert_eq!(
        text(&vector["expected"]),
        "reject_entire_list_keep_previous_state"
    );
    let page = text(&vector["signed_page"]);
    let check = text(&vector["check"]);
    let claims = claims(page);
    let holder_id = text(&claims["holder"]["holder_id"]);
    let holder_kind = text(&claims["holder"]["kind"]);
    let iat = claims["iat"].as_i64().expect("iat");
    let engine = holder_kind == engine_kind();
    let mut wire = profile(engine);
    if accept_every_kind {
        wire.accepted_resource_kinds = wire.resource_kinds.clone();
    }
    let mut rig = GoldenRig::enrolled(holder_id, holder_kind, wire, iat);
    // Profile S lists are the only valid lists the fixture has; a holder of
    // another class starts from its fresh state.
    let previous = engine.then(|| previous_list(vector, &claims)).flatten();
    if engine {
        let previous = previous.expect("the valid list the page was made from");
        hold_listed(&rig, previous);
        applied(rig.deliver(previous));
    }
    let before = rig.holder.state().clone();
    let serving_before = previous.map(|list| serving(&rig, list));
    let at_s = vector["input"]["receive_at"].as_i64().unwrap_or(iat);
    let answer = Scripted {
        url: Some(route_list_url()),
        response: HttpResponse {
            status: 200,
            content_type: Some(profile(true).list_page_content_type()),
            body: page.as_bytes().to_vec(),
            ..HttpResponse::default()
        },
    };
    let failure = failed(rig.list_with(&[text(&vector["expected_nonce"])], vec![answer], at_s));
    println!("MUST-REJECT {name:32} check={check:30} holder={failure:?}");
    assert!(
        refusal_class(check, &failure, accept_every_kind),
        "{name}: check {check} answered {failure:?}"
    );
    // Keep previous state: the durable state, and what serves.
    let after = rig.holder.state();
    let clock_hold = check == "iat_outside_window";
    assert_eq!(after.high_water, before.high_water, "{name}");
    assert_eq!(after.last_list_ref, before.last_list_ref, "{name}");
    assert_eq!(after.tombstones, before.tombstones, "{name}");
    assert_eq!(after.obligations, before.obligations, "{name}");
    assert_eq!(
        after.engine.pin_history, before.engine.pin_history,
        "{name}"
    );
    if let (Some(list), Some(serving_before)) = (previous, serving_before) {
        let serving_after = serving(&rig, list);
        if clock_hold {
            // H.36: an authority-signed page dated 121 s ahead is clock
            // evidence even though the list is refused; the hold refuses
            // every request until it clears.
            // The hold fails the request-time access check (§C.0), which
            // runs for every capability-bearing request.
            assert!(
                rig.holder
                    .raised_alerts()
                    .contains(&"clock_behind_authority")
            );
            for (id, _) in &serving_after {
                assert_eq!(
                    rig.serve_with_capability(id, at_s),
                    Err(Refusal::Engine(EngineRefusal::ClockHold)),
                    "{id}"
                );
            }
        } else {
            assert_eq!(serving_after, serving_before, "{name}: serving stands");
            assert_eq!(after.engine, before.engine, "{name}");
        }
        // The refused list renewed nothing: every lease runs down, and
        // nothing is deleted.
        assert!(serving_before.iter().any(|(_, answer)| answer.is_ok()));
        rig.advance_mono(900_000);
        for item in items(list) {
            let key = resource(&item);
            assert!(
                rig.serve_unstamped(&key.id).is_err(),
                "{name}: {} lapsed",
                key.id
            );
            assert!(rig.store.bytes_present(&key.kind, &key.id));
        }
    }
}

macro_rules! must_reject_tests {
    ($($test:ident => $name:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                must_reject($name, false);
            }
        )*
    };
}

must_reject_tests! {
    must_reject_ap17_missing => "AP17_missing",
    must_reject_ap17_malformed_pin_id => "AP17_malformed_pin_id",
    must_reject_ap17_malformed_pin_ref => "AP17_malformed_pin_ref",
    must_reject_h16_unknown_pin_member => "H16_unknown_pin_member",
    must_reject_h16_half_null_id => "H16_half_null_id",
    must_reject_h16_half_null_ref => "H16_half_null_ref",
    must_reject_h16_half_absent_id => "H16_half_absent_id",
    must_reject_h16_half_absent_ref => "H16_half_absent_ref",
    must_reject_h16_pin_on_non_engine => "H16_pin_on_non_engine",
    must_reject_h35_unknown_top_member => "H35_unknown_top_member",
    must_reject_h35_duplicate_pin_member => "H35_duplicate_pin_member",
    must_reject_h35_duplicate_header_member => "H35_duplicate_header_member",
    must_reject_h35_unknown_item_member => "H35_unknown_item_member",
    must_reject_h35_missing_subject => "H35_missing_subject",
    must_reject_h35_wrong_type => "H35_wrong_type",
    must_reject_bad_signature => "bad_signature",
    must_reject_wrong_issuer => "wrong_issuer",
    must_reject_wrong_audience => "wrong_audience",
    must_reject_wrong_subject => "wrong_subject",
    must_reject_wrong_nonce => "wrong_nonce",
    must_reject_expired => "expired",
    must_reject_future_iat => "future_iat",
    must_reject_rollback_seq => "rollback_seq",
    must_reject_impossible_page_position => "impossible_page_position",
    must_reject_repeated_holding => "repeated_holding",
    must_reject_terminating_without_obligation => "terminating_without_obligation",
}

#[test]
fn must_reject_h16_pin_on_non_room() {
    // Profile S as deployed: refused at the kind.
    must_reject("H16_pin_on_non_room", false);
    // A holder that accepts the kind reaches the §C.3 member check.
    must_reject("H16_pin_on_non_room", true);
}

/// The AP17 index names must-reject vectors this module runs.
#[test]
fn ap17_points_at_storage_pin_must_reject_vectors() {
    let names: Vec<&str> = at("AP17").as_array().unwrap().iter().map(text).collect();
    for name in names {
        let check = text(&at(&format!("must_reject.{name}"))["check"]);
        assert!(
            [
                "engine_room_missing_root_pin",
                "pin_id_grammar",
                "pin_ref_grammar"
            ]
            .contains(&check),
            "{name}: {check}"
        );
    }
}

// ------------------------------------------- interim (not a vector count)

/// Until epoch acceptance lands (S5), every list from a later epoch is
/// refused with `unexpected_epoch` (§5 "before S2 ships"): leases run down,
/// tombstones stand, nothing is deleted. Checked with the fixture's real
/// new-epoch lists; the vectors themselves stay registered as later-slice.
#[test]
fn new_epoch_lists_fail_closed_until_epoch_acceptance() {
    for (earlier, later) in [
        ("cases.V12.backup_list", "cases.V12.first_new_epoch_list"),
        ("H53.prior_list", "H53.first_current_list"),
        ("cases.V1.initial_list", "cases.V4.current_list"),
    ] {
        let (mut rig, report) = held_and_applied(at(earlier));
        assert_ne!(GoldenList::of(at(later)).rev().epoch, report.rev.epoch);
        let before = rig.holder.state().clone();
        let serving_before = serving(&rig, at(earlier));
        assert_eq!(
            failed(rig.deliver(at(later))),
            FetchFailure::UnexpectedEpoch,
            "{later}"
        );
        assert!(rig.codes().contains(&"unexpected_epoch"));
        assert_eq!(rig.holder.state(), &before, "{later}");
        assert_eq!(serving(&rig, at(earlier)), serving_before, "{later}");
        // The refused list renews nothing: the earlier list's leases lapse.
        rig.advance_mono(900_000);
        for item in items(at(earlier)) {
            let key = resource(&item);
            assert!(rig.serve_unstamped(&key.id).is_err());
            assert!(rig.store.bytes_present(&key.kind, &key.id));
        }
    }
}
