//! Composition of the generic Holdings holder (`kwiry-holdings`) for its first
//! authority, OpenClast.
//!
//! The protocol logic is authority-neutral; this module supplies the
//! authority's frozen wire constants (HOLDINGS-WIRE §2.3, §2.4, §3.3, H.7 and
//! H.16). Slice 1 only defines them and proves the holder against the fake
//! authority under them; slice S4 wires the holder into configuration, the
//! CLI and the server.

use kwiry_holdings::engine::EngineProfile;
use kwiry_holdings::profile::WireProfile;

/// OpenClast's Holdings wire profile for an engine holder that holds rooms.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the daemon in slice S4")
)]
pub(crate) fn openclast_profile() -> WireProfile {
    let strings = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect();
    WireProfile {
        typ_list_request: "openclast-holdings-listreq+jwt".into(),
        typ_list_page: "openclast-holdings-list+jwt".into(),
        typ_keyset: "openclast-lifecycle-keyset+jwt".into(),
        audience_prefix: "openclast-lifecycle:".into(),
        authority_key_id_prefix: "lc-".into(),
        resource_kinds: strings(&["room", "vault", "workspace", "file", "share", "backup"]),
        // HOLDINGS-WIRE §13: an engine holder holds `{kind: room}` only.
        accepted_resource_kinds: strings(&["room"]),
        obligation_reasons: strings(&["retire", "holder-decommission"]),
        // The search-engine holder profile: room items carry root pins, and
        // capability exposure is bounded.
        engine: Some(EngineProfile {
            holder_kind: "engine".into(),
            pinned_resource_kind: "room".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::openclast_profile;
    use kwiry_holdings::client::FetchFailure;
    use kwiry_holdings::engine::{ArtifactStamp, Cut, EngineQuery, RootObservation};
    use kwiry_holdings::gate::Refusal;
    use kwiry_holdings::holder::ListOutcome;
    use kwiry_holdings::testkit::Rig;
    use kwiry_holdings::verify::PageError;

    /// The authority's HKDF salt for `state_digest` (HOLDINGS-WIRE §3.5).
    /// Only the test authority computes digests; the holder never does.
    const DIGEST_SALT: &str = "openclast-lifecycle/v1";

    fn rig() -> (tempfile::TempDir, Rig) {
        let dir = tempfile::tempdir().expect("tempdir");
        let rig = Rig::with_profile(&dir.path().join("holder"), openclast_profile(), DIGEST_SALT);
        (dir, rig)
    }

    fn serve(rig: &Rig, room: &str, pin_ref: &str) -> Result<(), Refusal> {
        rig.engine_gate().check(
            &EngineQuery {
                resource_id: room,
                capability: None,
                artifact: Some(ArtifactStamp {
                    pin_ref,
                    validated_under_matching_root: true,
                }),
                root: RootObservation::Matches,
            },
            Cut::Admission,
        )
    }

    #[test]
    fn the_profile_is_coherent() {
        assert_eq!(openclast_profile().validate(), Ok(()));
    }

    #[test]
    fn an_attested_room_serves_only_after_a_verified_list_under_the_openclast_wire() {
        let (_dir, mut rig) = rig();
        rig.store.hold("room", "room-test-1", true, 1);
        let holding = rig.authority.state().add_active("room", "room-test-1");
        let pin_ref = rig.authority.state().attest(&holding, "synthetic-root-1");
        assert_eq!(
            serve(&rig, "room-test-1", &pin_ref),
            Err(Refusal::NoListThisProcess)
        );
        assert!(matches!(rig.holder().list_once(), ListOutcome::Applied(_)));
        assert_eq!(serve(&rig, "room-test-1", &pin_ref), Ok(()));
        rig.authority.state().retire(&holding);
        assert!(matches!(rig.relist(), ListOutcome::Applied(_)));
        assert_eq!(
            serve(&rig, "room-test-1", &pin_ref),
            Err(Refusal::Tombstoned)
        );
    }

    #[test]
    fn a_room_item_without_root_pin_is_refused_whole() {
        let (_dir, mut rig) = rig();
        rig.authority.state().add_active("room", "room-test-1");
        rig.authority.state().hooks.item = Some(Box::new(|item| {
            item.remove("root_pin");
        }));
        assert_eq!(
            rig.holder().list_once(),
            ListOutcome::Failed(FetchFailure::Invalid(PageError::RootPin("member missing")))
        );
    }

    #[test]
    fn capability_and_other_lifecycle_typs_never_verify_as_a_list() {
        for typ in [
            "kwiry-cap+jwt",
            "openclast-obligation-ack+jwt",
            "openclast-holder-status-ack+jwt",
            "openclast-lifecycle-keyset+jwt",
            "openclast-lifecycle-epoch+jwt",
            "secevent+jwt",
        ] {
            let (_dir, mut rig) = rig();
            rig.authority.state().hooks.typ = Some(typ.into());
            assert_eq!(
                rig.holder().list_once(),
                ListOutcome::Failed(FetchFailure::Invalid(PageError::Typ)),
                "{typ}"
            );
        }
    }

    #[test]
    fn a_vault_item_is_refused_by_an_engine_holder_of_rooms() {
        let (_dir, mut rig) = rig();
        rig.authority.state().add_active("vault", "vault-test-1");
        assert_eq!(
            rig.holder().list_once(),
            ListOutcome::Failed(FetchFailure::Invalid(PageError::Grammar(
                "resource.kind not accepted by this holder"
            )))
        );
    }
}
