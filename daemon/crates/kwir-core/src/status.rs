use serde::{Deserialize, Serialize};

use crate::model::CHUNKING_VERSION;

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
    fn lexical_status_reports_no_semantic_model() {
        let status = DaemonStatus::starting("0.1.0");
        let encoded = serde_json::to_value(status).unwrap();
        assert_eq!(encoded["state"], "starting");
        assert!(encoded["model"].is_null());
        assert_eq!(encoded["chunking_version"], CHUNKING_VERSION);
    }
}
