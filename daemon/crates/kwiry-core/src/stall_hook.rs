//! Test-only reader stall point.
//!
//! Compiled only with the non-default `internal-stall-hook-test` feature in a
//! build with debug assertions; release builds contain none of this code even
//! when the feature is selected. At run time it also needs
//! `KWIRY_TEST_STALL_READER_DIR` naming an existing directory. It replaces a
//! debugger breakpoint at the same place: after a request has opened its
//! reader, before it searches.
//!
//! Protocol, all files inside that directory:
//! 1. The test creates `arm`.
//! 2. The next search claims `arm` (exactly one request), creates `pinned`
//!    and waits on that thread only.
//! 3. The test publishes a new generation, then creates `release`.
//! 4. The paused request consumes `release` and searches its old reader.
//!
//! A pause that is never released gives up after [`MAX_PAUSE`] so a broken
//! test cannot wedge the process indefinitely.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub const STALL_DIR_ENV: &str = "KWIRY_TEST_STALL_READER_DIR";
const MAX_PAUSE: Duration = Duration::from_secs(120);

fn stall_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os(STALL_DIR_ENV)
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
    })
    .as_ref()
}

pub(crate) fn pause_opened_reader() {
    let Some(dir) = stall_dir() else {
        return;
    };
    // Rename is the claim: only one concurrent request can win `arm`.
    let claimed = dir.join(format!("claimed-{}", std::process::id()));
    if std::fs::rename(dir.join("arm"), &claimed).is_err() {
        return;
    }
    let _ = std::fs::remove_file(&claimed);
    let release = dir.join("release");
    if std::fs::write(dir.join("pinned"), b"").is_err() {
        return;
    }
    let deadline = Instant::now() + MAX_PAUSE;
    while Instant::now() < deadline {
        if std::fs::remove_file(&release).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
