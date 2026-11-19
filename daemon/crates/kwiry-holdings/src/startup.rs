//! Enrollment and startup: which holder state may be trusted (v2 §2.1;
//! Annex K.2 §13 "Holder state lost"; Kwiry IG-2A A6, rows S1–S5).
//!
//! | enrollment | holder state                 | data root                       | outcome |
//! |------------|------------------------------|---------------------------------|---------|
//! | invalid    | any                          | any                             | `not_enrolled` |
//! | valid      | unreadable or invalid        | any                             | `holder_state_corrupt` (file untouched) |
//! | valid      | another holder's             | any                             | `holder_identity_mismatch` |
//! | valid      | absent                       | proven lost, no marker          | trust on first use, `holder_state_reset` |
//! | valid      | absent                       | intact, or marker present       | `holder_state_missing` |
//! | valid      | valid                        | bound to this holder, or lost   | normal start |
//! | valid      | any                          | populated, marker missing/other | `root_holder_mismatch` |
//!
//! Trust on first use needs proof that *both* the holder state and the data
//! are gone. Missing state alone is never a re-enrollment signal.

use crate::clock::Clock;
use crate::events::{EventSink, HolderEvent};
use crate::keys::{HolderKey, PinnedKeys};
use crate::state::{
    DEFAULT_RETENTION_WINDOW_SECONDS, DurableIo, ENROLLMENT_FILE, ENROLLMENT_SCHEMA, Enrollment,
    HolderDir, HolderState, KEY_FILE, STATE_FILE, StateWriter,
};
use serde_json::Value;
use std::io;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentState {
    /// Absent, or only entries the engine recognises as non-content.
    Lost,
    /// Anything that might be content, including unreadable entries.
    Intact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerState {
    Absent,
    Matches,
    Other,
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataRootEvidence {
    pub content: ContentState,
    pub marker: MarkerState,
}

/// The engine's data root as the holder needs to see it at startup. The
/// engine holds its own data-root lock while these run.
pub trait DataRoot {
    /// Classifies the data root. `Lost` must be proven, never assumed.
    fn inspect(&self, holder_id: &str) -> Result<DataRootEvidence, String>;
    /// Writes the immutable holder marker; never overwrites another holder's.
    fn bind(&self, holder_id: &str) -> Result<(), String>;
}

#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("not enrolled: {0}")]
    NotEnrolled(String),
    #[error("holder state corrupt: {0}")]
    HolderStateCorrupt(String),
    #[error("holder state belongs to another holder")]
    HolderIdentityMismatch,
    #[error("holder state missing while data may remain")]
    HolderStateMissing,
    #[error("data root is bound to another holder or unbound and populated")]
    RootHolderMismatch,
    #[error("data root: {0}")]
    DataRoot(String),
    #[error("enrollment refused: {0}")]
    EnrollmentRefused(&'static str),
    #[error("holder files: {0}")]
    Io(#[from] io::Error),
}

impl StartupError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotEnrolled(_) => "not_enrolled",
            Self::HolderStateCorrupt(_) => "holder_state_corrupt",
            Self::HolderIdentityMismatch => "holder_identity_mismatch",
            Self::HolderStateMissing => "holder_state_missing",
            Self::RootHolderMismatch => "root_holder_mismatch",
            Self::DataRoot(_) => "data_root_unreadable",
            Self::EnrollmentRefused(_) => "enrollment_refused",
            Self::Io(_) => "holder_io",
        }
    }
}

impl From<crate::state::WriteError> for StartupError {
    fn from(error: crate::state::WriteError) -> Self {
        match error {
            crate::state::WriteError::Io(error) | crate::state::WriteError::Poisoned(error) => {
                Self::Io(error)
            }
            crate::state::WriteError::Refused(reason) => Self::EnrollmentRefused(reason),
        }
    }
}

/// Creates the holder key. Refuses if one already exists.
pub fn create_key(dir: &HolderDir, io: &dyn DurableIo) -> Result<HolderKey, StartupError> {
    if dir.read(KEY_FILE)?.is_some() {
        return Err(StartupError::EnrollmentRefused(
            "a holder key already exists",
        ));
    }
    let key = HolderKey::generate().map_err(|_| StartupError::EnrollmentRefused("entropy"))?;
    write_key(dir, io, &key)?;
    Ok(key)
}

/// Stores an existing key (enrollment fixtures with fixed test seeds).
pub fn write_key(dir: &HolderDir, io: &dyn DurableIo, key: &HolderKey) -> Result<(), StartupError> {
    let bytes = serde_json::to_vec(&key.private_jwk()).map_err(io::Error::other)?;
    io.write_atomic(dir.path(), KEY_FILE, &bytes)
        .map_err(io::Error::from)?;
    Ok(())
}

pub struct EnrollParams<'a> {
    /// Assigned by the authority at enrollment.
    pub holder_id: &'a str,
    pub holder_kind: &'a str,
    pub issuer: &'a str,
    /// The authority's bare JWKS; trusted at enrollment only.
    pub jwks: &'a Value,
    pub authority_key_id_prefix: &'a str,
}

/// Records enrollment and an initial holder state. Refuses to overwrite an
/// existing enrollment or state: re-enrollment is a separate admin action.
pub fn enroll(
    dir: HolderDir,
    io: Box<dyn DurableIo>,
    clock: Arc<dyn Clock>,
    params: &EnrollParams,
) -> Result<(), StartupError> {
    if !crate::grammar::holder_id(params.holder_id) {
        return Err(StartupError::EnrollmentRefused("holder_id"));
    }
    let key = read_key(&dir)?;
    if dir.read(ENROLLMENT_FILE)?.is_some() || dir.read(STATE_FILE)?.is_some() {
        return Err(StartupError::EnrollmentRefused("already enrolled"));
    }
    let pinned = PinnedKeys::from_enrollment_jwks(params.jwks, params.authority_key_id_prefix)
        .map_err(|_| StartupError::EnrollmentRefused("authority JWKS"))?;
    let enrollment = Enrollment {
        schema: ENROLLMENT_SCHEMA.to_owned(),
        holder_id: params.holder_id.to_owned(),
        holder_kind: params.holder_kind.to_owned(),
        issuer: params.issuer.to_owned(),
        holder_jkt: key.thumbprint(),
        pinned,
        retention_window_seconds: DEFAULT_RETENTION_WINDOW_SECONDS,
        initialized_at_ms: clock.wall_ms(),
    };
    let bytes = serde_json::to_vec_pretty(&enrollment).map_err(io::Error::other)?;
    io.write_atomic(dir.path(), ENROLLMENT_FILE, &bytes)
        .map_err(io::Error::from)?;
    let state = HolderState::fresh(params.holder_id, false);
    StateWriter::new(dir, io, clock, state, enrollment).persist_initial()?;
    Ok(())
}

fn read_key(dir: &HolderDir) -> Result<HolderKey, StartupError> {
    let bytes = dir
        .read(KEY_FILE)?
        .ok_or_else(|| StartupError::NotEnrolled("no holder key".into()))?;
    let jwk = crate::strict_json::parse(&bytes)
        .map_err(|_| StartupError::NotEnrolled("holder key unreadable".into()))?;
    HolderKey::from_private_jwk(&jwk)
        .map_err(|_| StartupError::NotEnrolled("holder key invalid".into()))
}

pub struct Started {
    pub writer: StateWriter,
    pub key: HolderKey,
    /// True when this start took trust on first use.
    pub reset: bool,
}

fn check_root(evidence: DataRootEvidence) -> Result<(), StartupError> {
    match (evidence.marker, evidence.content) {
        (MarkerState::Other | MarkerState::Unreadable, _)
        | (MarkerState::Absent, ContentState::Intact) => Err(StartupError::RootHolderMismatch),
        _ => Ok(()),
    }
}

/// Loads the holder under its lock and applies the loss and reset rules.
pub fn start(
    dir: HolderDir,
    io: Box<dyn DurableIo>,
    clock: Arc<dyn Clock>,
    data_root: &dyn DataRoot,
    events: &dyn EventSink,
) -> Result<Started, StartupError> {
    let key = read_key(&dir)?;
    let enrollment_bytes = dir
        .read(ENROLLMENT_FILE)?
        .ok_or_else(|| StartupError::NotEnrolled("no enrollment".into()))?;
    let enrollment = Enrollment::parse(&enrollment_bytes).map_err(StartupError::NotEnrolled)?;
    if enrollment.holder_jkt != key.thumbprint() {
        return Err(StartupError::NotEnrolled(
            "key does not match enrollment".into(),
        ));
    }
    let holder_id = enrollment.holder_id.clone();
    let inspect = || {
        data_root
            .inspect(&holder_id)
            .map_err(StartupError::DataRoot)
    };

    let evidence = inspect()?;
    check_root(evidence)?;
    let (state, reset) = match dir.read(STATE_FILE)? {
        Some(bytes) => (load_state(&bytes, &holder_id)?, false),
        None => {
            if evidence != lost_unbound() {
                return Err(StartupError::HolderStateMissing);
            }
            // Re-check both conditions immediately before the write.
            if let Some(bytes) = dir.read(STATE_FILE)? {
                (load_state(&bytes, &holder_id)?, false)
            } else if inspect()? != lost_unbound() {
                return Err(StartupError::HolderStateMissing);
            } else {
                (HolderState::fresh(&holder_id, true), true)
            }
        }
    };
    let marker = evidence.marker;
    let mut writer = StateWriter::new(dir, io, clock, state, enrollment);
    if reset {
        writer.persist_initial()?;
        events.emit(HolderEvent::HolderStateReset);
    }
    if marker == MarkerState::Absent {
        data_root.bind(&holder_id).map_err(StartupError::DataRoot)?;
    }
    Ok(Started { writer, key, reset })
}

fn lost_unbound() -> DataRootEvidence {
    DataRootEvidence {
        content: ContentState::Lost,
        marker: MarkerState::Absent,
    }
}

fn load_state(bytes: &[u8], holder_id: &str) -> Result<HolderState, StartupError> {
    let state = HolderState::parse(bytes).map_err(StartupError::HolderStateCorrupt)?;
    if state.holder_id != holder_id {
        return Err(StartupError::HolderIdentityMismatch);
    }
    Ok(state)
}
