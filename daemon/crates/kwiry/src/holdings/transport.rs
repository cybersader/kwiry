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

/// `Retry-After` values above a day are clamped.
const MAX_RETRY_AFTER_S: u64 = 86_400;

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
    // The reader's limit refuses a body that reaches it, so allow one byte
    // more and check the exact bound here.
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES + 1)
        .read_to_vec()
        .map_err(|error| match error {
            ureq::Error::Timeout(_) => TransportError::Timeout,
            // An oversize or truncated body is never handed to verification.
            _ => TransportError::Unreachable,
        })?;
    if u64::try_from(body.len()).map_or(true, |len| len > MAX_RESPONSE_BYTES) {
        return Err(TransportError::Unreachable);
    }
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
        return Some(
            value
                .parse::<u64>()
                .map_or(MAX_RETRY_AFTER_S, |s| s.min(MAX_RETRY_AFTER_S)),
        );
    }
    let at = parse_imf_fixdate(value)?;
    Some(u64::try_from(at.saturating_sub(now_s)).map_or(0, |s| s.min(MAX_RETRY_AFTER_S)))
}

/// `Sun, 06 Nov 1994 08:49:37 GMT` → Unix seconds.
fn parse_imf_fixdate(value: &str) -> Option<i64> {
    let (_weekday, rest) = value.split_once(", ")?;
    let mut parts = rest.split(' ');
    let day: i64 = parts.next()?.parse().ok()?;
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
    let year: i64 = parts.next()?.parse().ok()?;
    let mut clock = parts.next()?.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if parts.next()? != "GMT" || parts.next().is_some() || clock.next().is_some() {
        return None;
    }
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant), proleptic Gregorian.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
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
        assert_eq!(
            HttpTransport::new().post(&url, "application/x", b"r", FAST),
            Err(TransportError::Unreachable)
        );
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
    fn retry_after_reads_seconds_and_http_dates() {
        assert_eq!(parse_retry_after("120", 0), Some(120));
        assert_eq!(parse_retry_after(" 5 ", 0), Some(5));
        assert_eq!(parse_retry_after("999999999", 0), Some(MAX_RETRY_AFTER_S));
        assert_eq!(
            parse_retry_after("99999999999999999999999", 0),
            Some(MAX_RETRY_AFTER_S)
        );
        // 1994-11-06T08:49:37Z = 784111777.
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777 - 30),
            Some(30)
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777 + 30),
            Some(0)
        );
        for bad in ["", "-1", "1.5", "soon", "Sun, 06 Nov 1994 08:49:37 UTC"] {
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
