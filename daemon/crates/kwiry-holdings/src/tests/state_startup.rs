//! Holder directory, the single writer, enrollment and the loss/reset table.

use super::*;
use crate::engine::PinPair;
use crate::startup::{ContentState, StartupError};
use crate::state::{
    ApplyMutation, DirError, ENROLLMENT_FILE, FsIo, HolderDir, KEY_FILE, STATE_FILE, Tombstone,
    WriteError,
};
use crate::wire::Rev;
use std::fs;

fn start_error(rig: &mut Rig) -> crate::testkit::RigError {
    match rig.restart() {
        Err(error) => error,
        Ok(()) => panic!("start should have failed"),
    }
}

#[cfg(unix)]
#[test]
fn holder_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh-holder");
    let rig = Rig::new(&path);
    let mode = |name: &str| {
        fs::metadata(rig.dir.join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(mode(KEY_FILE), 0o600);
    assert_eq!(mode(ENROLLMENT_FILE), 0o600);
    assert_eq!(mode(STATE_FILE), 0o600);
}

#[test]
fn the_holder_directory_never_overlaps_a_data_or_index_directory() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(&data).unwrap();
    assert!(matches!(
        HolderDir::open(&data.join("holder"), &[&data]),
        Err(DirError::Overlap)
    ));
    assert!(matches!(
        HolderDir::open(dir.path(), &[&data]),
        Err(DirError::Overlap)
    ));
    let index = dir.path().join("index-not-yet-created");
    assert!(matches!(
        HolderDir::open(&index.join("h"), &[&index]),
        Err(DirError::Overlap)
    ));
    assert!(HolderDir::open(&dir.path().join("holder"), &[&data]).is_ok());
}

#[test]
fn one_writer_holds_the_lock() {
    let (_dir, rig) = rig();
    assert!(matches!(
        HolderDir::open(&rig.dir, &[]),
        Err(DirError::LockHeld)
    ));
}

#[test]
fn commits_are_whole_file_replacements_with_no_leftovers() {
    let (_dir, mut rig) = rig();
    let a = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&a, "dir-a");
    let b = rig.authority.state().add_active(PLAIN, "res-b");
    rig.authority.state().retire(&b);
    let report = applied(rig.holder().list_once());
    let on_disk =
        crate::state::HolderState::parse(&fs::read(rig.dir.join(STATE_FILE)).unwrap()).unwrap();
    assert_eq!(&on_disk, rig.holder().state());
    assert_eq!(on_disk.high_water.as_ref(), Some(&report.rev));
    assert_eq!(on_disk.tombstones.len(), 1);
    assert_eq!(on_disk.engine.pin_history.len(), 1);
    let leftovers: Vec<_> = fs::read_dir(&rig.dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn p2_high_water_tombstones_and_pin_pairs_commit_together_or_not_at_all() {
    let (_dir, mut rig) = rig();
    let a = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&a, "dir-a");
    let b = rig.authority.state().add_active(PLAIN, "res-b");
    rig.authority.state().retire(&b);
    rig.io_faults.fail_writes(1);
    assert_eq!(
        rig.holder().list_once(),
        crate::holder::ListOutcome::ApplyFailed { step: "A1" }
    );
    let on_disk =
        crate::state::HolderState::parse(&fs::read(rig.dir.join(STATE_FILE)).unwrap()).unwrap();
    assert_eq!(on_disk.high_water, None);
    assert!(on_disk.tombstones.is_empty() && on_disk.engine.pin_history.is_empty());
    assert_eq!(&on_disk, rig.holder().state(), "memory rolled back too");
}

#[test]
fn the_writer_refuses_regressions_and_foreign_epochs_and_unions_only_grow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("h");
    let mut other = Rig::new(&path);
    applied(other.holder().list_once());
    drop(other);
    let holder_dir = HolderDir::open(&path, &[]).unwrap();
    let data_root = crate::testkit::FakeDataRoot::default();
    data_root.view().marker = None;
    let events = crate::events::MemorySink::default();
    let mut started = crate::startup::start(
        holder_dir,
        Box::new(FsIo),
        std::sync::Arc::new(crate::testkit::FakeClock::new(0, 0)),
        &data_root,
        &events,
    )
    .unwrap();
    let rev = started.writer.state().high_water.clone().unwrap();
    let lower = ApplyMutation {
        rev: Rev {
            epoch: rev.epoch.clone(),
            seq: rev.seq - 1,
        },
        tombstones: Vec::new(),
        pin_pairs: Vec::new(),
        obligations: None,
    };
    assert!(matches!(
        started.writer.apply_list(lower),
        Err(WriteError::Refused(_))
    ));
    let foreign = ApplyMutation {
        rev: Rev {
            epoch: "ep_0000000000000000000099".into(),
            seq: rev.seq + 1,
        },
        tombstones: Vec::new(),
        pin_pairs: Vec::new(),
        obligations: None,
    };
    assert!(matches!(
        started.writer.apply_list(foreign),
        Err(WriteError::Refused(_))
    ));
    // Unions only grow.
    let grow = ApplyMutation {
        rev: rev.clone(),
        tombstones: vec![Tombstone {
            resource_id: "res-t".into(),
            resource_kind: PLAIN.into(),
            holding_id: "hl_0000000000000000000077".into(),
        }],
        pin_pairs: vec![PinPair {
            pin_id: crate::testkit::pin_id("x"),
            resource_id: "res-t".into(),
        }],
        obligations: None,
    };
    started.writer.apply_list(grow).unwrap();
    let empty = ApplyMutation {
        rev,
        tombstones: Vec::new(),
        pin_pairs: Vec::new(),
        obligations: None,
    };
    started.writer.apply_list(empty).unwrap();
    assert_eq!(started.writer.state().tombstones.len(), 1);
    assert_eq!(started.writer.state().engine.pin_history.len(), 1);
}

#[test]
fn ap11d_ii_trust_on_first_use_needs_state_and_data_both_proven_lost() {
    let (_dir, mut rig) = rig();
    let a = rig.authority.state().add_active(PINNED, "res-a");
    rig.authority.state().attest(&a, "dir-a");
    applied(rig.holder().list_once());
    fs::remove_file(rig.dir.join(STATE_FILE)).unwrap();
    rig.data_root.view().marker = None;
    rig.events.clear();
    rig.restart().unwrap();
    assert_eq!(
        codes(&rig),
        ["holder_state_reset", "pin_history_unavailable"]
    );
    let state = rig.holder().state().clone();
    assert!(state.reset_pending);
    assert_eq!(state.high_water, None);
    assert!(state.tombstones.is_empty() && state.engine.pin_history.is_empty());
    assert_eq!(
        rig.data_root.view().marker.as_deref(),
        Some(state.holder_id.as_str())
    );
    // The first verified list is trusted; the walk inputs say history is gone.
    applied(rig.relist());
    assert!(!rig.engine_gate().walk_inputs("res-a").history_available);
    // Every later start of a still-reset holder warns again.
    rig.events.clear();
    rig.restart().unwrap();
    assert_eq!(codes(&rig), ["pin_history_unavailable"]);
}

#[test]
fn ap11d_i_state_only_loss_stops_startup() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    fs::remove_file(rig.dir.join(STATE_FILE)).unwrap();
    // The marker still binds the root: missing state is never a reset.
    assert_eq!(start_error(&mut rig).code(), "holder_state_missing");
    // Intact content without a marker: also missing, never a reset.
    rig.data_root.view().marker = None;
    rig.data_root.view().content = ContentState::Intact;
    assert_eq!(start_error(&mut rig).code(), "root_holder_mismatch");
    assert!(!rig.dir.join(STATE_FILE).exists());
}

#[test]
fn corrupt_or_foreign_state_fails_closed_and_is_left_untouched() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    let path = rig.dir.join(STATE_FILE);
    let good = fs::read(&path).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&good).unwrap();

    for (bytes, code) in [
        (b"{not json".to_vec(), "holder_state_corrupt"),
        (
            {
                let mut v = value.clone();
                v["extra"] = 1.into();
                serde_json::to_vec(&v).unwrap()
            },
            "holder_state_corrupt",
        ),
        (
            {
                let mut v = value.clone();
                v["schema"] = "holdings-holder-state/0".into();
                serde_json::to_vec(&v).unwrap()
            },
            "holder_state_corrupt",
        ),
        (
            {
                value["holder_id"] = "hd_OTHERholder00000000001".into();
                serde_json::to_vec(&value).unwrap()
            },
            "holder_identity_mismatch",
        ),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert_eq!(start_error(&mut rig).code(), code);
        assert_eq!(fs::read(&path).unwrap(), bytes, "left untouched");
    }
    fs::write(&path, &good).unwrap();
    rig.restart().unwrap();
}

#[test]
fn root_markers_bind_the_data_root_to_one_holder() {
    let (_dir, mut rig) = rig();
    applied(rig.holder().list_once());
    rig.data_root.view().marker = Some("hd_OTHERholder00000000001".into());
    assert_eq!(start_error(&mut rig).code(), "root_holder_mismatch");
    rig.data_root.view().marker = None;
    rig.data_root.view().marker_unreadable = true;
    assert_eq!(start_error(&mut rig).code(), "root_holder_mismatch");
    rig.data_root.view().marker_unreadable = false;
    rig.data_root.view().content = ContentState::Intact;
    assert_eq!(start_error(&mut rig).code(), "root_holder_mismatch");
    rig.data_root.view().content = ContentState::Lost;
    rig.restart().unwrap();
    assert_eq!(rig.data_root.view().binds, 2, "re-bound after the wipe");
}

#[test]
fn enrollment_must_exist_and_match_the_key() {
    let (_dir, mut rig) = rig();
    let enrollment = fs::read(rig.dir.join(ENROLLMENT_FILE)).unwrap();
    fs::remove_file(rig.dir.join(ENROLLMENT_FILE)).unwrap();
    assert_eq!(start_error(&mut rig).code(), "not_enrolled");
    fs::write(rig.dir.join(ENROLLMENT_FILE), &enrollment).unwrap();
    let key = crate::keys::HolderKey::from_seed([0x01; 32]);
    fs::write(
        rig.dir.join(KEY_FILE),
        serde_json::to_vec(&key.private_jwk()).unwrap(),
    )
    .unwrap();
    assert_eq!(start_error(&mut rig).code(), "not_enrolled");
}

#[test]
fn enrollment_and_key_creation_never_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("fresh");
    let holder_dir = HolderDir::open(&fresh, &[]).unwrap();
    let key = crate::startup::create_key(&holder_dir, &FsIo).unwrap();
    assert!(matches!(
        crate::startup::create_key(&holder_dir, &FsIo),
        Err(StartupError::EnrollmentRefused(_))
    ));
    let stored = crate::keys::HolderKey::from_private_jwk(
        &serde_json::from_slice(&fs::read(fresh.join(KEY_FILE)).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(stored.thumbprint(), key.thumbprint());
    drop(holder_dir);

    let (_dir, rig) = rig();
    let path = rig.dir.clone();
    drop(rig);
    let holder_dir = HolderDir::open(&path, &[]).unwrap();
    let jwks = serde_json::json!({"keys": []});
    let refused = crate::startup::enroll(
        holder_dir,
        Box::new(FsIo),
        std::sync::Arc::new(crate::testkit::FakeClock::new(0, 0)),
        &crate::startup::EnrollParams {
            holder_id: "hd_TESTholder000000000001",
            holder_kind: "engine",
            issuer: crate::testkit::TEST_ISSUER,
            jwks: &jwks,
            authority_key_id_prefix: "ak-",
        },
    );
    assert!(matches!(
        refused,
        Err(StartupError::EnrollmentRefused("already enrolled"))
    ));
}
