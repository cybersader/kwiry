use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::format::SourceFormat;
use crate::model::{CHUNKING_VERSION, IndexFreshnessBasis, SourceFormatCounts};
use crate::policy::{
    ExtractionProfile, active_extraction_policy, active_format_identities,
    extraction_policy_fingerprint,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndexFreshnessState {
    Current,
    Reconciling,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexFreshness {
    pub state: IndexFreshnessState,
    pub basis: IndexFreshnessBasis,
}

impl IndexFreshness {
    pub const fn new(state: IndexFreshnessState, basis: IndexFreshnessBasis) -> Self {
        Self { state, basis }
    }

    pub const fn strict_hash(state: IndexFreshnessState) -> Self {
        Self {
            state,
            basis: IndexFreshnessBasis::StrictHash,
        }
    }

    pub fn header_value(self) -> &'static str {
        match (self.state, self.basis) {
            (IndexFreshnessState::Current, IndexFreshnessBasis::StrictHash) => {
                "current; basis=strict_hash"
            }
            (IndexFreshnessState::Reconciling, IndexFreshnessBasis::StrictHash) => {
                "reconciling; basis=strict_hash"
            }
            (IndexFreshnessState::Stale, IndexFreshnessBasis::StrictHash) => {
                "stale; basis=strict_hash"
            }
            (IndexFreshnessState::Unavailable, IndexFreshnessBasis::StrictHash) => {
                "unavailable; basis=strict_hash"
            }
            (IndexFreshnessState::Current, IndexFreshnessBasis::MetadataAudit) => {
                "current; basis=metadata_audit"
            }
            (IndexFreshnessState::Reconciling, IndexFreshnessBasis::MetadataAudit) => {
                "reconciling; basis=metadata_audit"
            }
            (IndexFreshnessState::Stale, IndexFreshnessBasis::MetadataAudit) => {
                "stale; basis=metadata_audit"
            }
            (IndexFreshnessState::Unavailable, IndexFreshnessBasis::MetadataAudit) => {
                "unavailable; basis=metadata_audit"
            }
            (IndexFreshnessState::Current, IndexFreshnessBasis::ProducerManifest) => {
                "current; basis=producer_manifest"
            }
            (IndexFreshnessState::Reconciling, IndexFreshnessBasis::ProducerManifest) => {
                "reconciling; basis=producer_manifest"
            }
            (IndexFreshnessState::Stale, IndexFreshnessBasis::ProducerManifest) => {
                "stale; basis=producer_manifest"
            }
            (IndexFreshnessState::Unavailable, IndexFreshnessBasis::ProducerManifest) => {
                "unavailable; basis=producer_manifest"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DaemonState {
    #[default]
    Starting,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelStatus {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultStatus {
    pub vault_id: String,
    pub room: Option<String>,
    pub documents: usize,
    pub chunks: usize,
    pub last_sync: Option<String>,
    pub dirty: bool,
    pub warning_count: usize,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub state: DaemonState,
    pub version: String,
    pub generation: Option<String>,
    pub chunking_version: u64,
    /// The extraction-policy identity this daemon compiles. A caller comparing
    /// it against its own gets an explicit signal that it is reading an index
    /// built under a different extractor **tier**, instead of rendering those
    /// results as equivalent to its own. This is the only place cross-*process*
    /// mistaken identity is addressable; the manifest gates cover only
    /// cross-*build* identity inside one store.
    ///
    /// Report-only since the split-identity wave: it is derived from the
    /// running build, never persisted, and it gates nothing. What decides reuse
    /// is `policy::format_identity_fingerprint`, compared per manifest row.
    ///
    /// **Tier only.** Its material is the schema version and every
    /// `format=profile` pair, and nothing else — so two builds whose
    /// `extractor_version_for` differ report byte-identical fingerprints while
    /// extracting the same format differently. That is precisely the
    /// cross-process mistaken identity this field exists to catch, which is why
    /// `format_identities` is served beside it rather than this digest being
    /// widened: folding the versions in here would move the value on every
    /// bump, and every caller mirroring it would have to re-pin for a fact it
    /// can read directly and attribute to a format.
    pub extraction_policy_fingerprint: String,
    /// Per-format profile behind that fingerprint, so the signal is diagnosable
    /// and not merely a hex mismatch.
    pub extraction_policy: BTreeMap<SourceFormat, ExtractionProfile>,
    /// The per-format identity this daemon compiles — format, tier, **and
    /// extractor version** — which is the value the manifest actually compares
    /// per row.
    ///
    /// This is the only field on this surface that moves when an extractor
    /// version is bumped, and it names *which* format moved rather than
    /// reporting an undifferentiated digest mismatch. A caller holding results
    /// from two daemons, or a plugin comparing itself against one, can say
    /// "markdown was extracted differently" instead of "something differs".
    pub format_identities: BTreeMap<SourceFormat, String>,
    pub documents: usize,
    pub chunks: usize,
    pub source_format_counts: SourceFormatCounts,
    pub last_sync: Option<String>,
    pub dirty: bool,
    pub rebuilding: bool,
    pub model: Option<ModelStatus>,
    pub vaults: Vec<VaultStatus>,
}

/// `active_format_identities` borrows `'static` strings; the status record owns
/// its values because it is serialized and cloned across task boundaries.
pub fn owned_format_identities() -> BTreeMap<SourceFormat, String> {
    active_format_identities()
        .into_iter()
        .map(|(format, identity)| (format, identity.to_owned()))
        .collect()
}

impl DaemonStatus {
    pub fn starting(version: impl Into<String>) -> Self {
        Self {
            state: DaemonState::Starting,
            version: version.into(),
            generation: None,
            chunking_version: CHUNKING_VERSION,
            extraction_policy_fingerprint: extraction_policy_fingerprint().to_owned(),
            extraction_policy: active_extraction_policy(),
            format_identities: owned_format_identities(),
            documents: 0,
            chunks: 0,
            source_format_counts: SourceFormatCounts::default(),
            last_sync: None,
            dirty: true,
            rebuilding: false,
            model: None,
            vaults: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_hash_freshness_has_a_stable_header_value() {
        assert_eq!(
            IndexFreshness::strict_hash(IndexFreshnessState::Current).header_value(),
            "current; basis=strict_hash"
        );
        assert_eq!(
            IndexFreshness::strict_hash(IndexFreshnessState::Reconciling).header_value(),
            "reconciling; basis=strict_hash"
        );
    }

    #[test]
    fn lexical_status_reports_no_semantic_model() {
        let status = DaemonStatus::starting("0.1.0");
        let encoded = serde_json::to_value(status).unwrap();
        assert_eq!(encoded["state"], "starting");
        assert!(encoded["model"].is_null());
        assert_eq!(encoded["chunking_version"], CHUNKING_VERSION);
        assert_eq!(
            encoded["extraction_policy_fingerprint"],
            extraction_policy_fingerprint()
        );
        assert_eq!(
            encoded["extraction_policy"]["markdown"],
            ExtractionProfile::Portable.as_str()
        );
        assert_eq!(
            encoded["source_format_counts"]["markdown"]["indexed-complete"],
            0
        );
    }

    /// The observability gap the split-identity wave left open.
    ///
    /// `extraction_policy_fingerprint` digests the schema version and every
    /// `format=profile` pair — and nothing else. So two builds that extract
    /// markdown differently, differing only in `extractor_version_for`, report
    /// the *same* fingerprint and the same policy map, and a caller comparing
    /// this surface against its own sees no difference at all. That is exactly
    /// the cross-process mistaken identity the field's own doc comment claims
    /// it catches.
    ///
    /// `format_identities` is what closes it, and it closes it by naming the
    /// format rather than by moving an opaque digest.
    #[test]
    fn only_the_identity_map_can_report_an_extractor_version_bump() {
        let encoded = serde_json::to_value(DaemonStatus::starting("0.1.0")).unwrap();

        for spec in crate::format::format_specs() {
            let reported = encoded["format_identities"][spec.name]
                .as_str()
                .unwrap_or_else(|| panic!("{} identity is served", spec.name));
            assert_eq!(
                reported,
                crate::policy::format_identity_fingerprint(spec.format)
            );

            // A bump moves this format's served identity and nothing else on
            // the surface. Derived, so it states the version difference rather
            // than asserting it about an arbitrary string.
            let bumped = crate::policy::identity_at_extractor_version(
                spec.format,
                crate::policy::extractor_version_for(spec.format) + 1,
            );
            assert_ne!(
                reported, bumped,
                "{} identity must move on a bump",
                spec.name
            );
            // The tier is unchanged by a version bump, so neither the policy
            // map nor its digest can see one. Asserted, not assumed: if a later
            // change folds the versions into that digest, this test says so.
            assert_eq!(
                encoded["extraction_policy"][spec.name],
                crate::policy::extraction_profile_for(spec.format).as_str()
            );
        }
        assert_eq!(
            encoded["format_identities"].as_object().unwrap().len(),
            crate::format::format_specs().len(),
            "every compiled format is named, or a caller cannot tell which moved"
        );
    }
}
