//! Report vectors: V1–V6, V10, V11 (H.50), V13 and H.53's report half.
//!
//! The recorded authority answers are classified by the holder's own answer
//! path (`classify` → ack verification → receipt, convergence, rejection or
//! back-off, with their durable floors and uncertain attempts). What a
//! vector says the holder already did (sent these requests, lost `sent/`,
//! superseded an attempt) is recorded as holder-local state first.

use super::*;
use crate::obligations::Uncertain;
use crate::purge::ObligationsRefused;

fn request(path: &str) -> &'static str {
    exchange_request(at(path))
}

fn ack_of(path: &str) -> Map<String, Value> {
    claims(text(&at(path)["response"]["body"]))
}

/// The ack names the exact request bytes it answers (§4.3 `report_digest`).
#[track_caller]
fn assert_digest_names(ack: &Map<String, Value>, compact: &str) {
    assert_eq!(
        text(&ack["report_digest"]),
        sha256_tagged(compact.as_bytes())
    );
    assert_eq!(ack["report_jti"], claims(compact)["jti"]);
}

/// Records `attempts` as possibly-sent attempts of an earlier key whose
/// report the holder has since superseded (`uncertain`, §C.0).
fn record_uncertain(rig: &mut GoldenRig, requests: &[&str]) -> Uncertain {
    let pending = pending_from(requests, false);
    let uncertain = Uncertain {
        obligation_id: pending.body.obligation_id.clone(),
        holding_id: pending.body.holding_id.clone(),
        state: pending.body.state,
        epoch: pending.epoch.clone(),
        attempts: pending.attempts.clone(),
    };
    let mut book = rig.holder.state().obligations.clone();
    book.uncertain
        .insert(pending.key.clone(), uncertain.clone());
    rig.holder
        .writer
        .update_obligations(book)
        .expect("record uncertain");
    for compact in requests {
        rig.holder
            .writer
            .write_side_file(
                &crate::report::sent_file(&sha256_tagged(compact.as_bytes())),
                compact.as_bytes(),
            )
            .expect("sent bytes");
    }
    uncertain
}

fn uncertain_keys(rig: &GoldenRig) -> Vec<String> {
    rig.holder
        .state()
        .obligations
        .uncertain
        .keys()
        .cloned()
        .collect()
}

// ------------------------------------------------------------------- V1

#[test]
fn v1() {
    let case = at("cases.V1");
    let a1 = request("cases.V1.attempts.0");
    let a2 = request("cases.V1.attempts.1");
    let mut rig = GoldenRig::applied(&case["initial_list"]);
    let pending = rig.record_sent(&[a1, a2], false);
    let ob = pending.body.obligation_id.clone();
    // The answer to a2 is delivered first: the byte-identical ack of a1.
    let order: Vec<u64> = case["delivery_order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(order, [2, 1]);
    let first = &case["attempts"][1];
    let ack = claims(text(&first["response"]["body"]));
    assert_digest_names(&ack, a1);
    assert_eq!(rig.answer(&pending, first), Answer::Receipt);
    assert_eq!(text(&case["expect"]["holder"]), "replay_receipt");
    // The floor rises; neither attempt is uncertain; the receipt is the
    // exact ack bytes; the sent bytes are pruned.
    assert_eq!(case["expect"]["advance_floor"], true);
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));
    assert!(rig.pending(&ob).is_none());
    assert!(case["expect"]["unresolved"].as_array().unwrap().is_empty());
    assert!(uncertain_keys(&rig).is_empty());
    assert_eq!(
        rig.file(&format!("receipt.{}.jws", pending.key)),
        first["response"]["body"].as_str().unwrap().as_bytes()
    );
    assert!(rig.files("sent.").is_empty());
    // a1's own answer, arriving second, is the same bytes and changes
    // nothing.
    let second = &case["attempts"][0];
    assert_eq!(second["response"]["body"], first["response"]["body"]);
    let state = rig.holder.state().clone();
    assert_eq!(rig.answer(&pending, second), Answer::Receipt);
    assert_eq!(rig.holder.state().obligations, state.obligations);
}

/// The holder's own report on V1's list carries every member of the
/// recorded request, echoes the list reference verbatim (§3.6, §4.1) and
/// adds only optional members.
#[test]
fn v1_the_holders_own_report_matches_the_recorded_request() {
    let case = at("cases.V1");
    let mut rig = GoldenRig::applied(&case["initial_list"]);
    let recorded = claims(request("cases.V1.attempts.0"));
    let resource = rig.listed_resource(text(&recorded["obligation_id"]));
    rig.purger.add(&resource.kind, &resource.id, 2);
    let summary = rig
        .holder
        .report_once(&rig.purger.clone())
        .expect("reports run");
    assert_eq!(summary.sent, 1, "{summary:?}");
    let sent = rig.transport.posted_claims();
    let (url, content_type, _) = rig
        .transport
        .wire()
        .posts
        .last()
        .cloned()
        .expect("a report");
    assert_eq!(url, text(&at("cases.V1.attempts.0")["url"]));
    assert_eq!(
        content_type,
        text(&at("cases.V1.attempts.0")["request_content_type"])
    );
    let sent = sent.last().expect("a report");
    for name in recorded.keys() {
        assert!(sent.contains_key(name), "the holder's report lacks {name}");
    }
    for name in sent.keys() {
        assert!(
            recorded.contains_key(name) || ["operator", "evidence"].contains(&name.as_str()),
            "the holder's report adds {name}"
        );
    }
    for name in [
        "iss",
        "sub",
        "aud",
        "htu",
        "obligation_id",
        "holding_id",
        "list_ref",
    ] {
        assert_eq!(sent[name], recorded[name], "{name}");
    }
    assert_eq!(text(&sent["state"]), "revoked");
}

// ------------------------------------------------------------------- V2

#[test]
fn v2() {
    let case = at("cases.V2");
    let mut rig = GoldenRig::applied(&case["initial_list"]);
    // The holder lost `sent/` for K: the attempt the ack names is unknown.
    assert!(
        case["retained_report_digests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let pending = rig.record_sent(
        &[
            request("cases.V1.attempts.0"),
            request("cases.V1.attempts.1"),
        ],
        true,
    );
    let ob = pending.body.obligation_id.clone();
    assert_eq!(
        text(&claims(text(&case["response"]["body"]))["idempotency_key"]),
        pending.key
    );
    let answer = rig.holder.classify(&ob, &pending, Ok(response(case)));
    assert_eq!(text(&case["expect"]["holder"]), "ack_unverifiable");
    assert_eq!(answer, Answer::Unverifiable);
    assert!(rig.codes().contains(&"ack_unverifiable"));
    // Nothing stored as a receipt, no floor from the ack, no halt.
    assert_eq!(case["expect"]["store_receipt"], false);
    assert!(rig.files("receipt.").is_empty());
    assert_eq!(rig.floor(&ob), None);
    assert_eq!(case["expect"]["halt"], false);
    assert!(!rig.holder.state().obligations.channels[&ob].halted);
    // Convergence is floor adoption from the next list (H.37.3: "from its
    // next list"); the fixture has no later list for this holder in this
    // epoch, and floor adoption from golden lists is driven by
    // `v11_at_d_a_covering_ack_and_page_zero_both_resolve`.
    assert_eq!(case["expect"]["relist"], true);
    assert_eq!(case["expect"]["adopt_floor"], true);
}

// ------------------------------------------------------------------- V3

#[test]
fn v3() {
    let case = at("cases.V3");
    let a1 = request("cases.V1.attempts.0");
    let mut rig = GoldenRig::applied(at("cases.V1.initial_list"));
    // K was invalidated after a1 may have been sent: a1 is uncertain, and
    // the holder's live report is a later one for the same obligation (the
    // recorded `complete` request V5 sent on the same list).
    assert_eq!(
        text(&case["invalidated_attempt_digest"]),
        sha256_tagged(a1.as_bytes())
    );
    let k = pending_from(&[a1], false).key;
    record_uncertain(&mut rig, &[a1]);
    let later = rig.record_sent(&[request("cases.V5.complete")], false);
    assert_ne!(later.key, k);
    let ob = later.body.obligation_id.clone();
    // V3 is a late response to K, not an HTTP response to the later key.
    assert_eq!(
        rig.holder.delayed_report(&ob, &k, response(case)),
        Answer::Receipt
    );
    assert_eq!(text(&case["expect"]["holder"]), "replay_receipt");
    // The ack is in the current epoch: the floor rises, K is pruned, the
    // later report stays pending.
    assert_eq!(case["expect"]["advance_floor_if_current_epoch"], true);
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));
    assert_eq!(case["expect"]["prune_uncertain"], true);
    assert!(uncertain_keys(&rig).is_empty());
    assert_eq!(rig.pending(&ob).map(|p| p.key), Some(later.key));
    assert_eq!(
        rig.file(&format!("receipt.{k}.jws")),
        text(&case["response"]["body"]).as_bytes()
    );
    assert!(
        !rig.files("sent.")
            .contains(&crate::report::sent_file(&sha256_tagged(a1.as_bytes())))
    );
}

// ------------------------------------------------------------------- V4

/// Holder state in the later epoch is reached by first contact with V4's
/// current list (epoch acceptance by chain is S5).
#[test]
fn v4() {
    let case = at("cases.V4");
    let mut rig = GoldenRig::applied(&case["current_list"]);
    let epoch = GoldenList::of(&case["current_list"]).rev().epoch;
    let pending = rig.record_sent(&[exchange_request(&case["exchange"])], false);
    let ob = pending.body.obligation_id.clone();
    let floor_before = rig.holder.state().obligations.channels[&ob].floor.clone();
    // The problem body's `current_ack` is the prior epoch's ack.
    let problem = crate::parse_strict_json(text(&case["exchange"]["response"]["body"]).as_bytes())
        .expect("problem body");
    assert_eq!(problem["current_ack"], case["prior_ack"]);
    assert_ne!(
        text(&claims(text(&case["prior_ack"]))["rev"]["epoch"]),
        epoch
    );
    let answer = rig.answer(&pending, &case["exchange"]);
    assert_eq!(text(&case["expect"]["holder"]), "audit_only");
    assert_eq!(answer, Answer::Converged);
    assert!(rig.codes().contains(&"lifecycle_ack_other_epoch"));
    // No floor change; the report is settled as not accepted; relist first.
    assert_eq!(case["expect"]["advance_floor"], false);
    assert_eq!(
        rig.holder.state().obligations.channels[&ob].floor,
        floor_before
    );
    assert_eq!(
        rig.pending(&ob).map(|p| p.status),
        Some(PendingStatus::Rejected)
    );
    assert_eq!(case["expect"]["relist"], true);
    assert!(rig.holder.relist_before_report);
    assert!(rig.files("receipt.").is_empty());
}

// ------------------------------------------------------------------- V5

#[test]
fn v5() {
    let case = at("cases.V5");
    let mut rig = GoldenRig::applied(at("cases.V1.initial_list"));
    // K1 `complete` and its receipt.
    let first = rig.record_sent(&[exchange_request(&case["complete"])], false);
    let ob = first.body.obligation_id.clone();
    assert_digest_names(
        &ack_of("cases.V5.complete"),
        exchange_request(&case["complete"]),
    );
    assert_eq!(rig.answer(&first, &case["complete"]), Answer::Receipt);
    assert_eq!(rig.floor(&ob), Some(ReportState::Complete));
    // K2 `complete` answered `200` with the outcome header and K1's ack.
    let second = rig.record_sent(&[exchange_request(&case["exchange"])], false);
    assert_ne!(second.key, first.key);
    assert_eq!(case["exchange"]["response"]["outcome"], "already_complete");
    assert_eq!(
        case["exchange"]["response"]["body"],
        case["complete"]["response"]["body"]
    );
    let receipts_before = rig.files("receipt.");
    assert_eq!(rig.answer(&second, &case["exchange"]), Answer::Converged);
    assert_eq!(text(&case["expect"]["holder"]), "convergence");
    assert_eq!(case["expect"]["receipt_for_sent_key"], false);
    assert!(
        !rig.files("receipt.")
            .contains(&format!("receipt.{}.jws", second.key))
    );
    assert_eq!(rig.files("receipt."), receipts_before);
    assert_eq!(rig.files("conv.").len(), 1);
    assert!(rig.codes().contains(&"lifecycle_report_converged"));
    assert_eq!(rig.floor(&ob), Some(ReportState::Complete));
}

/// Without the header, a `200` ack under another key is still convergence
/// (§4.4, H.5).
#[test]
fn v5_an_ack_under_another_key_is_convergence_without_the_header() {
    let case = at("cases.V5");
    let mut rig = GoldenRig::applied(at("cases.V1.initial_list"));
    let second = rig.record_sent(&[exchange_request(&case["exchange"])], false);
    let mut headerless = response(&case["exchange"]);
    headerless.headers.clear();
    let ob = second.body.obligation_id.clone();
    assert_eq!(
        rig.holder.classify(&ob, &second, Ok(headerless)),
        Answer::Converged
    );
    assert!(rig.files("receipt.").is_empty());
}

// ------------------------------------------------------------------- V6

/// V6's list and report half: the incomplete report's receipt, the
/// instruction parsed and bound to that report's digest, nothing deleted.
/// The status report and release execution run in
/// `status::v6_status_and_release`.
#[test]
fn v6_lists_and_report() {
    let case = at("cases.V6");
    let mut rig = GoldenRig::applied(&case["initial_list"]);
    let unattributed: Vec<String> = (0..2).map(|_| rig.purger.add_unattributable()).collect();
    let pending = rig.record_sent(&[exchange_request(&case["report"])], false);
    let ob = pending.body.obligation_id.clone();
    assert_eq!(rig.answer(&pending, &case["report"]), Answer::Receipt);
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));

    let instructed = rig.deliver(&case["instructed_list"]);
    let ListOutcome::Applied(report) = instructed else {
        panic!("the instructed list applies: {instructed:?}");
    };
    assert_eq!(report.instructions, 1);
    let instruction =
        &claims(&GoldenList::of(&case["instructed_list"]).pages[0])["instructions"][0];
    assert_eq!(
        text(&instruction["bound_report_digest"]),
        sha256_tagged(exchange_request(&case["report"]).as_bytes()),
        "the release binds the report the holder sent"
    );
    // No release execution in this slice: purge runs, deletes nothing.
    let mut purger = rig.purger.clone();
    rig.holder.purge_detach(&mut purger).expect("purge runs");
    rig.holder
        .purge_delete_and_verify(&mut purger)
        .expect("purge runs");
    assert_eq!(case["expect"]["delete_units"], 0);
    assert!(rig.purger.deleted().is_empty());
    assert!(unattributed.iter().all(|unit| rig.purger.present(unit)));

    let ListOutcome::Applied(report) = rig.deliver(&case["following_list"]) else {
        panic!("the following list applies");
    };
    assert_eq!(report.instructions, 0, "the superseded instruction is gone");
    assert!(rig.purger.deleted().is_empty());
}

// ------------------------------------------------------------------ V10

#[test]
fn v10() {
    let case = at("cases.V10");
    let mut rig = GoldenRig::applied(&case["tagged_newer_list"]);
    let golden = GoldenList::of(&case["tagged_newer_list"]);
    let pending = rig.record_sent(&[exchange_request(&case["exchange"])], false);
    assert_eq!(
        pending.body.list_ref,
        golden.list_ref(),
        "a tag-valid ref ahead of the store"
    );
    assert!(golden.rev().seq > rev_of(&case["rolled_back_revision"]).seq);
    assert_eq!(rig.answer(&pending, &case["exchange"]), Answer::BackOff);
    assert_eq!(
        text(&case["expect"]["authority"]),
        "authority_rollback_suspected"
    );
    assert!(rig.codes().contains(&"authority_rollback_suspected"));
    // §4.5: treat as unreachable; nothing is deleted.
    let resource = rig.listed_resource(&pending.body.obligation_id);
    rig.purger.add(&resource.kind, &resource.id, 2);
    let mut purger = rig.purger.clone();
    assert_eq!(
        rig.holder.purge_detach(&mut purger),
        Err(ObligationsRefused::AuthorityRollbackSuspected)
    );
    assert!(rig.purger.deleted().is_empty());
    // The attempt stays recorded (possibly committed elsewhere).
    assert_eq!(
        rig.pending(&pending.body.obligation_id)
            .map(|p| p.attempts.len()),
        Some(1)
    );

    // The next list attempt gets the same answer: rollback suspected.
    let before = rig.holder.state().clone();
    let attempt = &case["following_list_attempt"];
    let failure = failed(rig.list_with(&[], vec![scripted(attempt)], golden.iat_s() + 60));
    assert_eq!(failure, FetchFailure::RollbackSuspected);
    assert_eq!(
        rig.holder.route_health(),
        crate::holder::RouteHealth::Normal
    );
    let sent = rig.transport.posted_claims();
    let list_request = sent.last().expect("a list request");
    assert_eq!(rev_of(&list_request["high_water"]), golden.rev());
    assert_eq!(
        list_ref_of(&list_request["last_list_ref"]),
        golden.list_ref()
    );
    assert_eq!(rig.holder.state().high_water, before.high_water);
    assert_eq!(rig.holder.state().tombstones, before.tombstones);
    // Treated as unreachable: nothing renews, nothing is deleted.
    rig.advance_mono(900_000);
    assert!(rig.serve_unstamped(&resource.id).is_err());
    assert!(rig.purger.deleted().is_empty());
}

// ----------------------------------------------------------- V11 / H.50

/// Applies the case's initial list and receives the lower report's ack.
fn v11_rig(case: &'static Value) -> (GoldenRig, String) {
    let mut rig = GoldenRig::applied(&case["initial_list"]);
    let lower = rig.record_sent(&[exchange_request(&case["lower"])], false);
    let ob = lower.body.obligation_id.clone();
    assert_digest_names(
        &claims(text(&case["lower"]["response"]["body"])),
        exchange_request(&case["lower"]),
    );
    assert_eq!(rig.answer(&lower, &case["lower"]), Answer::Receipt);
    assert_eq!(rig.floor(&ob), Some(ReportState::Revoked));
    (rig, ob)
}

/// The case's `D = exp + 120 s` is the attempt's own deadline (H.13).
fn deadline(case: &Value) -> i64 {
    let d = case["D"].as_i64().expect("D");
    let exp = claims(exchange_request(&case["attempt"]))["exp"]
        .as_i64()
        .unwrap();
    assert_eq!(d, exp + crate::report::ABSENCE_BOUND_S);
    d
}

#[test]
fn v11_at_d_a_covering_ack_and_page_zero_both_resolve() {
    let case = at("cases.V11.at_D");
    let d = deadline(case);
    assert_eq!(
        case["commit_reading"].as_i64(),
        Some(d),
        "committed at exactly D"
    );
    assert_eq!(case["expect"]["accepted"], true);
    assert_eq!(
        text(&case["expect"]["resolves_higher_attempt_by"]),
        "covering_ack"
    );
    // By the covering ack.
    let (mut rig, ob) = v11_rig(case);
    let attempt = rig.record_sent(&[exchange_request(&case["attempt"])], false);
    assert_digest_names(
        &claims(text(&case["attempt"]["response"]["body"])),
        exchange_request(&case["attempt"]),
    );
    assert_eq!(rig.answer(&attempt, &case["attempt"]), Answer::Receipt);
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));
    assert!(rig.pending(&ob).is_none());
    // By the later page 0, whose `last_accepted` covers the attempt (R15),
    // with the floor adopted from it (R13).
    let (mut rig, ob) = v11_rig(case);
    record_uncertain(&mut rig, &[exchange_request(&case["attempt"])]);
    let later = GoldenList::of(&case["later_page_zero"]);
    assert_eq!(later.rev().seq, case["seq_after"].as_u64().unwrap());
    assert!(matches!(
        rig.deliver(&case["later_page_zero"]),
        ListOutcome::Applied(_)
    ));
    assert!(uncertain_keys(&rig).is_empty());
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));
}

#[test]
fn v11_after_d_proof_expired_is_re_signed_and_page_zero_after_d_resolves() {
    let case = at("cases.V11.after_D");
    let d = deadline(case);
    assert_eq!(
        case["commit_reading"].as_i64(),
        Some(d + 1),
        "commit after D"
    );
    assert_eq!(case["expect"]["accepted"], false);
    assert_eq!(case["seq_before"], case["seq_after"], "nothing committed");
    // The answer is `401 proof_expired`: re-sign once with a fresh proof.
    let (mut rig, ob) = v11_rig(case);
    let attempt = rig.record_sent(&[exchange_request(&case["attempt"])], false);
    assert_eq!(rig.answer(&attempt, &case["attempt"]), Answer::RetryExpired);
    assert_eq!(
        rig.holder.route_health(),
        crate::holder::RouteHealth::Normal,
        "never an auth halt"
    );
    assert_eq!(rig.floor(&ob), Some(ReportState::Revoked));
    // H.50.2: a verified page 0 with `iat > D` and `last_accepted` below the
    // attempt proves it was not committed.
    assert_eq!(
        text(&case["expect"]["resolves_higher_attempt_by"]),
        "verified_page_zero_strictly_after_D_with_lower_floor"
    );
    let (mut rig, ob) = v11_rig(case);
    record_uncertain(&mut rig, &[exchange_request(&case["attempt"])]);
    let later = GoldenList::of(&case["later_page_zero"]);
    assert!(later.iat_s() > d);
    let last_accepted = &later.page0()["items"][0]["obligation"]["last_accepted"]["state"];
    assert_eq!(text(last_accepted), "revoked");
    assert!(matches!(
        rig.deliver(&case["later_page_zero"]),
        ListOutcome::Applied(_)
    ));
    assert!(uncertain_keys(&rig).is_empty(), "resolved by absence");
    assert_eq!(rig.floor(&ob), Some(ReportState::Revoked));
}

#[test]
fn v11_receipt_at_exp_plus_1_is_accepted_and_covering() {
    let case = at("cases.V11.receipt_at_exp_plus_1");
    let d = deadline(case);
    let exp = d - crate::report::ABSENCE_BOUND_S;
    assert_eq!(case["received_at"].as_i64(), Some(exp + 1));
    assert_eq!(case["expect"]["accepted"], true);
    let (mut rig, ob) = v11_rig(case);
    let attempt = rig.record_sent(&[exchange_request(&case["attempt"])], false);
    assert_eq!(rig.answer(&attempt, &case["attempt"]), Answer::Receipt);
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));
    assert!(matches!(
        rig.deliver(&case["later_page_zero"]),
        ListOutcome::Applied(_)
    ));
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));
}

/// H.50's two pointers name V11's boundary cases with the strict bound;
/// the V11 tests above drive them. The equality case (page 0 `iat == D`)
/// has no golden list and stays with the self-generated
/// `h50_a_lower_state_waits_until_page_zero_iat_passes_d`.
#[test]
fn h50_absence_needs_page_zero_strictly_after_d() {
    let h50 = at("H50");
    assert_eq!(follow(&h50["at_D"]), at("cases.V11.at_D"));
    assert_eq!(follow(&h50["at_D_plus_1"]), at("cases.V11.after_D"));
    assert_eq!(text(&h50["deadline"]), "exp + 120");
    assert_eq!(
        h50["absence_requires_page_zero_iat_strictly_greater_than_D"],
        true
    );
    for path in [
        "cases.V11.at_D",
        "cases.V11.after_D",
        "cases.V11.receipt_at_exp_plus_1",
    ] {
        let case = at(path);
        let d = deadline(case);
        let attempt = pending_from(&[exchange_request(&case["attempt"])], false);
        let latest = attempt.attempts.iter().map(|a| a.exp_s).max().unwrap();
        assert_eq!(latest + crate::report::ABSENCE_BOUND_S, d, "{path}");
        assert_eq!(
            GoldenList::of(&case["later_page_zero"]).iat_s(),
            d + 1,
            "{path}"
        );
    }
}

// ------------------------------------------------------------------ V13

#[test]
fn v13() {
    let case = at("cases.V13");
    let mut rig = GoldenRig::applied(&case["list"]);
    let unattributed: Vec<String> = (0..2).map(|_| rig.purger.add_unattributable()).collect();
    let item = &GoldenList::of(&case["list"]).page0()["items"][0];
    assert_eq!(text(&item["obligation"]["state"]), "blocked");
    let id = text(&item["resource"]["id"]);
    assert_eq!(text(&case["expect"]["holder"]), "terminating");
    let placement = &rig.holder.applied.as_ref().unwrap().plan.dispositions;
    assert!(
        placement
            .values()
            .all(|p| matches!(p, crate::store::Disposition::Obligated { .. }))
    );
    assert_eq!(rig.serve_unstamped(id), Err(Refusal::Tombstoned));
    assert!(rig.holder.state().is_tombstoned(id));
    // Its incomplete report's ack is a receipt like any other.
    let pending = rig.record_sent(&[exchange_request(&case["report"])], false);
    assert_eq!(rig.answer(&pending, &case["report"]), Answer::Receipt);
    // `blocked` authorizes nothing: purge runs and deletes nothing.
    let mut purger = rig.purger.clone();
    rig.holder.purge_detach(&mut purger).expect("purge runs");
    rig.holder
        .purge_delete_and_verify(&mut purger)
        .expect("purge runs");
    assert_eq!(case["expect"]["delete_units"], 0);
    assert!(rig.purger.deleted().is_empty());
    assert!(unattributed.iter().all(|unit| rig.purger.present(unit)));
}

// ------------------------------------------------------------------ H.53

/// An earlier-epoch report refused at commit with `409 stale_list_ref` is
/// definitively not accepted: the attempt is resolved (counts as never
/// sent), and the holder relists before re-reporting (§4.5).
#[test]
fn h53_refused_report() {
    let h53 = at("H53");
    let mut rig = GoldenRig::applied(&h53["prior_list"]);
    let refused = &h53["refused"]["report"];
    let pending = rig.record_sent(&[exchange_request(refused)], false);
    let ob = pending.body.obligation_id.clone();
    assert_eq!(
        pending.body.list_ref,
        GoldenList::of(&h53["prior_list"]).list_ref()
    );
    assert_eq!(h53["expect"]["status"], refused["response"]["status"]);
    assert_eq!(
        crate::parse_strict_json(text(&refused["response"]["body"]).as_bytes()).unwrap()["code"],
        h53["expect"]["code"]
    );
    assert_eq!(rig.answer(&pending, refused), Answer::Rejected);
    assert_eq!(h53["expect"]["committed"], false);
    let after = rig.pending(&ob).expect("the settled report");
    assert_eq!(after.status, PendingStatus::Rejected);
    assert!(
        after.attempts.is_empty(),
        "resolved: nothing is possibly sent"
    );
    assert!(uncertain_keys(&rig).is_empty());
    assert_eq!(h53["expect"]["earlier_attempt_resolved_before_D"], true);
    assert!(
        claims(exchange_request(refused))["exp"].as_i64().unwrap() + crate::report::ABSENCE_BOUND_S
            <= h53["D"].as_i64().unwrap()
    );
    assert!(rig.holder.relist_before_report);
    assert!(rig.files("sent.").is_empty());
    assert_eq!(rig.floor(&ob), None);
}

/// The current-epoch control: a report citing the first current-epoch list
/// is accepted, and its ack is a receipt. Holder state in that epoch is
/// reached by first contact with the list (epoch acceptance is S5).
#[test]
fn h53_accepted_current_report() {
    let h53 = at("H53");
    let mut rig = GoldenRig::applied(&h53["first_current_list"]);
    let accepted = &h53["accepted_current"]["report"];
    let pending = rig.record_sent(&[exchange_request(accepted)], false);
    let ob = pending.body.obligation_id.clone();
    assert_eq!(
        pending.body.list_ref,
        GoldenList::of(&h53["first_current_list"]).list_ref()
    );
    assert_digest_names(
        &claims(text(&accepted["response"]["body"])),
        exchange_request(accepted),
    );
    assert_eq!(rig.answer(&pending, accepted), Answer::Receipt);
    assert_eq!(rig.floor(&ob), Some(ReportState::Incomplete));
}

#[test]
fn different_request_key_with_an_uncertain_ack_is_convergence() {
    let case = at("cases.V3");
    let earlier = request("cases.V1.attempts.0");
    let mut rig = GoldenRig::applied(at("cases.V1.initial_list"));
    record_uncertain(&mut rig, &[earlier]);
    let later = rig.record_sent(&[request("cases.V5.complete")], false);
    let earlier_key = pending_from(&[earlier], false).key;
    assert_ne!(earlier_key, later.key);
    assert!(response(case).headers.is_empty());
    assert_eq!(rig.answer(&later, case), Answer::Converged);
    assert!(rig.files("receipt.").is_empty());
    assert_eq!(rig.files("conv.").len(), 1);
}
