use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::manifest::{Manifest, ManifestFile};
use crate::model::{
    DiscoveredFile, IndexFreshnessBasis, PreparedChunk, ResourceKey, RetrievalMetadata,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PartitionScope {
    Whole,
    Resource(ResourceKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnumerationCompleteness {
    Complete,
    Defective,
}

impl EnumerationCompleteness {
    pub(crate) fn may_infer_deletions(self) -> bool {
        self == Self::Complete
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChangeReason {
    New,
    ContentHashChanged,
    RegistrationFingerprintChanged,
    ResourceChanged,
    OutcomeChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetentionReason {
    VaultUnavailable,
    TransientReadError,
    IncompleteEnumeration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceDecision {
    Unchanged,
    Reingest(ChangeReason),
}

/// What a reconciliation pass may look at. `Full` is the authoritative
/// default. `Paths` never restricts enumeration or deletion inference —
/// it is an accelerator carrying `(vault_id, relative_path)` watcher
/// evidence that can only *add* forced byte reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileScope {
    Full,
    Paths(BTreeSet<(String, String)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadReason {
    StrictHash,
    NewSource,
    RegistrationChanged,
    ResourceChanged,
    SizeChanged,
    MtimeChanged,
    RacyTimestamp,
    Audit,
    SemanticBackfill,
    WatchEvent,
}

/// Per-source read-forcing evidence gathered outside the metadata ladder.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SourceSignals {
    pub forced_read: bool,
    pub semantic_backfill: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservationDecision {
    ReadHash(ReadReason),
    ReuseMetadata,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ObservationPolicy {
    pub basis: IndexFreshnessBasis,
    pub now_nanos: u128,
    pub racy_window_nanos: u128,
}

pub(crate) fn plan_observation(
    previous: Option<&ManifestFile>,
    file: &DiscoveredFile,
    registration_fingerprint: &str,
    scope: &PartitionScope,
    previous_scope: Option<&PartitionScope>,
    policy: ObservationPolicy,
    signals: SourceSignals,
) -> ObservationDecision {
    if policy.basis == IndexFreshnessBasis::StrictHash {
        return ObservationDecision::ReadHash(ReadReason::StrictHash);
    }
    let Some(previous) = previous else {
        return ObservationDecision::ReadHash(ReadReason::NewSource);
    };
    if previous_scope.is_some_and(|previous| previous != scope) {
        return ObservationDecision::ReadHash(ReadReason::ResourceChanged);
    }
    if previous.registration_fingerprint != registration_fingerprint {
        return ObservationDecision::ReadHash(ReadReason::RegistrationChanged);
    }
    if previous.byte_length != file.byte_length {
        return ObservationDecision::ReadHash(ReadReason::SizeChanged);
    }
    if previous.mtime_nanos != file.mtime_nanos {
        return ObservationDecision::ReadHash(ReadReason::MtimeChanged);
    }
    if signals.forced_read {
        return ObservationDecision::ReadHash(ReadReason::WatchEvent);
    }
    if signals.semantic_backfill {
        return ObservationDecision::ReadHash(ReadReason::SemanticBackfill);
    }
    if policy.now_nanos.saturating_sub(file.mtime_nanos) <= policy.racy_window_nanos {
        return ObservationDecision::ReadHash(ReadReason::RacyTimestamp);
    }
    ObservationDecision::ReuseMetadata
}

#[derive(Debug)]
pub(crate) struct AuditBudget {
    cursor: usize,
    remaining_sources: usize,
    remaining_bytes: u64,
}

impl AuditBudget {
    pub(crate) fn new(cursor: usize, source_limit: usize, byte_limit: u64) -> Self {
        Self {
            cursor,
            remaining_sources: source_limit,
            remaining_bytes: byte_limit,
        }
    }

    pub(crate) fn select(&mut self, candidates: &[(String, u64)]) -> BTreeSet<String> {
        let mut selected = BTreeSet::new();
        if candidates.is_empty() || self.remaining_sources == 0 {
            return selected;
        }

        let start = self.cursor % candidates.len();
        let mut examined = 0;
        while examined < candidates.len() && self.remaining_sources > 0 {
            let index = (start + examined) % candidates.len();
            let (key, byte_length) = &candidates[index];
            examined += 1;
            if *byte_length > self.remaining_bytes {
                continue;
            }
            selected.insert(key.clone());
            self.remaining_sources -= 1;
            self.remaining_bytes -= *byte_length;
        }
        self.cursor = (start + examined) % candidates.len();
        selected
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }
}

pub(crate) type PlannedChunk = (PreparedChunk, RetrievalMetadata);

#[derive(Debug)]
pub(crate) struct ReconcilePlan {
    pub next_manifest: Manifest,
    retained: BTreeMap<String, RetentionReason>,
    deletes: BTreeMap<PartitionScope, BTreeSet<String>>,
    additions: BTreeMap<PartitionScope, Vec<PlannedChunk>>,
    changed_sources: BTreeSet<String>,
}

impl ReconcilePlan {
    pub(crate) fn new(previous: &Manifest) -> Self {
        Self {
            next_manifest: previous.clone(),
            retained: BTreeMap::new(),
            deletes: BTreeMap::new(),
            additions: BTreeMap::new(),
            changed_sources: BTreeSet::new(),
        }
    }

    pub(crate) fn retain_source(
        &mut self,
        key: &str,
        previous: &ManifestFile,
        reason: RetentionReason,
    ) {
        self.next_manifest
            .files
            .insert(key.to_owned(), ManifestFile::retained(previous));
        self.retained.insert(key.to_owned(), reason);
    }

    pub(crate) fn reconcile_source(
        &mut self,
        key: String,
        previous: Option<&ManifestFile>,
        next: ManifestFile,
        scope: PartitionScope,
        previous_scope: Option<PartitionScope>,
        chunks: Vec<PlannedChunk>,
    ) -> SourceDecision {
        let decision = plan_source(previous, &next, &scope, previous_scope.as_ref());
        if matches!(decision, SourceDecision::Reingest(_)) {
            self.changed_sources.insert(key.clone());
            let delete_scope = previous_scope.unwrap_or_else(|| scope.clone());
            self.deletes
                .entry(delete_scope.clone())
                .or_default()
                .insert(key.clone());
            if delete_scope != scope {
                self.deletes
                    .entry(scope.clone())
                    .or_default()
                    .insert(key.clone());
            }
            self.additions.entry(scope).or_default().extend(chunks);
        }
        self.next_manifest.files.insert(key, next);
        decision
    }

    pub(crate) fn remove_source(&mut self, key: String, scope: PartitionScope) {
        self.remove_manifest_source(&key);
        self.remove_index_source(key, scope);
    }

    pub(crate) fn remove_manifest_source(&mut self, key: &str) {
        self.next_manifest.files.remove(key);
        self.changed_sources.insert(key.to_owned());
    }

    pub(crate) fn remove_index_source(&mut self, key: String, scope: PartitionScope) {
        self.changed_sources.insert(key.clone());
        self.deletes.entry(scope).or_default().insert(key);
    }

    pub(crate) fn deletes(&self, scope: &PartitionScope) -> Option<&BTreeSet<String>> {
        self.deletes.get(scope)
    }

    pub(crate) fn additions(&self, scope: &PartitionScope) -> Option<&[PlannedChunk]> {
        self.additions.get(scope).map(Vec::as_slice)
    }

    pub(crate) fn update_scopes(&self) -> BTreeSet<PartitionScope> {
        self.deletes
            .keys()
            .chain(self.additions.keys())
            .cloned()
            .collect()
    }

    pub(crate) fn changed_source_count(&self) -> usize {
        self.changed_sources.len()
    }

    pub(crate) fn added_chunk_count(&self) -> usize {
        self.additions.values().map(Vec::len).sum()
    }

    pub(crate) fn validate_retention(&self) -> Result<()> {
        if let Some(key) = self
            .retained
            .keys()
            .find(|key| !self.next_manifest.files.contains_key(*key))
        {
            return Err(Error::State(format!(
                "retained source {key} is missing from the candidate manifest"
            )));
        }
        Ok(())
    }
}

pub(crate) fn plan_source(
    previous: Option<&ManifestFile>,
    next: &ManifestFile,
    scope: &PartitionScope,
    previous_scope: Option<&PartitionScope>,
) -> SourceDecision {
    let Some(previous) = previous else {
        return SourceDecision::Reingest(ChangeReason::New);
    };
    if previous_scope.is_some_and(|previous| previous != scope) {
        return SourceDecision::Reingest(ChangeReason::ResourceChanged);
    }
    if previous.registration_fingerprint != next.registration_fingerprint {
        return SourceDecision::Reingest(ChangeReason::RegistrationFingerprintChanged);
    }
    if previous.outcome != next.outcome {
        return SourceDecision::Reingest(ChangeReason::OutcomeChanged);
    }
    if previous.content_hash != next.content_hash {
        return SourceDecision::Reingest(ChangeReason::ContentHashChanged);
    }
    SourceDecision::Unchanged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestFileOutcome;

    fn manifest_file(hash: &str, fingerprint: &str) -> ManifestFile {
        ManifestFile {
            vault_id: "vault".to_owned(),
            path: "note.md".to_owned(),
            format: crate::format::SourceFormat::Markdown,
            coverage: crate::extract::ExtractionCoverage::IndexedComplete,
            content_hash: hash.to_owned(),
            registration_fingerprint: fingerprint.to_owned(),
            resource: None,
            byte_length: 4,
            mtime_nanos: 1,
            chunk_count: 1,
            outcome: ManifestFileOutcome::Indexed,
            warning: None,
        }
    }

    #[test]
    fn new_source_requires_ingest() {
        assert_eq!(
            plan_source(
                None,
                &manifest_file("hash", "fingerprint"),
                &PartitionScope::Whole,
                None
            ),
            SourceDecision::Reingest(ChangeReason::New)
        );
    }

    #[test]
    fn content_hash_change_requires_ingest() {
        assert_eq!(
            plan_source(
                Some(&manifest_file("old", "fingerprint")),
                &manifest_file("new", "fingerprint"),
                &PartitionScope::Whole,
                None,
            ),
            SourceDecision::Reingest(ChangeReason::ContentHashChanged)
        );
    }

    #[test]
    fn registration_change_precedes_content_equality() {
        assert_eq!(
            plan_source(
                Some(&manifest_file("hash", "old")),
                &manifest_file("hash", "new"),
                &PartitionScope::Whole,
                None,
            ),
            SourceDecision::Reingest(ChangeReason::RegistrationFingerprintChanged)
        );
    }

    #[test]
    fn resource_change_precedes_other_equality() {
        let previous = PartitionScope::Resource(ResourceKey::new("tenant", "vault", "old"));
        let next = PartitionScope::Resource(ResourceKey::new("tenant", "vault", "new"));
        assert_eq!(
            plan_source(
                Some(&manifest_file("hash", "fingerprint")),
                &manifest_file("hash", "fingerprint"),
                &next,
                Some(&previous),
            ),
            SourceDecision::Reingest(ChangeReason::ResourceChanged)
        );
    }

    #[test]
    fn equal_source_is_unchanged() {
        let previous = manifest_file("hash", "fingerprint");
        assert_eq!(
            plan_source(Some(&previous), &previous, &PartitionScope::Whole, None,),
            SourceDecision::Unchanged
        );
    }

    #[test]
    fn defective_enumeration_cannot_infer_deletions() {
        assert!(EnumerationCompleteness::Complete.may_infer_deletions());
        assert!(!EnumerationCompleteness::Defective.may_infer_deletions());
    }

    #[test]
    fn plan_moves_a_source_between_resource_scopes_atomically() {
        let key = "vault\0note.md".to_owned();
        let previous_resource = ResourceKey::new("tenant", "vault", "old");
        let next_resource = ResourceKey::new("tenant", "vault", "new");
        let mut previous_file = manifest_file("hash", "fingerprint");
        previous_file.resource = Some(previous_resource.clone());
        let mut next_file = previous_file.clone();
        next_file.resource = Some(next_resource.clone());
        let mut manifest = Manifest::default();
        manifest.files.insert(key.clone(), previous_file.clone());
        let mut plan = ReconcilePlan::new(&manifest);

        assert_eq!(
            plan.reconcile_source(
                key.clone(),
                Some(&previous_file),
                next_file.clone(),
                PartitionScope::Resource(next_resource.clone()),
                Some(PartitionScope::Resource(previous_resource.clone())),
                Vec::new(),
            ),
            SourceDecision::Reingest(ChangeReason::ResourceChanged)
        );

        assert!(
            plan.deletes(&PartitionScope::Resource(previous_resource))
                .unwrap()
                .contains(&key)
        );
        assert!(
            plan.deletes(&PartitionScope::Resource(next_resource))
                .unwrap()
                .contains(&key)
        );
        assert_eq!(plan.next_manifest.files[&key], next_file);
        assert_eq!(plan.changed_source_count(), 1);
    }

    #[test]
    fn plan_retains_exact_previous_evidence_with_a_reason() {
        let key = "vault\0note.md";
        let previous_file = manifest_file("hash", "fingerprint");
        let mut manifest = Manifest::default();
        manifest.files.insert(key.to_owned(), previous_file.clone());
        let mut plan = ReconcilePlan::new(&manifest);

        plan.retain_source(key, &previous_file, RetentionReason::TransientReadError);
        plan.validate_retention().unwrap();

        assert_eq!(plan.next_manifest.files[key], previous_file);
        assert_eq!(plan.retained[key], RetentionReason::TransientReadError);
        assert_eq!(plan.changed_source_count(), 0);
    }

    fn discovered(byte_length: u64, mtime_nanos: u128) -> DiscoveredFile {
        DiscoveredFile {
            absolute_path: std::path::PathBuf::from("/vault/note.md"),
            relative_path: "note.md".to_owned(),
            extension: "md".to_owned(),
            byte_length,
            mtime: (mtime_nanos / 1_000_000_000) as u64,
            mtime_nanos,
        }
    }

    fn policy(basis: IndexFreshnessBasis, now_nanos: u128) -> ObservationPolicy {
        ObservationPolicy {
            basis,
            now_nanos,
            racy_window_nanos: 2_000_000_000,
        }
    }

    #[test]
    fn strict_hash_always_reads_source_bytes() {
        let previous = manifest_file("hash", "fingerprint");
        assert_eq!(
            plan_observation(
                Some(&previous),
                &discovered(previous.byte_length, previous.mtime_nanos),
                "fingerprint",
                &PartitionScope::Whole,
                Some(&PartitionScope::Whole),
                policy(IndexFreshnessBasis::StrictHash, 1_000_000_000_000),
                SourceSignals::default(),
            ),
            ObservationDecision::ReadHash(ReadReason::StrictHash)
        );
    }

    #[test]
    fn metadata_audit_reuses_only_settled_metadata_equal_sources() {
        let previous = manifest_file("hash", "fingerprint");
        let settled_now = previous.mtime_nanos + 10_000_000_000;
        let base = |file: &DiscoveredFile,
                    previous: Option<&ManifestFile>,
                    fingerprint: &str,
                    now: u128,
                    backfill: bool| {
            plan_observation(
                previous,
                file,
                fingerprint,
                &PartitionScope::Whole,
                previous.map(|_| &PartitionScope::Whole),
                policy(IndexFreshnessBasis::MetadataAudit, now),
                SourceSignals {
                    forced_read: false,
                    semantic_backfill: backfill,
                },
            )
        };

        assert_eq!(
            base(
                &discovered(previous.byte_length, previous.mtime_nanos),
                Some(&previous),
                "fingerprint",
                settled_now,
                false,
            ),
            ObservationDecision::ReuseMetadata
        );
        assert_eq!(
            base(
                &discovered(previous.byte_length, previous.mtime_nanos),
                None,
                "fingerprint",
                settled_now,
                false,
            ),
            ObservationDecision::ReadHash(ReadReason::NewSource)
        );
        assert_eq!(
            base(
                &discovered(previous.byte_length + 1, previous.mtime_nanos),
                Some(&previous),
                "fingerprint",
                settled_now,
                false,
            ),
            ObservationDecision::ReadHash(ReadReason::SizeChanged)
        );
        assert_eq!(
            base(
                &discovered(previous.byte_length, previous.mtime_nanos + 1),
                Some(&previous),
                "fingerprint",
                settled_now,
                false,
            ),
            ObservationDecision::ReadHash(ReadReason::MtimeChanged)
        );
        assert_eq!(
            base(
                &discovered(previous.byte_length, previous.mtime_nanos),
                Some(&previous),
                "other-fingerprint",
                settled_now,
                false,
            ),
            ObservationDecision::ReadHash(ReadReason::RegistrationChanged)
        );
        assert_eq!(
            base(
                &discovered(previous.byte_length, previous.mtime_nanos),
                Some(&previous),
                "fingerprint",
                previous.mtime_nanos + 1_000_000_000,
                false,
            ),
            ObservationDecision::ReadHash(ReadReason::RacyTimestamp)
        );
        assert_eq!(
            base(
                &discovered(previous.byte_length, previous.mtime_nanos),
                Some(&previous),
                "fingerprint",
                settled_now,
                true,
            ),
            ObservationDecision::ReadHash(ReadReason::SemanticBackfill)
        );
    }

    #[test]
    fn metadata_audit_reads_on_resource_scope_change() {
        let previous = manifest_file("hash", "fingerprint");
        assert_eq!(
            plan_observation(
                Some(&previous),
                &discovered(previous.byte_length, previous.mtime_nanos),
                "fingerprint",
                &PartitionScope::Resource(ResourceKey::new("tenant", "vault", "new")),
                Some(&PartitionScope::Resource(ResourceKey::new(
                    "tenant", "vault", "old"
                ))),
                policy(
                    IndexFreshnessBasis::MetadataAudit,
                    previous.mtime_nanos + 10_000_000_000,
                ),
                SourceSignals::default(),
            ),
            ObservationDecision::ReadHash(ReadReason::ResourceChanged)
        );
    }

    #[test]
    fn strict_hash_ignores_watch_scope_by_owner_ruling() {
        // Owner ruling (KWIRY-Q-0020, 2026-07-25): scoping is forbidden
        // under strict_hash. Watcher evidence must never change what a
        // strict pass reads, and no mixed-evidence generation may exist.
        let previous = manifest_file("hash", "fingerprint");
        assert_eq!(
            plan_observation(
                Some(&previous),
                &discovered(previous.byte_length, previous.mtime_nanos),
                "fingerprint",
                &PartitionScope::Whole,
                Some(&PartitionScope::Whole),
                policy(IndexFreshnessBasis::StrictHash, 1_000_000_000_000),
                SourceSignals {
                    forced_read: true,
                    semantic_backfill: false,
                },
            ),
            ObservationDecision::ReadHash(ReadReason::StrictHash)
        );
    }

    #[test]
    fn watch_event_forces_a_read_for_a_settled_metadata_equal_source() {
        let previous = manifest_file("hash", "fingerprint");
        assert_eq!(
            plan_observation(
                Some(&previous),
                &discovered(previous.byte_length, previous.mtime_nanos),
                "fingerprint",
                &PartitionScope::Whole,
                Some(&PartitionScope::Whole),
                policy(
                    IndexFreshnessBasis::MetadataAudit,
                    previous.mtime_nanos + 10_000_000_000,
                ),
                SourceSignals {
                    forced_read: true,
                    semantic_backfill: false,
                },
            ),
            ObservationDecision::ReadHash(ReadReason::WatchEvent)
        );
    }

    #[test]
    fn audit_budget_is_bounded_and_rotates_deterministically() {
        let candidates: Vec<(String, u64)> = (0..6)
            .map(|index| (format!("source-{index}"), 10))
            .collect();

        let mut first = AuditBudget::new(0, 2, 100);
        let first_selection = first.select(&candidates);
        assert_eq!(
            first_selection,
            BTreeSet::from(["source-0".to_owned(), "source-1".to_owned()])
        );

        let mut second = AuditBudget::new(first.cursor(), 2, 100);
        let second_selection = second.select(&candidates);
        assert_eq!(
            second_selection,
            BTreeSet::from(["source-2".to_owned(), "source-3".to_owned()])
        );

        let mut byte_bounded = AuditBudget::new(0, 10, 25);
        assert_eq!(byte_bounded.select(&candidates).len(), 2);

        let mut exhausted = AuditBudget::new(0, 0, 100);
        assert!(exhausted.select(&candidates).is_empty());
    }

    #[test]
    fn plan_rejects_a_retained_source_missing_from_the_manifest() {
        let key = "vault\0note.md";
        let previous_file = manifest_file("hash", "fingerprint");
        let manifest = Manifest::default();
        let mut plan = ReconcilePlan::new(&manifest);
        plan.retain_source(key, &previous_file, RetentionReason::VaultUnavailable);
        plan.next_manifest.files.remove(key);

        let error = plan.validate_retention().unwrap_err();
        assert!(error.to_string().contains("retained source"));
    }
}
