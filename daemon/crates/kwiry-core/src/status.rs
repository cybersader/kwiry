use serde::{Deserialize, Serialize};

use crate::model::{CHUNKING_VERSION, IndexFreshnessBasis, SourceFormatCounts};

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
    pub documents: usize,
    pub chunks: usize,
    pub source_format_counts: SourceFormatCounts,
    pub last_sync: Option<String>,
    pub dirty: bool,
    pub rebuilding: bool,
    pub model: Option<ModelStatus>,
    pub vaults: Vec<VaultStatus>,
}

impl DaemonStatus {
    pub fn starting(version: impl Into<String>) -> Self {
        Self {
            state: DaemonState::Starting,
            version: version.into(),
            generation: None,
            chunking_version: CHUNKING_VERSION,
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
            encoded["source_format_counts"]["markdown"]["indexed-complete"],
            0
        );
    }
}
