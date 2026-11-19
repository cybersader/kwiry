//! Status and release vectors: V6's status and release half, V7, V8, V9,
//! V12's first new-epoch status and H.53's status pair.
//!
//! Release instructions are checked by the holder's own release pass
//! against its own census and its own manifest index. The holder's own
//! status request is compared with the recorded one member by member; the
//! recorded ack, which names the recorded request's exact bytes, is
//! classified against that request recorded as the holder's pending status
//! (labelled holder-local state, as for the report vectors).

use super::*;
use crate::report::Answer;
use crate::status::{
    HolderStateClaim, ManifestRef, ReleaseOutcome, ReleaseResult, ReleaseStep, StatusBody,
    StatusPending,
};
use std::collections::BTreeSet;

/// The holder-local record of a status the vector says this holder sent:
/// one pending status under the request's key with one attempt per request,
/// and each request's exact bytes.
fn record_status_sent(rig: &mut GoldenRig, requests: &[&str]) -> StatusPending {
    let first = claims(requests[0]);
    for request in requests {
        let mut a = first.clone();
        let mut b = claims(request);
        for volatile in ["jti", "iat", "exp"] {
            a.remove(volatile);
            b.remove(volatile);
        }
        assert_eq!(a, b, "attempts of one key carry one body");
    }
    let release_result = first.get("release_result").map(|r| ReleaseResult {
        instruction_id: text(&r["instruction_id"]).to_owned(),
        result: match text(&r["result"]) {
            "executed" => ReleaseOutcome::Executed,
            "mismatch" => ReleaseOutcome::Mismatch,
            other => panic!("result {other}"),
        },
        observed_manifest_digest: text(&r["observed_manifest_digest"]).to_owned(),
    });
    let body = StatusBody {
        list_ref: list_ref_of(&first["list_ref"]),
        quarantined_unattributed: first["quarantined_unattributed"].as_u64().unwrap(),
        quarantine_manifest_digest: text(&first["quarantine_manifest_digest"]).to_owned(),
        holder_state: HolderStateClaim::from_wire(text(&first["holder_state"])).unwrap(),
        release_result,
    };
    let pending = StatusPending {
        key: text(&first["idempotency_key"]).to_owned(),
        epoch: body.list_ref.rev.epoch.clone(),
        body,
        status: crate::obligations::PendingStatus::Armed,
        attempts: requests.iter().map(|r| attempt_of(r)).collect(),
    };
    let mut status = rig.holder.state().status.clone();
    status.pending = Some(pending.clone());
    rig.holder
        .writer
        .update_lifecycle(None, Some(status), false)
        .expect("record the pending status");
    for compact in requests {
        rig.holder
            .writer
            .write_side_file(
                &crate::report::sent_file(&sha256_tagged(compact.as_bytes())),
                compact.as_bytes(),
            )
            .expect("sent bytes");
    }
    pending
}

/// Records, as holder-local state, a manifest the vector says this holder
/// retained for the report whose exact bytes hash to `digest`.
fn retain_manifest(rig: &mut GoldenRig, digest: &str, units: &BTreeSet<String>) {
    let holder_id = rig.holder.writer.enrollment().holder_id.clone();
    let key = "ik_goldenRetainedManifest00000000";
    rig.holder
        .writer
        .write_side_file(
            &format!("manifest.{key}.json"),
            &crate::obligations::manifest_bytes(&holder_id, units),
        )
        .expect("manifest");
    let mut status = rig.holder.state().status.clone();
    status.manifests.insert(
        digest.to_owned(),
        ManifestRef {
            key: key.to_owned(),
            manifest_digest: crate::obligations::manifest_digest(&holder_id, units),
            recorded_ms: rig.clock_wall_ms(),
        },
    );
    rig.holder
        .writer
        .update_lifecycle(None, Some(status), false)
        .expect("index");
}

impl GoldenRig {
    fn clock_wall_ms(&self) -> i64 {
        use crate::clock::Clock;
        self.clock.wall_ms()
    }

    fn status_pass(&mut self) -> crate::status::StatusSummary {
        let purger = self.purger.clone();
        self.holder.status_once(&purger).expect("status runs")
    }

    fn release_pass(&mut self) -> crate::release::ReleaseSummary {
        let mut purger = self.purger.clone();
        self.holder.release_once(&mut purger).expect("release runs")
    }

    fn answer_status(&mut self, pending: &StatusPending, exchange: &Value) -> Answer {
        self.holder.classify_status(pending, Ok(response(exchange)))
    }

    fn instruction_step(&self, id: &str) -> Option<ReleaseStep> {
        self.holder
            .state()
            .status
            .instructions
            .get(id)
            .map(|r| r.step)
    }
}

/// The holder's own status request has exactly the recorded request's
/// members, on the recorded route and media type.
#[track_caller]
fn assert_own_status_matches(rig: &GoldenRig, recorded: &Value) -> Map<String, Value> {
    let (url, content_type, body) = rig
        .transport
        .wire()
        .posts
        .last()
        .cloned()
        .expect("a status request");
    assert_eq!(url, text(&recorded["url"]));
    assert_eq!(content_type, text(&recorded["request_content_type"]));
    let own = claims(std::str::from_utf8(&body).unwrap());
    let theirs = claims(exchange_request(recorded));
    let mut a: Vec<&String> = own.keys().collect();
    let mut b: Vec<&String> = theirs.keys().collect();
    a.sort();
    b.sort();
    assert_eq!(a, b, "the same member set");
    for name in ["iss", "sub", "aud", "htu", "holder_state"] {
        assert_eq!(own[name], theirs[name], "{name}");
    }
    assert_eq!(
        header(std::str::from_utf8(&body).unwrap())["typ"],
        header(exchange_request(recorded))["typ"]
    );
    own
}

/// V6's list and report half, then the instructed list applied.
fn v6_instructed(rig_units: usize) -> (GoldenRig, Vec<String>) {
    let case = at("cases.V6");
    let mut rig = GoldenRig::applied(&case["initial_list"]);
    let units: Vec<String> = (0..rig_units)
        .map(|_| rig.purger.add_unattributable())
        .collect();
    let pending = rig.record_sent(&[exchange_request(&case["report"])], false);
    assert_eq!(rig.answer(&pending, &case["report"]), Answer::Receipt);
    assert!(matches!(
        rig.deliver(&case["instructed_list"]),
        ListOutcome::Applied(_)
    ));
    (rig, units)
}

/// V6's recorded `quarantine_release` entry.
fn instruction() -> Value {
    let page = &GoldenList::of(at("cases.V6.instructed_list")).pages[0];
    claims(page)["instructions"][0].clone()
}

/// The release pass found a mismatch for V6's instruction, deleted
/// nothing, and the holder's own status reports it with its current
/// manifest digest.
#[track_caller]
fn assert_mismatch_reported(rig: &mut GoldenRig, units: &[String], case: &Value) {
    let ins = text(&instruction()["instruction_id"]).to_owned();
    let pass = rig.release_pass();
    assert_eq!((pass.evaluated, pass.mismatched, pass.deleted), (1, 1, 0));
    assert_eq!(rig.instruction_step(&ins), Some(ReleaseStep::Mismatch));
    assert_eq!(case["expect"]["delete_units"], 0);
    assert!(rig.purger.deleted().is_empty());
    assert!(units.iter().all(|u| rig.purger.present(u)));
    assert!(!rig.purger.view().calls.contains(&"reserve"));
    // The holder's own status: `mismatch`, observed = its current digest.
    let summary = rig.status_pass();
    assert_eq!(summary.sent, 1, "{summary:?}");
    let own = assert_own_status_matches(rig, at("cases.V6.status"));
    assert_eq!(text(&case["expect"]["status_result"]), "mismatch");
    assert_eq!(own["release_result"]["result"], "mismatch");
    assert_eq!(own["release_result"]["instruction_id"], ins.as_str());
    assert_eq!(
        own["release_result"]["observed_manifest_digest"],
        own["quarantine_manifest_digest"]
    );
    assert_eq!(
        text(&case["expect"]["instruction_disposition"]),
        "superseded_mismatch"
    );
    assert_eq!(case["expect"]["reapproval"], true);
}

// ------------------------------------------------------------------- V6

#[test]
fn v6_status_and_release() {
    let case = at("cases.V6");
    // No manifest is retained for the bound digest.
    assert!(case["retained_manifest"].is_null());
    let (mut rig, units) = v6_instructed(2);
    assert_eq!(
        text(&instruction()["bound_report_digest"]),
        sha256_tagged(exchange_request(&case["report"]).as_bytes())
    );
    assert!(
        rig.holder
            .retained_manifest(text(&instruction()["bound_report_digest"]))
            .is_none()
    );
    assert_mismatch_reported(&mut rig, &units, case);
    // The recorded ack answers the recorded request: a receipt that
    // finalizes the mismatch (L8) with disposition `applied`.
    let recorded = &case["status"];
    let ack = claims(text(&recorded["response"]["body"]));
    assert_eq!(ack["release_result_disposition"], "applied");
    assert_eq!(
        text(&ack["report_digest"]),
        sha256_tagged(exchange_request(recorded).as_bytes())
    );
    let pending = record_status_sent(&mut rig, &[exchange_request(recorded)]);
    assert_eq!(rig.answer_status(&pending, recorded), Answer::Receipt);
    let ins = text(&instruction()["instruction_id"]).to_owned();
    assert_eq!(
        rig.instruction_step(&ins),
        Some(ReleaseStep::MismatchReported)
    );
    assert!(rig.holder.state().status.pending.is_none());
    assert_eq!(
        rig.file(&format!("sreceipt.{}.jws", pending.key)),
        text(&recorded["response"]["body"]).as_bytes()
    );
    // The following list no longer carries it, and nothing was deleted.
    let ListOutcome::Applied(report) = rig.deliver(&case["following_list"]) else {
        panic!("the following list applies");
    };
    assert_eq!(report.instructions, 0);
    assert_eq!(
        rig.release_pass(),
        crate::release::ReleaseSummary::default()
    );
    assert!(rig.purger.deleted().is_empty());
}

// ------------------------------------------------------------------- V7

#[test]
fn v7() {
    let case = at("cases.V7");
    assert_eq!(text(&case["inherited_artifacts"]), "V6");
    let retained = &case["retained_manifest"];
    let count = usize::try_from(retained["unit_count"].as_u64().unwrap()).unwrap();
    let (mut rig, units) = v6_instructed(count);
    // The manifest exists for the bound digest and the census still holds
    // every unit; only the count differs from `approved_count`.
    assert_eq!(
        retained["bound_report_digest"],
        instruction()["bound_report_digest"]
    );
    assert_ne!(
        instruction()["approved_count"].as_u64(),
        retained["unit_count"].as_u64()
    );
    let manifest: BTreeSet<String> = units.iter().cloned().collect();
    retain_manifest(&mut rig, text(&retained["bound_report_digest"]), &manifest);
    assert_eq!(
        rig.holder
            .retained_manifest(text(&retained["bound_report_digest"])),
        Some(manifest)
    );
    assert_mismatch_reported(&mut rig, &units, case);
}

// ------------------------------------------------------------------- V8

#[test]
fn v8() {
    let case = at("cases.V8");
    assert_eq!(text(&case["inherited_artifacts"]), "V6");
    let retained = &case["retained_manifest"];
    assert_eq!(retained["unit_count"], instruction()["approved_count"]);
    assert_eq!(retained["reclassified_units"], 1);
    let (mut rig, mut units) = v6_instructed(1);
    // The second manifest unit is now attributed to the listed holding.
    let item = &GoldenList::of(&at("cases.V6")["instructed_list"]).page0()["items"][0];
    let resource = item["resource"].clone();
    let reclassified = rig
        .purger
        .add(text(&resource["kind"]), text(&resource["id"]), 1)
        .remove(0);
    units.push(reclassified);
    let manifest: BTreeSet<String> = units.iter().cloned().collect();
    retain_manifest(&mut rig, text(&retained["bound_report_digest"]), &manifest);
    assert_mismatch_reported(&mut rig, &units, case);
}

// ------------------------------------------------------------------- V9

#[test]
fn v9() {
    let case = at("cases.V9");
    let mut rig = GoldenRig::applied(at("cases.V6.following_list"));
    let request = claims(exchange_request(&case["exchange"]));
    let result = &request["release_result"];
    let ins = text(&result["instruction_id"]).to_owned();
    // Holder-local state the vector describes: this holder executed that
    // instruction and owes its result.
    let mut status = rig.holder.state().status.clone();
    status.instructions.insert(
        ins.clone(),
        crate::status::InstructionRecord {
            bound_report_digest: format!("sha256:{}", "0".repeat(64)),
            approved_count: 2,
            step: ReleaseStep::Deleted,
            units: BTreeSet::new(),
            claimed: BTreeSet::new(),
            deleted: BTreeSet::new(),
            observed_manifest_digest: Some(text(&result["observed_manifest_digest"]).to_owned()),
            carried_epoch: None,
            order: 0,
        },
    );
    rig.holder
        .writer
        .update_lifecycle(None, Some(status), false)
        .unwrap();
    assert_eq!(
        rig.holder.owed_release_result().map(|r| r.instruction_id),
        Some(ins.clone())
    );
    let pending = record_status_sent(&mut rig, &[exchange_request(&case["exchange"])]);
    assert_eq!(
        rig.answer_status(&pending, &case["exchange"]),
        Answer::Receipt
    );
    assert_eq!(case["expect"]["accepted"], true);
    assert_eq!(
        text(&case["expect"]["release_result_disposition"]),
        "unknown_instruction"
    );
    assert!(rig.codes().contains(&"lifecycle_release_result_audit"));
    // Accepted: the result is carried, so it is never reported again.
    assert_eq!(rig.instruction_step(&ins), Some(ReleaseStep::Done));
    assert!(rig.holder.owed_release_result().is_none());
}

// ------------------------------------------------------------------ V12

/// After the epoch acceptance (S5 consumes it; recorded here as the gate's
/// armed epoch), the holder sends a status at its first opportunity
/// whatever its count (H.19). Its current-epoch receipt opens the gate.
#[test]
fn v12_first_new_epoch_status() {
    let case = at("cases.V12");
    let list = &case["first_new_epoch_list"];
    let mut rig = GoldenRig::applied(list);
    let golden = GoldenList::of(list);
    let epoch = golden.rev().epoch;
    assert!(rig.holder.arm_status_gate(&epoch));
    assert!(!rig.holder.state().obligations.status_gate.open());
    let recorded = &case["first_new_epoch_status"];
    let request = claims(exchange_request(recorded));
    let count = usize::try_from(request["quarantined_unattributed"].as_u64().unwrap()).unwrap();
    for _ in 0..count {
        rig.purger.add_unattributable();
    }
    let summary = rig.status_pass();
    assert_eq!((summary.derived, summary.sent), (1, 1));
    let own = assert_own_status_matches(&rig, recorded);
    assert_eq!(list_ref_of(&own["list_ref"]), golden.list_ref());
    assert_eq!(own["list_ref"], request["list_ref"]);
    assert_eq!(
        own["quarantined_unattributed"],
        request["quarantined_unattributed"]
    );
    assert!(own.get("release_result").is_none());
    // The recorded ack, minted in the new epoch, is a receipt.
    let ack = claims(text(&recorded["response"]["body"]));
    assert_eq!(text(&ack["rev"]["epoch"]), epoch);
    assert!(ack.get("release_result_disposition").is_none());
    let pending = record_status_sent(&mut rig, &[exchange_request(recorded)]);
    assert_eq!(rig.answer_status(&pending, recorded), Answer::Receipt);
    let gate = &rig.holder.state().obligations.status_gate;
    assert_eq!(gate.acked_epoch.as_deref(), Some(epoch.as_str()));
    assert!(gate.open());
    let accepted = rig.holder.state().status.last_accepted.clone().unwrap();
    assert_eq!(accepted.rev, rev_of(&ack["rev"]));
    assert_eq!(accepted.report_digest, text(&ack["report_digest"]));
}

// ------------------------------------------------------------------ H.53

/// An earlier-epoch status refused at commit with `409 stale_list_ref` is
/// definitively not accepted: never a halt, the attempt is resolved and the
/// holder relists before deriving again.
#[test]
fn h53_refused_status() {
    let h53 = at("H53");
    let mut rig = GoldenRig::applied(&h53["prior_list"]);
    let refused = &h53["refused"]["status"];
    let pending = record_status_sent(&mut rig, &[exchange_request(refused)]);
    assert_eq!(
        pending.body.list_ref,
        GoldenList::of(&h53["prior_list"]).list_ref()
    );
    assert_eq!(h53["expect"]["status"], refused["response"]["status"]);
    assert_eq!(
        crate::parse_strict_json(text(&refused["response"]["body"]).as_bytes()).unwrap()["code"],
        h53["expect"]["code"]
    );
    assert_eq!(rig.answer_status(&pending, refused), Answer::Rejected);
    assert_eq!(h53["expect"]["committed"], false);
    let state = rig.holder.state().status.clone();
    let after = state.pending.expect("the settled status");
    assert_eq!(after.status, crate::obligations::PendingStatus::Rejected);
    assert!(after.attempts.is_empty(), "resolved: nothing possibly sent");
    assert!(state.uncertain.is_empty());
    assert!(!state.halted);
    assert!(rig.holder.relist_before_report);
    assert!(rig.files("sent.").is_empty());
    assert!(
        rig.holder
            .state()
            .obligations
            .status_gate
            .acked_epoch
            .is_none()
    );
}

/// The current-epoch control: a status citing the first current-epoch list
/// is accepted, and its ack is a receipt that opens the gate.
#[test]
fn h53_accepted_current_status() {
    let h53 = at("H53");
    let mut rig = GoldenRig::applied(&h53["first_current_list"]);
    let golden = GoldenList::of(&h53["first_current_list"]);
    assert!(rig.holder.arm_status_gate(&golden.rev().epoch));
    let accepted = &h53["accepted_current"]["status"];
    let pending = record_status_sent(&mut rig, &[exchange_request(accepted)]);
    assert_eq!(pending.body.list_ref, golden.list_ref());
    let ack = claims(text(&accepted["response"]["body"]));
    assert_eq!(
        text(&ack["report_digest"]),
        sha256_tagged(exchange_request(accepted).as_bytes())
    );
    assert_eq!(rig.answer_status(&pending, accepted), Answer::Receipt);
    assert!(rig.holder.state().obligations.status_gate.open());
    assert_eq!(
        rig.holder.state().obligations.status_gate.acked_epoch,
        Some(golden.rev().epoch)
    );
}
