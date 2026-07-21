use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use kwiry_core::{
    CONNECTION_SCHEMA_VERSION, ConnectionDescriptor, DaemonState, DaemonStatus, HealthResponse,
    load_connection_descriptor, load_token,
};

use super::model::{ReadinessExpectation, SetupConnectionReport, SetupErrorCode};

const MAX_HTTP_RESPONSE_BYTES: u64 = 1024 * 1024;

pub trait ReadinessProbe {
    fn wait_until_ready(
        &mut self,
        expectation: &ReadinessExpectation,
    ) -> Result<SetupConnectionReport, SetupErrorCode>;
}

#[derive(Debug, Clone)]
pub struct HttpReadinessProbe {
    timeout: Duration,
    retry_interval: Duration,
    connect_timeout: Duration,
}

impl Default for HttpReadinessProbe {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            retry_interval: Duration::from_millis(250),
            connect_timeout: Duration::from_secs(1),
        }
    }
}

impl HttpReadinessProbe {
    pub fn new(timeout: Duration, retry_interval: Duration, connect_timeout: Duration) -> Self {
        Self {
            timeout,
            retry_interval,
            connect_timeout,
        }
    }

    fn probe_once(
        &self,
        expectation: &ReadinessExpectation,
    ) -> Result<SetupConnectionReport, SetupErrorCode> {
        let descriptor = load_descriptor(&expectation.connection_path)?;
        let address = validate_descriptor(&descriptor)?;
        let health: HealthResponse = get_json(
            address,
            "/v0/health",
            None,
            self.connect_timeout,
            SetupErrorCode::HealthCheckFailed,
        )?;
        if health.status != "ok" {
            return Err(SetupErrorCode::HealthCheckFailed);
        }

        let token =
            load_token(&descriptor.token_file).map_err(|_| SetupErrorCode::TokenUnavailable)?;
        let status: DaemonStatus = get_json(
            address,
            "/v0/status",
            Some(&token),
            self.connect_timeout,
            SetupErrorCode::StatusCheckFailed,
        )?;
        validate_status(&descriptor, &status, expectation)?;

        Ok(SetupConnectionReport {
            url: descriptor.url,
            daemon_version: status.version,
            state: status.state,
            semantic_ready: status.model.is_some(),
        })
    }
}

impl ReadinessProbe for HttpReadinessProbe {
    fn wait_until_ready(
        &mut self,
        expectation: &ReadinessExpectation,
    ) -> Result<SetupConnectionReport, SetupErrorCode> {
        let deadline = Instant::now() + self.timeout;
        loop {
            match self.probe_once(expectation) {
                Ok(connection) => return Ok(connection),
                Err(error)
                    if matches!(
                        error,
                        SetupErrorCode::NonLoopbackConnection
                            | SetupErrorCode::ConnectionDescriptorInvalid
                    ) =>
                {
                    return Err(error);
                }
                Err(_) if Instant::now() < deadline => thread::sleep(self.retry_interval),
                Err(_) => return Err(SetupErrorCode::ReadinessTimedOut),
            }
        }
    }
}

fn load_descriptor(path: &Path) -> Result<ConnectionDescriptor, SetupErrorCode> {
    if !path.is_file() {
        return Err(SetupErrorCode::ConnectionDescriptorMissing);
    }
    load_connection_descriptor(path).map_err(|_| SetupErrorCode::ConnectionDescriptorInvalid)
}

pub(crate) fn validate_descriptor(
    descriptor: &ConnectionDescriptor,
) -> Result<SocketAddr, SetupErrorCode> {
    if descriptor.schema_version != CONNECTION_SCHEMA_VERSION
        || descriptor.daemon_version.trim().is_empty()
        || !descriptor.token_file.is_absolute()
    {
        return Err(SetupErrorCode::ConnectionDescriptorInvalid);
    }
    let authority = descriptor
        .url
        .strip_prefix("http://")
        .ok_or(SetupErrorCode::ConnectionDescriptorInvalid)?;
    let address: SocketAddr = authority
        .parse()
        .map_err(|_| SetupErrorCode::ConnectionDescriptorInvalid)?;
    if !address.ip().is_loopback() {
        return Err(SetupErrorCode::NonLoopbackConnection);
    }
    Ok(address)
}

pub(crate) fn validate_status(
    descriptor: &ConnectionDescriptor,
    status: &DaemonStatus,
    expectation: &ReadinessExpectation,
) -> Result<(), SetupErrorCode> {
    if status.version != descriptor.daemon_version {
        return Err(SetupErrorCode::DaemonVersionMismatch);
    }
    if status.state != DaemonState::Ready
        || status.generation.is_none()
        || status.dirty
        || status.rebuilding
    {
        return Err(SetupErrorCode::StatusCheckFailed);
    }
    let vault = status
        .vaults
        .iter()
        .find(|vault| vault.vault_id == expectation.vault_id)
        .ok_or(SetupErrorCode::VaultNotReady)?;
    if vault.dirty || vault.last_error.is_some() {
        return Err(SetupErrorCode::VaultNotReady);
    }
    if expectation.semantic_enabled && status.model.is_none() {
        return Err(SetupErrorCode::SemanticNotReady);
    }
    Ok(())
}

fn get_json<T: serde::de::DeserializeOwned>(
    address: SocketAddr,
    path: &str,
    token: Option<&str>,
    timeout: Duration,
    error_code: SetupErrorCode,
) -> Result<T, SetupErrorCode> {
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|_| error_code)?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| error_code)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| error_code)?;

    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nAccept: application/json\r\n{authorization}Connection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|_| error_code)?;
    stream.flush().map_err(|_| error_code)?;

    let mut response = Vec::new();
    stream
        .take(MAX_HTTP_RESPONSE_BYTES)
        .read_to_end(&mut response)
        .map_err(|_| error_code)?;
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
        .ok_or(error_code)?;
    let headers = std::str::from_utf8(&response[..header_end]).map_err(|_| error_code)?;
    let status_line = headers.lines().next().ok_or(error_code)?;
    if !matches!(status_line, "HTTP/1.1 200 OK" | "HTTP/1.0 200 OK") {
        return Err(error_code);
    }
    serde_json::from_slice(&response[header_end..]).map_err(|_| error_code)
}
