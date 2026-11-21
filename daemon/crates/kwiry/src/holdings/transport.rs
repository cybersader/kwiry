//! The production lifecycle [`Transport`]: blocking HTTP(S) to the authority.
//!
//! - TLS through rustls with the bundled web PKI roots; plain `http` reaches
//!   only a loopback authority (configuration refuses anything else).
//! - The holder's [`Timeouts`] are enforced: `connect_ms` bounds the
//!   connection and `response_ms` bounds the whole exchange as the caller sees
//!   it. A lifecycle POST that misses its deadline returns
//!   [`TransportError::Timeout`]; if its answer still arrives (bounded by
//!   [`LATE_DELIVERY_CAP`]) it is handed to the holder through the
//!   [`ResponseDelivery`] it was dispatched with.
//! - Response bodies are bounded ([`MAX_RESPONSE_BYTES`], the largest list
//!   page the wire allows); an oversize answer is treated as unreachable.
//!   A 403 is authoritative on headers alone, so its body is not read.
//! - Redirects are never followed and no proxy is consulted: environment
//!   proxy variables are ignored.
//! - `Retry-After` is read as delta-seconds or an HTTP date.
//!
//! No holder or inbox lock is held here: the holder consumes
//! [`PostStart::entered`] on entry, before any I/O, and a late delivery takes
//! only the inbox's own short lock inside [`ResponseDelivery::deliver`].

use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kwiry_holdings::client::{
    HttpResponse, PostStart, ResponseDelivery, Timeouts, Transport, TransportError,
};

/// The largest list page the wire allows (v2 §7: ≤ 1 MiB). Every other
/// lifecycle answer is far smaller.
pub(crate) const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// How long a timed-out lifecycle POST may still deliver its answer late.
pub(crate) const LATE_DELIVERY_CAP: Duration = Duration::from_secs(120);

/// Response headers kept for the holder (it reads a few by name).
const MAX_KEPT_HEADERS: usize = 64;

pub(crate) struct HttpTransport {
    user_agent: String,
    late_cap: Duration,
}

impl HttpTransport {
    pub(crate) fn new() -> Self {
        Self {
            user_agent: format!("kwiry/{}", env!("CARGO_PKG_VERSION")),
            late_cap: LATE_DELIVERY_CAP,
        }
    }

    #[cfg(test)]
    fn with_late_cap(late_cap: Duration) -> Self {
        Self {
            late_cap,
            ..Self::new()
        }
    }

    fn agent(&self, connect: Duration, global: Duration) -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .save_redirect_history(false)
            .user_agent(self.user_agent.as_str())
            .accept("*/*")
            .timeout_connect(Some(connect.min(global)))
            .timeout_global(Some(global))
            .build()
            .new_agent()
    }

    fn deadlines(timeouts: Timeouts) -> (Duration, Duration) {
        (
            Duration::from_millis(timeouts.connect_ms),
            Duration::from_millis(timeouts.response_ms.max(1)),
        )
    }

    fn post_with(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        connect: Duration,
        global: Duration,
    ) -> Result<HttpResponse, TransportError> {
        let result = self
            .agent(connect, global)
            .post(url)
            .header("content-type", content_type)
            .header("accept-encoding", "identity")
            .send(body);
        read_response(result)
    }
}

fn map_error(error: &ureq::Error) -> TransportError {
    match error {
        ureq::Error::Timeout(_) => TransportError::Timeout,
        _ => TransportError::Unreachable,
    }
}

fn read_response(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<HttpResponse, TransportError> {
    let mut response = result.map_err(|error| map_error(&error))?;
    let status = response.status().as_u16();
    let mut headers = Vec::new();
    for (name, value) in response.headers() {
        if headers.len() >= MAX_KEPT_HEADERS {
            break;
        }
        if let Ok(value) = value.to_str() {
            headers.push((name.as_str().to_owned(), value.to_owned()));
        }
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    let content_type = header("content-type").map(str::to_owned);
    let retry_after_s = header("retry-after").and_then(|value| parse_retry_after(value, now_s()));
    let body = if status == 403 {
        // Nothing in the body can change the auth halt. Don't let a body
        // read (or its deadline) conceal a refusal already received.
        Vec::new()
    } else {
        // The reader's limit refuses a body that reaches it, so allow one
        // byte more and check the exact bound here.
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES + 1)
            .read_to_vec()
            .map_err(|error| map_error(&error))?;
        if u64::try_from(body.len()).map_or(true, |len| len > MAX_RESPONSE_BYTES) {
            return Err(TransportError::Unreachable);
        }
        body
    };
    Ok(HttpResponse {
        status,
        content_type,
        retry_after_s,
        headers,
        body,
    })
}

impl Transport for HttpTransport {
    fn post(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        timeouts: Timeouts,
    ) -> Result<HttpResponse, TransportError> {
        let (connect, global) = Self::deadlines(timeouts);
        self.post_with(url, content_type, body, connect, global)
    }

    /// The POST runs on its own thread, bounded by the late-delivery cap.
    /// The caller waits only for its deadline; an answer that comes later is
    /// delivered through `delivery`, never returned.
    fn post_lifecycle(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        timeouts: Timeouts,
        delivery: ResponseDelivery,
        start: PostStart<'_>,
    ) -> Result<HttpResponse, TransportError> {
        start.entered();
        let (connect, deadline) = Self::deadlines(timeouts);
        let late_cap = self.late_cap.max(deadline);
        let agent_url = url.to_owned();
        let content_type = content_type.to_owned();
        let body = body.to_vec();
        let worker = Self {
            user_agent: self.user_agent.clone(),
            late_cap,
        };
        let handoff = Arc::new(Mutex::new(Handoff::Waiting));
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_handoff = handoff.clone();
        let spawned = std::thread::Builder::new()
            .name("kwiry-holder-post".into())
            .spawn(move || {
                let result = worker.post_with(&agent_url, &content_type, &body, connect, late_cap);
                let mut state = worker_handoff
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match *state {
                    Handoff::Waiting => {
                        *state = Handoff::Answered;
                        let _ = sender.send(result);
                    }
                    Handoff::Abandoned => {
                        *state = Handoff::Answered;
                        drop(state);
                        if let Ok(response) = result {
                            let key = delivery.request_key().to_owned();
                            delivery.deliver(key, response);
                        }
                    }
                    Handoff::Answered => {}
                }
            });
        if spawned.is_err() {
            return Err(TransportError::Unreachable);
        }
        match receiver.recv_timeout(deadline) {
            Ok(result) => result,
            Err(_) => {
                let mut state = handoff.lock().unwrap_or_else(PoisonError::into_inner);
                if *state == Handoff::Answered {
                    // The worker answered between the timeout and this lock.
                    drop(state);
                    return receiver.recv().unwrap_or(Err(TransportError::Timeout));
                }
                *state = Handoff::Abandoned;
                Err(TransportError::Timeout)
            }
        }
    }

    fn get(&self, url: &str, timeouts: Timeouts) -> Result<HttpResponse, TransportError> {
        let (connect, global) = Self::deadlines(timeouts);
        let result = self
            .agent(connect, global)
            .get(url)
            .header("accept-encoding", "identity")
            .call();
        read_response(result)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handoff {
    Waiting,
    Abandoned,
    Answered,
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// `Retry-After` as delta-seconds or an IMF-fixdate (RFC 9110 §10.2.3),
/// relative to `now_s`. A date in the past is zero; anything unparseable is
/// ignored.
pub(crate) fn parse_retry_after(value: &str, now_s: i64) -> Option<u64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(value.parse::<u64>().unwrap_or(u64::MAX));
    }
    let at = parse_imf_fixdate(value)?;
    Some(if at > now_s { at.abs_diff(now_s) } else { 0 })
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` → Unix seconds.
fn parse_imf_fixdate(value: &str) -> Option<i64> {
    fn decimal(value: &str, width: usize) -> Option<i64> {
        if value.len() != width || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        value.parse().ok()
    }

    let (weekday, rest) = value.split_once(", ")?;
    if !matches!(
        weekday,
        "Mon" | "Tue" | "Wed" | "Thu" | "Fri" | "Sat" | "Sun"
    ) {
        return None;
    }
    let mut parts = rest.split(' ');
    let day = decimal(parts.next()?, 2)?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = decimal(parts.next()?, 4)?;
    let mut clock = parts.next()?.split(':');
    let hour = decimal(clock.next()?, 2)?;
    let minute = decimal(clock.next()?, 2)?;
    let second = decimal(clock.next()?, 2)?;
    if parts.next()? != "GMT" || parts.next().is_some() || clock.next().is_some() {
        return None;
    }
    let days_in_month = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant), proleptic Gregorian. Validated fields
    // bound the within-era and within-day offsets.
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era
        .checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)?;
    days.checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A one-connection-per-request test server answering with canned bytes
    /// after an optional delay. Synthetic data only.
    struct TestServer {
        url: String,
        hits: Arc<AtomicUsize>,
    }

    fn read_request(stream: &mut TcpStream) {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        loop {
            let Ok(read) = stream.read(&mut chunk) else {
                return;
            };
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&buffer);
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if buffer.len() >= end + 4 + length {
                    return;
                }
            }
        }
    }

    impl TestServer {
        fn start(delay: Duration, response: Vec<u8>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/api/lifecycle/v1", listener.local_addr().unwrap());
            let hits = Arc::new(AtomicUsize::new(0));
            let counter = hits.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    counter.fetch_add(1, Ordering::SeqCst);
                    let response = response.clone();
                    std::thread::spawn(move || {
                        read_request(&mut stream);
                        std::thread::sleep(delay);
                        let _ = stream.write_all(&response);
                        let _ = stream.flush();
                    });
                }
            });
            Self { url, hits }
        }
    }

    fn http(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        let mut out = out.into_bytes();
        out.extend_from_slice(body);
        out
    }

    const FAST: Timeouts = Timeouts {
        connect_ms: 2_000,
        response_ms: 2_000,
    };

    #[test]
    fn a_post_returns_status_headers_and_body() {
        let server = TestServer::start(
            Duration::ZERO,
            http(
                "200 OK",
                &[
                    ("content-type", "application/example+jwt"),
                    ("example-lifecycle-outcome", "already_complete"),
                ],
                b"synthetic.page.bytes",
            ),
        );
        let response = HttpTransport::new()
            .post(&server.url, "application/example+jwt", b"request", FAST)
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            response.content_type.as_deref(),
            Some("application/example+jwt")
        );
        assert_eq!(
            response.header("Example-Lifecycle-Outcome"),
            Some("already_complete")
        );
        assert_eq!(response.body, b"synthetic.page.bytes");
    }

    #[test]
    fn error_statuses_are_answers_not_transport_failures() {
        let server = TestServer::start(
            Duration::ZERO,
            http(
                "429 Too Many Requests",
                &[
                    ("content-type", "application/problem+json"),
                    ("retry-after", "17"),
                ],
                br#"{"code":"rate_limited"}"#,
            ),
        );
        let response = HttpTransport::new()
            .post(&server.url, "application/x", b"r", FAST)
            .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(response.retry_after_s, Some(17));
    }

    #[test]
    fn a_redirect_is_returned_and_never_followed() {
        let target = TestServer::start(Duration::ZERO, http("200 OK", &[], b"followed"));
        let server = TestServer::start(
            Duration::ZERO,
            http("307 Temporary Redirect", &[("location", &target.url)], b""),
        );
        let response = HttpTransport::new()
            .post(&server.url, "application/x", b"r", FAST)
            .unwrap();
        assert_eq!(response.status, 307);
        let response = HttpTransport::new().get(&server.url, FAST).unwrap();
        assert_eq!(response.status, 307);
        assert_eq!(target.hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_oversize_body_is_refused() {
        let body = vec![b'a'; usize::try_from(MAX_RESPONSE_BYTES).unwrap() + 1];
        let server = TestServer::start(Duration::ZERO, http("200 OK", &[], &body));
        assert_eq!(
            HttpTransport::new().post(&server.url, "application/x", b"r", FAST),
            Err(TransportError::Unreachable)
        );
        let exact = vec![b'a'; usize::try_from(MAX_RESPONSE_BYTES).unwrap()];
        let server = TestServer::start(Duration::ZERO, http("200 OK", &[], &exact));
        assert_eq!(
            HttpTransport::new()
                .post(&server.url, "application/x", b"r", FAST)
                .unwrap()
                .body
                .len(),
            exact.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn oversized_and_truncated_403_bodies_cannot_hide_an_auth_halt() {
        use crate::holdings::{
            adapters::LogSink, open_holder_dir, openclast_profile, start_holder, system_clock,
        };
        use kwiry_core::{Config, HolderProfile, HolderSettings};
        use kwiry_holdings::client::FetchFailure;
        use kwiry_holdings::holder::{AuthHalt, ListOutcome, RouteHealth};
        use kwiry_holdings::startup::{EnrollParams, create_key, enroll};
        use kwiry_holdings::state::FsIo;
        use kwiry_holdings::testkit::Rig;

        let temporary = tempfile::tempdir().unwrap();
        let profile = openclast_profile();
        let rig = Rig::with_profile(
            &temporary.path().join("test-kit"),
            profile.clone(),
            "openclast-lifecycle/v1",
        );
        let oversized = vec![b'a'; usize::try_from(MAX_RESPONSE_BYTES).unwrap() + 1];
        let headers = [("content-type", "application/problem+json")];
        let truncated = b"HTTP/1.1 403 Forbidden\r\ncontent-type: application/problem+json\r\ncontent-length: 99\r\nconnection: close\r\n\r\nshort";
        for (id, response) in [
            http("403 Forbidden", &headers, &oversized),
            truncated.to_vec(),
        ]
        .into_iter()
        .enumerate()
        {
            let server = TestServer::start(Duration::ZERO, response);
            let response = HttpTransport::new()
                .post(&server.url, "application/x", b"r", FAST)
                .unwrap();
            assert_eq!(response.status, 403);
            assert_eq!(
                response.header("content-type"),
                Some("application/problem+json")
            );
            assert!(response.body.is_empty());

            let settings = HolderSettings {
                profile: HolderProfile::OpenClast,
                issuer: server.url,
                state_dir: temporary.path().join(format!("holder-{id}")),
                page_size: None,
                clock_skew_allowance_seconds: None,
                max_stale_serve_seconds: None,
            };
            let config = Config::default();
            let data = temporary.path().join(format!("data-{id}"));
            let clock = system_clock();
            let dir = open_holder_dir(&settings, &config, &data).unwrap();
            let key = create_key(&dir, &FsIo).unwrap();
            let (holder_id, jwks) = rig.authority.enroll(&key);
            enroll(
                dir,
                Box::new(FsIo),
                clock.clone(),
                &EnrollParams {
                    holder_id: &holder_id,
                    holder_kind: "engine",
                    issuer: &settings.issuer,
                    jwks: &jwks,
                    authority_key_id_prefix: &profile.authority_key_id_prefix,
                },
            )
            .unwrap();
            let (mut holder, _lock) = start_holder(
                &settings,
                &config,
                &data,
                clock,
                Arc::new(LogSink::new(Arc::new(|| {}))),
            )
            .unwrap();
            assert_eq!(
                holder.list_once(),
                ListOutcome::Failed(FetchFailure::NotAuthorized)
            );
            assert_eq!(
                holder.route_health(),
                RouteHealth::AuthHalt(AuthHalt::NotAuthorized)
            );
            assert_eq!(holder.list_once(), ListOutcome::AuthHalted);
        }
    }

    #[test]
    fn the_response_deadline_is_enforced() {
        let server = TestServer::start(Duration::from_millis(1_500), http("200 OK", &[], b"late"));
        let started = std::time::Instant::now();
        let result = HttpTransport::new().post(
            &server.url,
            "application/x",
            b"r",
            Timeouts {
                connect_ms: 500,
                response_ms: 300,
            },
        );
        assert_eq!(result, Err(TransportError::Timeout));
        assert!(started.elapsed() < Duration::from_millis(1_400));
    }

    #[test]
    fn an_unreachable_authority_is_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/lifecycle/v1", listener.local_addr().unwrap());
        drop(listener);
        let result = HttpTransport::new().post(&url, "application/x", b"r", FAST);
        // Unix refuses a closed port at once; Windows retries the SYN until
        // the connect deadline. Either way nothing answered.
        if cfg!(unix) {
            assert_eq!(result, Err(TransportError::Unreachable));
        } else {
            assert!(matches!(
                result,
                Err(TransportError::Unreachable | TransportError::Timeout)
            ));
        }
    }

    #[test]
    fn proxy_environment_is_ignored() {
        let transport = HttpTransport::new();
        let agent = transport.agent(Duration::from_secs(1), Duration::from_secs(1));
        assert!(agent.config().proxy().is_none());
        assert_eq!(agent.config().max_redirects(), 0);
        assert!(!agent.config().http_status_as_error());
    }

    #[test]
    fn retry_after_reads_seconds_and_http_dates_without_shortening() {
        assert_eq!(parse_retry_after("120", 0), Some(120));
        assert_eq!(parse_retry_after(" 5 ", 0), Some(5));
        assert_eq!(parse_retry_after("172800", 0), Some(172_800));
        assert_eq!(
            parse_retry_after("99999999999999999999999", 0),
            Some(u64::MAX)
        );
        // 1994-11-06T08:49:37Z = 784111777.
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777 - 30),
            Some(30)
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777 - 172_800),
            Some(172_800)
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777 + 30),
            Some(0)
        );
        for bad in [
            "",
            "-1",
            "1.5",
            "soon",
            "Sun, 06 Nov 1994 08:49:37 UTC",
            "Sun, 06 Nov 11994 08:49:37 GMT",
            "Sun, 06 Nov -994 08:49:37 GMT",
            "Sun, 06 Nov 9223372036854775807 08:49:37 GMT",
            "Sun, 00 Nov 1994 08:49:37 GMT",
            "Sun, 32 Nov 1994 08:49:37 GMT",
            "Sun, 29 Feb 1994 08:49:37 GMT",
            "Sun, 06 Nov 1994 24:49:37 GMT",
            "Sun, 06 Nov 1994 08:60:37 GMT",
            "Sun, 06 Nov 1994 08:49:61 GMT",
            "Tue, 06 Xxx 1994 08:49:37 GMT",
            "Fry, 06 Nov 1994 08:49:37 GMT",
        ] {
            assert_eq!(parse_retry_after(bad, 0), None, "{bad}");
        }
    }

    /// A late answer is delivered through the handle, never returned; this
    /// runs against the testkit holder's inbox via a real dispatch.
    #[test]
    fn a_late_lifecycle_answer_is_delivered_through_its_handle() {
        use kwiry_holdings::testkit::LateDeliveryProbe;
        let server = TestServer::start(
            Duration::from_millis(600),
            http(
                "403 Forbidden",
                &[("content-type", "application/problem+json")],
                br#"{"code":"holder_not_authorized"}"#,
            ),
        );
        let probe = LateDeliveryProbe::new();
        let transport = HttpTransport::with_late_cap(Duration::from_secs(5));
        let result = probe.dispatch(|delivery, start| {
            transport.post_lifecycle(
                &server.url,
                "application/x",
                b"r",
                Timeouts {
                    connect_ms: 500,
                    response_ms: 200,
                },
                delivery,
                start,
            )
        });
        assert_eq!(result, Err(TransportError::Timeout));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while probe.delivered().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(probe.delivered(), vec![403]);
        // The delivered refusal reached the holder's gate at once.
        assert!(probe.auth_halted());
    }

    #[test]
    fn an_answer_inside_the_deadline_is_returned_and_not_delivered() {
        use kwiry_holdings::testkit::LateDeliveryProbe;
        let server = TestServer::start(Duration::ZERO, http("200 OK", &[], b"ack"));
        let probe = LateDeliveryProbe::new();
        let result = probe.dispatch(|delivery, start| {
            HttpTransport::new().post_lifecycle(
                &server.url,
                "application/x",
                b"r",
                FAST,
                delivery,
                start,
            )
        });
        assert_eq!(result.unwrap().body, b"ack");
        std::thread::sleep(Duration::from_millis(100));
        assert!(probe.delivered().is_empty());
    }
}
