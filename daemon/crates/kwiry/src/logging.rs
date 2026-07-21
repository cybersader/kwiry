use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const RETAINED_LOG_FILES: usize = 48;

pub(crate) struct LoggingGuard {
    _file_guard: WorkerGuard,
}

pub(crate) fn init(logs_dir: &Path) -> Result<LoggingGuard> {
    create_private_logs_dir(logs_dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::HOURLY)
        .filename_prefix("kwiry")
        .filename_suffix("jsonl")
        .max_log_files(RETAINED_LOG_FILES)
        .build(logs_dir)
        .with_context(|| format!("failed to initialize logs in {}", logs_dir.display()))?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let console = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr);
    let file = tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .with_writer(file_writer);

    tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file)
        .try_init()
        .context("failed to initialize daemon logging")?;

    Ok(LoggingGuard {
        _file_guard: file_guard,
    })
}

fn create_private_logs_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create log directory {}", path.display()))?;
    set_owner_only(path)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure log directory {}", path.display()))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

pub(crate) struct Redacted<'a>(&'a str);

impl<'a> Redacted<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(secret: &'a str) -> Self {
        Self(secret)
    }
}

impl fmt::Display for Redacted<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = self.0;
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::Subscriber;

    use super::*;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn structured_logs_redact_secret_fields() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let make_writer = {
            let output = output.clone();
            move || SharedWriter(output.clone())
        };
        let subscriber = Subscriber::builder()
            .json()
            .flatten_event(true)
            .with_writer(make_writer)
            .finish();
        let sentinel = "sentinel-token-must-not-appear";

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(token = %Redacted::new(sentinel), "redaction check");
        });

        let encoded = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        assert!(encoded.contains("[REDACTED]"));
        assert!(!encoded.contains(sentinel));
        let event: serde_json::Value = serde_json::from_str(encoded.trim()).unwrap();
        assert_eq!(event["token"], "[REDACTED]");
    }
}
