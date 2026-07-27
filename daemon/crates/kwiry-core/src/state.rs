use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tempfile::NamedTempFile;

use crate::error::{Error, Result, io_error};

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let source = fs::read(path).map_err(|error| io_error(path, error))?;
    serde_json::from_slice(&source)
        .map_err(|error| Error::State(format!("invalid JSON at {}: {error}", path.display())))
}

pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::State(format!("state path has no parent: {}", path.display())))?;
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let encoded = serde_json::to_vec_pretty(value)
        .map_err(|error| Error::State(format!("could not encode {}: {error}", path.display())))?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|error| io_error(parent, error))?;
    temporary
        .write_all(&encoded)
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .write_all(b"\n")
        .map_err(|error| io_error(temporary.path(), error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error(temporary.path(), error))?;
    persist_atomically(temporary, path)?;
    sync_directory(parent)?;
    Ok(())
}

/// Replaces `path` with `temporary` as a single atomic step.
///
/// On Unix `rename` over an open file always succeeds. On Windows the
/// equivalent replace fails with `ERROR_ACCESS_DENIED` (5) or
/// `ERROR_SHARING_VIOLATION` (32) while any reader still holds a handle on the
/// destination, so a concurrent search reading `current.json` can make a
/// publication fail even though nothing is wrong. Those two codes are
/// transient by nature, so retry briefly before surfacing the error; every
/// other failure is reported immediately. The retry never weakens atomicity —
/// each attempt is still a single replace, and readers observe either the old
/// or the new file, never a partial one.
#[cfg(not(windows))]
fn persist_atomically(temporary: NamedTempFile, path: &Path) -> Result<()> {
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|error| io_error(path, error.error))
}

#[cfg(windows)]
fn persist_atomically(temporary: NamedTempFile, path: &Path) -> Result<()> {
    /// ERROR_ACCESS_DENIED and ERROR_SHARING_VIOLATION: a reader still holds
    /// the destination. Both clear once that handle closes.
    const TRANSIENT: [i32; 2] = [5, 32];
    /// Bounded so a genuinely stuck handle surfaces as an error rather than
    /// hanging a publication indefinitely.
    const ATTEMPTS: u32 = 20;
    const DELAY: std::time::Duration = std::time::Duration::from_millis(25);

    let mut candidate = temporary;
    for attempt in 1..=ATTEMPTS {
        match candidate.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) => {
                let transient = error
                    .error
                    .raw_os_error()
                    .is_some_and(|code| TRANSIENT.contains(&code));
                if !transient || attempt == ATTEMPTS {
                    return Err(io_error(path, error.error));
                }
                candidate = error.file;
                std::thread::sleep(DELAY);
            }
        }
    }
    unreachable!("the loop returns on the final attempt")
}

/// Flushes one already-written file to stable storage.
///
/// The handle is opened for write because Windows `FlushFileBuffers` requires
/// write access and fails a read-only handle with `ERROR_ACCESS_DENIED`. Unix
/// permits `fsync` on a read-only descriptor, which is why opening read-only
/// went unnoticed until the native Windows suite ran. `write(true)` alone
/// neither truncates nor modifies the file; it only widens the access rights.
fn sync_file(path: &Path) -> Result<()> {
    #[cfg(test)]
    record_sync_test_event(path, SyncOperation::File)?;
    File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| io_error(path, error))
}

pub(crate) fn sync_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::State(format!(
            "derived-state tree contains a symbolic link: {}",
            path.display()
        )));
    }
    if metadata.is_file() {
        return sync_file(path);
    }
    if !metadata.is_dir() {
        return Err(Error::State(format!(
            "derived-state tree contains an unsupported entry: {}",
            path.display()
        )));
    }

    let mut entries = fs::read_dir(path)
        .map_err(|error| io_error(path, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| io_error(path, error))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        sync_tree(&entry.path())?;
    }
    sync_directory(path)
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(test)]
    record_sync_test_event(path, SyncOperation::Directory)?;
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_error(path, error))
}

/// Directory fsync has no portable equivalent outside Unix. On Windows a
/// directory handle cannot be flushed, and durability of the rename itself is
/// provided by the filesystem, so this is deliberately a no-op rather than an
/// error. The parameter is still consumed by the test instrumentation.
#[cfg(not(unix))]
pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(test)]
    record_sync_test_event(path, SyncOperation::DirectoryNoop)?;
    let _ = path;
    Ok(())
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncOperation {
    File,
    #[cfg(unix)]
    Directory,
    #[cfg(not(unix))]
    DirectoryNoop,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct SyncTestState {
    events: Vec<(SyncOperation, std::path::PathBuf)>,
    fail_at: Option<(SyncOperation, std::path::PathBuf)>,
}

#[cfg(test)]
thread_local! {
    static SYNC_TEST_STATE: std::cell::RefCell<Option<SyncTestState>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
fn begin_sync_test(fail_at: Option<(SyncOperation, std::path::PathBuf)>) {
    SYNC_TEST_STATE.with(|state| {
        let previous = state.replace(Some(SyncTestState {
            events: Vec::new(),
            fail_at,
        }));
        assert!(
            previous.is_none(),
            "sync test instrumentation was already active"
        );
    });
}

#[cfg(test)]
fn finish_sync_test() -> SyncTestState {
    SYNC_TEST_STATE.with(|state| {
        state
            .borrow_mut()
            .take()
            .expect("sync test instrumentation was not active")
    })
}

#[cfg(test)]
fn record_sync_test_event(path: &Path, operation: SyncOperation) -> Result<()> {
    SYNC_TEST_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return Ok(());
        };
        let path = path.to_path_buf();
        state.events.push((operation, path.clone()));
        if state.fail_at.as_ref() == Some(&(operation, path.clone())) {
            return Err(Error::State(format!(
                "injected {operation:?} sync failure at {}",
                path.display()
            )));
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::{Duration, Instant};

    use serde::Deserialize;
    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, Deserialize, Serialize)]
    struct AtomicProbe {
        generation: u64,
        payload: String,
    }

    #[derive(Debug)]
    struct ObserverOutcome {
        successful_reads: u64,
        reads_while_writer_active: u64,
        error: Option<String>,
    }

    fn probe_payload(generation: u64) -> String {
        format!("{generation:016x}").repeat(512)
    }

    fn validate_atomic_probe(path: &Path) -> std::result::Result<(), String> {
        let value: AtomicProbe = read_json(path).map_err(|error| error.to_string())?;
        let expected = probe_payload(value.generation);
        if value.payload != expected {
            return Err(format!(
                "generation {} had a torn or mismatched payload",
                value.generation
            ));
        }
        Ok(())
    }

    fn spawn_atomic_observer(
        pointer: std::path::PathBuf,
        start: Arc<Barrier>,
        writer_active: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        successful_reads: Arc<AtomicU64>,
    ) -> (
        std::thread::JoinHandle<()>,
        mpsc::Receiver<()>,
        mpsc::Receiver<ObserverOutcome>,
    ) {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (outcome_tx, outcome_rx) = mpsc::channel();
        let observer = std::thread::spawn(move || {
            start.wait();
            let mut ready_tx = Some(ready_tx);
            let mut reads_while_writer_active = 0_u64;
            loop {
                match validate_atomic_probe(&pointer) {
                    Ok(()) => {
                        let reads = successful_reads.fetch_add(1, Ordering::AcqRel) + 1;
                        if writer_active.load(Ordering::Acquire) {
                            reads_while_writer_active += 1;
                        }
                        if let Some(ready_tx) = ready_tx.take() {
                            ready_tx.send(()).unwrap();
                        }
                        if stop.load(Ordering::Acquire) && !writer_active.load(Ordering::Acquire) {
                            outcome_tx
                                .send(ObserverOutcome {
                                    successful_reads: reads,
                                    reads_while_writer_active,
                                    error: None,
                                })
                                .unwrap();
                            return;
                        }
                    }
                    Err(error) => {
                        outcome_tx
                            .send(ObserverOutcome {
                                successful_reads: successful_reads.load(Ordering::Acquire),
                                reads_while_writer_active,
                                error: Some(error),
                            })
                            .unwrap();
                        return;
                    }
                }
                std::thread::yield_now();
            }
        });
        (observer, ready_rx, outcome_rx)
    }

    fn wait_for_observer_read(reads: &AtomicU64, previous: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while reads.load(Ordering::Acquire) <= previous {
            assert!(
                Instant::now() < deadline,
                "observer did not read while the writer was active"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn atomic_pointer_observer_rejects_a_synchronized_two_phase_write() {
        let temporary = tempdir().unwrap();
        let pointer = temporary.path().join("current.json");
        write_json_atomic(
            &pointer,
            &AtomicProbe {
                generation: 0,
                payload: probe_payload(0),
            },
        )
        .unwrap();

        let start = Arc::new(Barrier::new(2));
        let writer_active = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let successful_reads = Arc::new(AtomicU64::new(0));
        let (observer, ready, outcome) = spawn_atomic_observer(
            pointer.clone(),
            Arc::clone(&start),
            Arc::clone(&writer_active),
            Arc::clone(&stop),
            Arc::clone(&successful_reads),
        );
        start.wait();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();

        let value = AtomicProbe {
            generation: 1,
            payload: probe_payload(1),
        };
        let mut encoded = serde_json::to_vec_pretty(&value).unwrap();
        encoded.push(b'\n');
        let split = encoded.len() / 2;
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&pointer)
            .unwrap();
        file.write_all(&encoded[..split]).unwrap();
        file.sync_all().unwrap();

        let observed = outcome
            .recv_timeout(Duration::from_secs(5))
            .expect("observer did not reject the synchronized torn interval");
        assert!(
            observed.error.as_deref().is_some_and(|error| {
                error.contains("invalid JSON") || error.contains("torn or mismatched payload")
            }),
            "unexpected observer outcome: {observed:?}"
        );
        assert!(observed.reads_while_writer_active > 0);

        file.write_all(&encoded[split..]).unwrap();
        file.sync_all().unwrap();
        writer_active.store(false, Ordering::Release);
        stop.store(true, Ordering::Release);
        observer.join().unwrap();
    }

    #[test]
    fn native_atomic_pointer_replacement_never_exposes_partial_state() {
        let temporary = tempdir().unwrap();
        let pointer = temporary.path().join("current.json");
        write_json_atomic(
            &pointer,
            &AtomicProbe {
                generation: 0,
                payload: probe_payload(0),
            },
        )
        .unwrap();

        let start = Arc::new(Barrier::new(2));
        let writer_active = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let successful_reads = Arc::new(AtomicU64::new(0));
        let (observer, ready, outcome) = spawn_atomic_observer(
            pointer.clone(),
            Arc::clone(&start),
            Arc::clone(&writer_active),
            Arc::clone(&stop),
            Arc::clone(&successful_reads),
        );
        start.wait();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();

        for generation in 1..=64 {
            let before = successful_reads.load(Ordering::Acquire);
            write_json_atomic(
                &pointer,
                &AtomicProbe {
                    generation,
                    payload: probe_payload(generation),
                },
            )
            .unwrap();
            wait_for_observer_read(&successful_reads, before);
            assert!(
                outcome.try_recv().is_err(),
                "observer exited during atomic writes"
            );
        }
        writer_active.store(false, Ordering::Release);
        stop.store(true, Ordering::Release);

        let observed = outcome.recv_timeout(Duration::from_secs(5)).unwrap();
        observer.join().unwrap();
        assert!(observed.error.is_none(), "observer failed: {observed:?}");
        assert!(observed.successful_reads >= 65);
        assert!(observed.reads_while_writer_active >= 65);
        let final_value: AtomicProbe = read_json(&pointer).unwrap();
        assert_eq!(final_value.generation, 64);
        assert_eq!(final_value.payload, probe_payload(64));
    }

    #[test]
    fn native_sync_tree_records_nested_syncs_and_propagates_failures() {
        let temporary = tempdir().unwrap();
        let nested = temporary.path().join("nested");
        let root_file = temporary.path().join("root.bin");
        let leaf_file = nested.join("leaf.bin");
        fs::create_dir(&nested).unwrap();
        fs::write(&root_file, b"root").unwrap();
        fs::write(&leaf_file, b"leaf").unwrap();

        begin_sync_test(None);
        sync_tree(temporary.path()).unwrap();
        let observed = finish_sync_test();
        assert!(observed.events.contains(&(SyncOperation::File, root_file)));
        assert!(
            observed
                .events
                .contains(&(SyncOperation::File, leaf_file.clone()))
        );
        #[cfg(unix)]
        {
            assert!(
                observed
                    .events
                    .contains(&(SyncOperation::Directory, nested.clone()))
            );
            assert!(
                observed
                    .events
                    .contains(&(SyncOperation::Directory, temporary.path().to_path_buf()))
            );
        }
        #[cfg(not(unix))]
        {
            assert!(
                observed
                    .events
                    .contains(&(SyncOperation::DirectoryNoop, nested.clone()))
            );
            assert!(
                observed
                    .events
                    .contains(&(SyncOperation::DirectoryNoop, temporary.path().to_path_buf()))
            );
        }

        begin_sync_test(Some((SyncOperation::File, leaf_file.clone())));
        let error = sync_tree(temporary.path()).unwrap_err();
        let failed = finish_sync_test();
        assert!(error.to_string().contains("injected File sync failure"));
        assert!(failed.events.contains(&(SyncOperation::File, leaf_file)));
    }
}
