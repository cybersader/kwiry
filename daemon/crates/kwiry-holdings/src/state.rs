//! The holder directory and its single, fsynced writer (v2 Part I §3.4
//! rule 6, §3.7; Annex K.2 §13 "Holder state" and "Keys").
//!
//! The directory lives next to the operator configuration and never inside
//! a disposable data or index directory. It holds:
//!
//! ```text
//! holder.key         Ed25519 private JWK (0600)
//! holder.json        enrollment: holder id, kind, issuer, pinned authority keys
//! holder-state.json  high-water, last list ref, grow-only tombstones, the
//!                    reset flag, and the engine profile's section (S.4, S.6)
//! holder.lock        flock held by the one writer for its whole lifetime
//! ```
//!
//! Every file is replaced whole: temporary file, write, fsync, rename, fsync
//! the directory. A failed commit leaves the previous durable state in force
//! and the in-memory copy unchanged. The state only ever refuses: no field
//! can authorise serving, building or deleting.

use crate::clock::Clock;
use crate::engine::{EngineState, PinPair};
use crate::keys::PinnedKeys;
use crate::wire::{ListRef, Rev};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const KEY_FILE: &str = "holder.key";
pub const ENROLLMENT_FILE: &str = "holder.json";
pub const STATE_FILE: &str = "holder-state.json";
pub const LOCK_FILE: &str = "holder.lock";

pub const STATE_SCHEMA: &str = "holdings-holder-state/1";
pub const ENROLLMENT_SCHEMA: &str = "holdings-holder/1";
/// Holder-state backups older than this are refused on recovery.
pub const DEFAULT_RETENTION_WINDOW_SECONDS: u64 = 30 * 24 * 3600;

/// Durable file replacement. Production uses [`FsIo`]; tests inject faults.
pub trait DurableIo: Send {
    fn write_atomic(&self, dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()>;
}

pub struct FsIo;

#[cfg(unix)]
fn private_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options
}

#[cfg(not(unix))]
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

impl DurableIo for FsIo {
    fn write_atomic(&self, dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        let suffix = crate::b64::random_token(9).map_err(|e| io::Error::other(e.to_string()))?;
        let temporary = dir.join(format!(".{name}.{suffix}.tmp"));
        let result = (|| {
            let mut file = private_options().open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, dir.join(name))?;
            sync_dir(dir)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DirError {
    #[error("holder directory unusable: {0}")]
    Io(#[from] io::Error),
    #[error("holder directory overlaps a data or index directory")]
    Overlap,
    #[error("another process holds the holder lock")]
    LockHeld,
}

/// An opened holder directory with its exclusive lock held.
pub struct HolderDir {
    root: PathBuf,
    _lock: File,
}

impl HolderDir {
    /// Opens (creating with mode 0700 if needed) the holder directory and takes
    /// its exclusive lock. `forbidden` are the data and index directories: the
    /// holder directory may neither sit inside nor contain any of them.
    pub fn open(path: &Path, forbidden: &[&Path]) -> Result<Self, DirError> {
        if !path.exists() {
            create_private_dir(path)?;
        }
        let root = fs::canonicalize(path)?;
        for other in forbidden {
            let other = match fs::canonicalize(other) {
                Ok(canonical) => canonical,
                Err(error) if error.kind() == io::ErrorKind::NotFound => other.to_path_buf(),
                Err(error) => return Err(error.into()),
            };
            if root.starts_with(&other) || other.starts_with(&root) {
                return Err(DirError::Overlap);
            }
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join(LOCK_FILE))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| DirError::LockHeld)?;
        Ok(Self { root, _lock: lock })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Reads a holder file; `Ok(None)` when it does not exist.
    pub fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.file(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tombstone {
    pub resource_id: String,
    pub resource_kind: String,
    pub holding_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HolderState {
    pub schema: String,
    pub holder_id: String,
    pub committed_at_ms: i64,
    pub high_water: Option<Rev>,
    pub last_list_ref: Option<ListRef>,
    /// Set by trust on first use; cleared only by an accepted status report
    /// carrying `holder_state: "reset"` (a later slice).
    pub reset_pending: bool,
    pub tombstones: BTreeSet<Tombstone>,
    /// Engine-profile fields; default for every other holder.
    pub engine: EngineState,
}

impl HolderState {
    pub fn fresh(holder_id: &str, reset_pending: bool) -> Self {
        Self {
            schema: STATE_SCHEMA.to_owned(),
            holder_id: holder_id.to_owned(),
            committed_at_ms: 0,
            high_water: None,
            last_list_ref: None,
            reset_pending,
            tombstones: BTreeSet::new(),
            engine: EngineState::default(),
        }
    }

    pub fn is_tombstoned(&self, resource_id: &str) -> bool {
        self.tombstones.iter().any(|t| t.resource_id == resource_id)
    }

    /// Parses a state file, refusing unknown fields and other schemas.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value = crate::strict_json::parse(bytes).map_err(|e| e.to_string())?;
        let state: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if state.schema != STATE_SCHEMA {
            return Err(format!("unsupported schema {:?}", state.schema));
        }
        Ok(state)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enrollment {
    pub schema: String,
    pub holder_id: String,
    pub holder_kind: String,
    /// Authority issuer and request audience, `https://…/v1`.
    pub issuer: String,
    /// RFC 7638 thumbprint of the enrolled holder key.
    pub holder_jkt: String,
    pub pinned: PinnedKeys,
    pub retention_window_seconds: u64,
    pub initialized_at_ms: i64,
}

impl Enrollment {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value = crate::strict_json::parse(bytes).map_err(|e| e.to_string())?;
        let enrollment: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if enrollment.schema != ENROLLMENT_SCHEMA {
            return Err(format!("unsupported schema {:?}", enrollment.schema));
        }
        if !crate::grammar::holder_id(&enrollment.holder_id) {
            return Err("holder_id".into());
        }
        Ok(enrollment)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("holder-state write failed: {0}")]
    Io(#[from] io::Error),
    #[error("mutation refused: {0}")]
    Refused(&'static str),
}

/// What one admitted list adds to durable state (row A1).
pub struct ApplyMutation {
    pub rev: Rev,
    pub tombstones: Vec<Tombstone>,
    pub pin_pairs: Vec<PinPair>,
}

/// Result of a committed A1 mutation.
#[derive(Debug, PartialEq, Eq)]
pub struct ApplyCommitted {
    pub ceiling_raised: Option<(u64, u64)>,
}

/// The only code that writes holder files. Each typed mutation checks its
/// guard against the durable copy, writes the whole merged file, and only
/// then updates the in-memory copy.
pub struct StateWriter {
    dir: HolderDir,
    io: Box<dyn DurableIo>,
    clock: Arc<dyn Clock>,
    state: HolderState,
    enrollment: Enrollment,
}

impl StateWriter {
    pub(crate) fn new(
        dir: HolderDir,
        io: Box<dyn DurableIo>,
        clock: Arc<dyn Clock>,
        state: HolderState,
        enrollment: Enrollment,
    ) -> Self {
        Self {
            dir,
            io,
            clock,
            state,
            enrollment,
        }
    }

    pub fn state(&self) -> &HolderState {
        &self.state
    }

    pub fn enrollment(&self) -> &Enrollment {
        &self.enrollment
    }

    pub fn dir(&self) -> &HolderDir {
        &self.dir
    }

    /// Writes the in-memory state as-is (fresh state at enrollment or trust
    /// on first use).
    pub(crate) fn persist_initial(&mut self) -> Result<(), WriteError> {
        self.commit(self.state.clone())
    }

    pub(crate) fn commit(&mut self, mut next: HolderState) -> Result<(), WriteError> {
        next.committed_at_ms = self.clock.wall_ms();
        let bytes = serde_json::to_vec_pretty(&next).map_err(io::Error::other)?;
        self.io.write_atomic(self.dir.path(), STATE_FILE, &bytes)?;
        self.state = next;
        Ok(())
    }

    /// Row A1: advance the high-water and union tombstones and (engine
    /// profile) pin-history pairs in one atomic commit. If the pairs would exceed the ceiling, the
    /// ceiling is raised in the same commit; nothing is ever evicted.
    pub fn apply_list(&mut self, mutation: ApplyMutation) -> Result<ApplyCommitted, WriteError> {
        if let Some(high_water) = &self.state.high_water {
            if mutation.rev.epoch != high_water.epoch {
                return Err(WriteError::Refused("epoch differs from the high-water"));
            }
            if mutation.rev.seq < high_water.seq {
                return Err(WriteError::Refused("revision below the high-water"));
            }
        }
        let mut next = self.state.clone();
        next.high_water = Some(mutation.rev);
        next.tombstones.extend(mutation.tombstones);
        let engine = &mut next.engine;
        engine.pin_history.extend(mutation.pin_pairs);
        let size = u64::try_from(engine.pin_history.len()).unwrap_or(u64::MAX);
        let ceiling_raised =
            crate::engine::pin_history::raised_ceiling(size, engine.pin_history_ceiling)
                .map(|raised| (engine.pin_history_ceiling, raised));
        if let Some((_, raised)) = ceiling_raised {
            engine.pin_history_ceiling = raised;
        }
        self.commit(next)?;
        Ok(ApplyCommitted { ceiling_raised })
    }

    /// Row A4: record the list that was fully applied.
    pub fn record_applied(&mut self, reference: ListRef) -> Result<(), WriteError> {
        if self.state.high_water.as_ref() != Some(&reference.rev) {
            return Err(WriteError::Refused("applied list is not at the high-water"));
        }
        let mut next = self.state.clone();
        next.last_list_ref = Some(reference);
        self.commit(next)
    }

    /// Persists an accepted key-set statement's pins before any key it adds
    /// is trusted.
    pub fn pin_keys(&mut self, pinned: PinnedKeys) -> Result<(), WriteError> {
        if let (Some(current), Some(next)) = (self.enrollment.pinned.keyset_seq, pinned.keyset_seq)
            && next <= current
        {
            return Err(WriteError::Refused("keyset_seq does not increase"));
        }
        let mut next = self.enrollment.clone();
        next.pinned = pinned;
        let bytes = serde_json::to_vec_pretty(&next).map_err(io::Error::other)?;
        self.io
            .write_atomic(self.dir.path(), ENROLLMENT_FILE, &bytes)?;
        self.enrollment = next;
        Ok(())
    }
}
