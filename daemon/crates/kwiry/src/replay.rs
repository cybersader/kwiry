use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::capability::CapabilityFailure;

/// Single-instance replay set. Losing the set requires advancing its durable
/// epoch before serving again. The lock prevents two live sets per data root.
pub(crate) struct ReplayState {
    _lock: File,
    epoch: u64,
    watermark: PathBuf,
    entries: Mutex<Entries>,
}

struct Entries {
    high_water: u64,
    persisted_iat: u64,
    by_jti: HashMap<String, u64>,
}

impl ReplayState {
    pub(crate) fn start(root: &Path, now: u64) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("replay.lock"))?;
        lock.try_lock()?;
        let path = root.join("replay-epoch");
        let watermark = root.join("replay-iat-watermark");
        let epoch = now.max(read_floor(&path)?).max(read_floor(&watermark)?);
        write_floor(&path, epoch)?;
        Ok(Self {
            _lock: lock,
            epoch,
            watermark,
            entries: Mutex::new(Entries {
                high_water: epoch,
                persisted_iat: epoch,
                by_jti: HashMap::new(),
            }),
        })
    }

    pub(crate) fn record(
        &self,
        jti: &str,
        iat: u64,
        exp: u64,
        now: u64,
    ) -> Result<(), CapabilityFailure> {
        if iat <= self.epoch {
            return Err(CapabilityFailure::Unauthorized);
        }
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| CapabilityFailure::ReplayStateUnavailable)?;
        // Once expired, an entry cannot become usable again on a backwards
        // clock step, even if it has already been evicted.
        let now = now.max(entries.high_water);
        entries.high_water = now;
        entries.by_jti.retain(|_, expires| *expires > now);
        if exp <= now {
            return Err(CapabilityFailure::Unauthorized);
        }
        if entries.by_jti.contains_key(jti) {
            return Err(CapabilityFailure::Unauthorized);
        }
        // Persist a separate floor before accepting tokens issued after the
        // fixed epoch; a backwards clock step on restart cannot revive them.
        if iat > entries.persisted_iat {
            write_floor(&self.watermark, iat)
                .map_err(|_| CapabilityFailure::ReplayStateUnavailable)?;
            entries.persisted_iat = iat;
        }
        entries.by_jti.insert(jti.to_owned(), exp);
        Ok(())
    }
}

fn read_floor(path: &Path) -> io::Result<u64> {
    match fs::read_to_string(path) {
        Ok(value) => value.parse().map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn write_floor(path: &Path, value: u64) -> io::Result<()> {
    let root = path.parent().expect("replay files are in the data root");
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    write!(temporary, "{value}")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_is_atomic_and_epoch_survives_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let state = ReplayState::start(root.path(), 100).unwrap();
        let admitted = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| state.record("one", 101, 160, 101)))
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .filter(Result::is_ok)
                .count()
        });
        assert_eq!(admitted, 1);
        assert!(ReplayState::start(root.path(), 102).is_err());
        drop(state);
        let state = ReplayState::start(root.path(), 102).unwrap();
        assert_eq!(
            state.record("one", 101, 160, 102),
            Err(CapabilityFailure::Unauthorized)
        );
        drop(state);
        let state = ReplayState::start(root.path(), 99).unwrap();
        assert_eq!(
            state.record("epoch", 102, 160, 103),
            Err(CapabilityFailure::Unauthorized)
        );
        assert!(state.record("fresh", 103, 160, 103).is_ok());
    }

    #[test]
    fn replay_watermark_failure_does_not_admit_token() {
        let directory = tempfile::tempdir().unwrap();
        let replay = ReplayState::start(directory.path(), 100).unwrap();
        let blocked = directory.path().join("replay-iat-watermark");
        fs::create_dir(&blocked).unwrap();
        assert_eq!(
            replay.record("new", 101, 160, 101),
            Err(CapabilityFailure::ReplayStateUnavailable)
        );
        fs::remove_dir(blocked).unwrap();
        assert!(replay.record("new", 101, 160, 101).is_ok());
    }
}
