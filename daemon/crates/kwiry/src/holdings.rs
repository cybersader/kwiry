//! Composition of the generic Holdings holder (`kwiry-holdings`) for its first
//! authority, OpenClast.
//!
//! Normative source: OpenClast `.workspace/kwiry-integration/HOLDINGS-WIRE-v2.md`
//! (sha256 `a3d5220c0a2f0a1114f0ca93247f3b8b705313e029c568afc751f9c837c25ea0`).
//! Kwiry implements Part I (generic core) and Part II (Profile S, the
//! search-engine holder); Annex K maps Kwiry's ids to the generic states.
//!
//! The protocol logic is authority-neutral; this module supplies the
//! authority's wire constants (v2 §2.3, §2.4, §3.3, §C.1 and S.0). Slice 1
//! only defines them and proves the holder against the fake authority under
//! them; slice S4 wires the holder into configuration (`[holder]`), the CLI
//! (`kwiry holder …`) and the server (one driver thread per daemon).
//!
//! **This build is list-only.** It verifies and applies lists but does not
//! send status or obligation reports or evaluate purge or release instructions.
//! There is no deletion or reservation adapter.

pub(crate) mod adapters;
pub(crate) mod cli;
pub(crate) mod driver;
pub(crate) mod transport;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use kwiry_core::{Config, HolderProfile, HolderSettings};
use kwiry_holdings::clock::{Clock, SystemClock};
use kwiry_holdings::engine::{EngineProfile, EngineSettings};
use kwiry_holdings::holder::{ConfigError, DEFAULT_PAGE_SIZE, Holder, HolderConfig};
use kwiry_holdings::profile::WireProfile;
use kwiry_holdings::state::{DirError, FsIo, HolderDir};

use adapters::{ConfiguredRooms, IndexDataRoot, LogSink};
use transport::HttpTransport;

/// The holder as the daemon composes it.
pub(crate) type DaemonHolder = Holder<ConfiguredRooms, HttpTransport>;

/// Default `clock_skew_allowance` (H.21.3 caps it at 30 s).
const DEFAULT_CLOCK_SKEW_ALLOWANCE_S: u32 = 30;

/// The wire profile a configured profile value selects.
pub(crate) fn wire_profile(profile: HolderProfile) -> WireProfile {
    match profile {
        HolderProfile::OpenClast => openclast_profile(),
    }
}

/// Opens the holder directory under its exclusive lock. A held lock means
/// another process (normally the running daemon) owns the holder.
pub(crate) fn open_holder_dir(
    settings: &HolderSettings,
    config: &Config,
    data_dir: &Path,
) -> Result<HolderDir> {
    let mut forbidden: Vec<&Path> = vec![data_dir];
    forbidden.extend(config.vaults.iter().map(|vault| vault.path.as_path()));
    HolderDir::open(&settings.state_dir, &forbidden).map_err(|error| match error {
        DirError::LockHeld => anyhow!(
            "the holder state directory is locked by another process (is the daemon running?); stop it before running this command"
        ),
        other => anyhow!("holder state directory: {other}"),
    })
}

/// Starts the holder from durable state: enrollment, the loss/reset rules
/// against the index data root, and replay of every owed transition. A
/// failed replay refuses to start.
pub(crate) fn start_holder(
    settings: &HolderSettings,
    config: &Config,
    data_dir: &Path,
    clock: Arc<dyn Clock>,
    events: Arc<LogSink>,
) -> Result<DaemonHolder> {
    let dir = open_holder_dir(settings, config, data_dir)?;
    let started = kwiry_holdings::startup::start(
        dir,
        Box::new(FsIo),
        clock.clone(),
        &IndexDataRoot::new(data_dir),
        events.as_ref(),
    )
    .map_err(|error| match error {
        kwiry_holdings::startup::StartupError::RootHolderMismatch => anyhow!(
            "holder startup refused ({}): {error}. Stop the daemon, move or empty the configured data directory, then restart; the index rebuilds from the vault",
            error.code()
        ),
        other => anyhow!("holder startup refused ({}): {other}", other.code()),
    })?;
    if started.writer.enrollment().issuer != settings.issuer {
        return Err(anyhow!(
            "holder startup refused: holder.issuer does not match the enrolled issuer"
        ));
    }
    let engine = EngineSettings::new(
        settings
            .clock_skew_allowance_seconds
            .unwrap_or(DEFAULT_CLOCK_SKEW_ALLOWANCE_S),
        settings
            .max_stale_serve_seconds
            .unwrap_or(kwiry_holdings::engine::stale_serve::DEFAULT_STALE_SERVE_S),
    )
    .map_err(|reason| anyhow!("holder configuration: {reason}"))?;
    let holder_config = HolderConfig {
        profile: wire_profile(settings.profile),
        page_size: settings.page_size.unwrap_or(DEFAULT_PAGE_SIZE),
        engine: Some(engine),
    };
    Holder::new(
        started,
        holder_config,
        ConfiguredRooms::from_config(config),
        HttpTransport::new(),
        clock,
        events,
    )
    .map_err(|error| match error {
        ConfigError::CancellationRecovery => anyhow!(
            "holder startup refused: replaying owed lifecycle transitions failed; holder state is unchanged, fix storage and restart"
        ),
        other => anyhow!("holder startup refused: {other}"),
    })
}

/// A system clock for one holder process.
pub(crate) fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock::new())
}

/// OpenClast's Holdings wire profile for an engine holder that holds rooms.
pub(crate) fn openclast_profile() -> WireProfile {
    let strings = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect();
    WireProfile {
        typ_list_request: "openclast-holdings-listreq+jwt".into(),
        typ_list_page: "openclast-holdings-list+jwt".into(),
        typ_keyset: "openclast-lifecycle-keyset+jwt".into(),
        typ_report_request: "openclast-obligation-report+jwt".into(),
        typ_report_ack: "openclast-obligation-ack+jwt".into(),
        // v2 §11.4.1: the status route.
        typ_status_request: "openclast-holder-status+jwt".into(),
        typ_status_ack: "openclast-holder-status-ack+jwt".into(),
        // v2 H.5: marks `200 already_complete`.
        outcome_header: "OpenClast-Lifecycle-Outcome".into(),
        audience_prefix: "openclast-lifecycle:".into(),
        authority_key_id_prefix: "lc-".into(),
        resource_kinds: strings(&["room", "vault", "workspace", "file", "share", "backup"]),
        // v2 S.0: Profile S receives `room` items only (S1).
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

#[cfg(all(test, unix))]
mod tests {
    use super::openclast_profile;
    use kwiry_holdings::client::FetchFailure;
    use kwiry_holdings::engine::{ArtifactStamp, Cut, EngineQuery, RootObservation};
    use kwiry_holdings::gate::Refusal;
    use kwiry_holdings::holder::ListOutcome;
    use kwiry_holdings::testkit::Rig;
    use kwiry_holdings::verify::PageError;

    /// The authority's HKDF salt for `state_digest` (v2 §3.5).
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
    fn authority_specific_vectors_belong_to_the_composition_crate() {
        let provenance: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/holdings/lifecycle-vectors.v1.provenance.json"
        ))
        .unwrap();
        let profile = openclast_profile();
        assert_eq!(
            provenance["wire_profile"]["typ_status_request"],
            profile.typ_status_request
        );
        assert_eq!(
            provenance["wire_profile"]["typ_status_ack"],
            profile.typ_status_ack
        );
        // Neither the vector nor its provenance is duplicated under the holder.
        let old = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kwiry-holdings/tests/fixtures");
        for name in [
            "openclast-lifecycle-vectors.v1.json",
            "openclast-lifecycle-vectors.v1.provenance.json",
        ] {
            assert!(!old.join(name).exists());
        }
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
