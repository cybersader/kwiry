//! Fetching one complete, verified list.
//!
//! - Each request is a holder-signed request object (`EdDSA`, `kid` = the
//!   enrolled thumbprint, `exp = iat + 300`, fresh `jti`), with one nonce for
//!   every page of a list and the durable `last_list_ref` as a witness.
//! - Every page is verified before the next is requested; any failure
//!   discards every page. Nothing is applied here.
//! - `410 list_expired` restarts from page 1 with a fresh nonce; the fifth
//!   consecutive one gives up (`lifecycle_list_livelock`).
//! - `401 replay` and `401 proof_expired` are retried once with a fresh
//!   proof; a second `proof_expired` raises a clock alert and never halts.
//! - An unknown `kid` is resolved at most once per fetch through a key-set
//!   statement, which is persisted before the new key is used.
//! - Whole-list budget: 120 s, then `min(1800, max(120, 30 + 2·N))` s once
//!   page 0 states `items_total` (N pages of 500).

use crate::b64;
use crate::clock::Clock;
use crate::keys::{HolderKey, PinnedKeys, verify_keyset};
use crate::profile::{PROBLEM_CONTENT_TYPE, ROUTE_KEYSET, ROUTE_LIST};
use crate::verify::{
    PageError, PageExpectations, VerifiedList, VerifiedPage, VerifyContext, assemble, check_page,
    verify_signed,
};
use crate::wire::{ListRef, ListRequest, Rev};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    pub connect_ms: u64,
    pub response_ms: u64,
}

pub const LIST_TIMEOUTS: Timeouts = Timeouts {
    connect_ms: 10_000,
    response_ms: 30_000,
};

pub const INITIAL_LIST_BUDGET_MS: u64 = 120_000;
pub const MAX_LIST_BUDGET_MS: u64 = 1_800_000;
pub const LIVELOCK_410S: u32 = 5;
/// A page whose `iat` is this far ahead of the holder clock proves the
/// holder lags the authority.
pub const CLOCK_BEHIND_MS: i64 = 30_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub retry_after_s: Option<u64>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    Unreachable,
    Timeout,
}

/// HTTP to the authority. Implementations enforce the connect and response
/// timeouts they are given.
pub trait Transport {
    fn post(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        timeouts: Timeouts,
    ) -> Result<HttpResponse, TransportError>;

    fn get(&self, url: &str, timeouts: Timeouts) -> Result<HttpResponse, TransportError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchFailure {
    /// Network failure, `429`, `503 lifecycle_unavailable`, a second `replay`.
    Unreachable {
        retry_after_s: Option<u64>,
    },
    /// The whole-list budget or a transport timeout ran out.
    Deadline,
    Invalid(PageError),
    Regressed,
    UnexpectedEpoch,
    UnknownKid,
    Livelock,
    /// `503 authority_epoch_mismatch`.
    RollbackSuspected,
    ProofExpiredTwice,
    /// `401 invalid_holder_proof`.
    Unauthorized,
    /// `403 holder_not_authorized`.
    NotAuthorized,
    /// `400`, `413`, or a `409` the list route never gives.
    Rejected {
        status: u16,
    },
    /// Entropy or pin persistence failed locally.
    Local(&'static str),
}

/// Clock evidence from one page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockEvidence {
    /// A signed, audience-bound page with `iat − now_recv > 30 s`.
    Behind { lag_lower_bound_ms: i64 },
    /// A fully verified page answering this process's request with the
    /// matching nonce: `upper_bound_ms = (iat − now_recv) + rtt`.
    Measured { upper_bound_ms: i64, rtt_ms: u64 },
}

pub struct FetchEnv<'a> {
    pub transport: &'a dyn Transport,
    pub clock: &'a dyn Clock,
    pub key: &'a HolderKey,
    pub ctx: VerifyContext<'a>,
    pub pins: &'a PinnedKeys,
    /// Persists an accepted key-set statement's pins durably.
    pub accept_pins: &'a mut dyn FnMut(&PinnedKeys) -> bool,
    pub on_clock: &'a mut dyn FnMut(ClockEvidence),
}

pub struct FetchParams<'a> {
    pub high_water: Option<&'a Rev>,
    pub last_list_ref: Option<&'a ListRef>,
    pub page_size: u16,
}

struct Session {
    nonce: String,
    cursor: Option<String>,
    keyset_tried: bool,
    pins: PinnedKeys,
}

enum Answer {
    Page(Box<VerifiedPage>),
    Problem {
        status: u16,
        code: Option<String>,
        retry_after_s: Option<u64>,
    },
    Transport(TransportError),
}

fn token(bytes: usize) -> Result<String, FetchFailure> {
    b64::random_token(bytes).map_err(|_| FetchFailure::Local("entropy"))
}

fn problem_code(response: &HttpResponse) -> Option<String> {
    if response.content_type.as_deref() != Some(PROBLEM_CONTENT_TYPE) {
        return None;
    }
    crate::strict_json::parse(&response.body)
        .ok()?
        .get("code")?
        .as_str()
        .map(str::to_owned)
}

fn whole_list_budget_ms(items_total: u64) -> u64 {
    let pages = items_total.div_ceil(500).max(1);
    pages
        .checked_mul(2_000)
        .and_then(|p| p.checked_add(30_000))
        .map_or(MAX_LIST_BUDGET_MS, |b| {
            b.clamp(INITIAL_LIST_BUDGET_MS, MAX_LIST_BUDGET_MS)
        })
}

fn page_error(error: PageError) -> FetchFailure {
    match error {
        PageError::Regressed => FetchFailure::Regressed,
        PageError::UnexpectedEpoch => FetchFailure::UnexpectedEpoch,
        PageError::UnknownKid => FetchFailure::UnknownKid,
        other => FetchFailure::Invalid(other),
    }
}

/// Resolves an unknown `kid` once: the page's attached statement, else the
/// key-set route. Returns the new pins after they were persisted.
fn resolve_keyset(
    env: &mut FetchEnv,
    session: &mut Session,
    compact: &str,
) -> Result<(), FetchFailure> {
    session.keyset_tried = true;
    let attached = crate::jws::parse(compact).ok().and_then(|page| {
        page.claims
            .get("keyset")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let statement = match attached {
        Some(statement) => statement,
        None => {
            let url = format!("{}{ROUTE_KEYSET}", env.ctx.issuer);
            let response = env
                .transport
                .get(&url, LIST_TIMEOUTS)
                .map_err(|_| FetchFailure::UnknownKid)?;
            if response.status != 200
                || response.content_type.as_deref()
                    != Some(env.ctx.profile.keyset_content_type().as_str())
            {
                return Err(FetchFailure::UnknownKid);
            }
            String::from_utf8(response.body).map_err(|_| FetchFailure::UnknownKid)?
        }
    };
    let next = verify_keyset(
        &statement,
        &session.pins,
        env.ctx.profile,
        env.ctx.issuer,
        env.clock.wall_ms(),
    )
    .map_err(|_| FetchFailure::UnknownKid)?;
    if !(env.accept_pins)(&next) {
        return Err(FetchFailure::Local("pin persistence"));
    }
    session.pins = next;
    Ok(())
}

fn exchange(
    env: &mut FetchEnv,
    params: &FetchParams,
    session: &mut Session,
    high_water: Option<&Rev>,
) -> Result<Answer, FetchFailure> {
    let jti = token(16)?;
    let url = format!("{}{ROUTE_LIST}", env.ctx.issuer);
    let claims = ListRequest {
        holder_id: env.ctx.holder_id,
        audience: env.ctx.issuer,
        htu: &url,
        iat: env.clock.wall_ms().div_euclid(1000),
        jti: &jti,
        nonce: &session.nonce,
        high_water: params.high_water,
        page_size: params.page_size,
        cursor: session.cursor.as_deref(),
        last_list_ref: params.last_list_ref,
    }
    .claims();
    let body = crate::jws::sign_eddsa(
        &env.ctx.profile.typ_list_request,
        &env.key.thumbprint(),
        &claims,
        env.key.signing_key(),
    );
    let sent_mono = env.clock.mono_ms();
    let response = env.transport.post(
        &url,
        &env.ctx.profile.list_request_content_type(),
        body.as_bytes(),
        LIST_TIMEOUTS,
    );
    let received_mono = env.clock.mono_ms();
    let now_recv = env.clock.wall_ms();
    let response = match response {
        Ok(response) => response,
        Err(error) => return Ok(Answer::Transport(error)),
    };
    if response.status != 200 {
        return Ok(Answer::Problem {
            status: response.status,
            code: problem_code(&response),
            retry_after_s: response.retry_after_s,
        });
    }
    if response.content_type.as_deref() != Some(env.ctx.profile.list_page_content_type().as_str()) {
        return Err(FetchFailure::Invalid(PageError::Grammar("content-type")));
    }
    let compact = String::from_utf8(response.body)
        .map_err(|_| FetchFailure::Invalid(PageError::Jws(crate::jws::JwsError::Malformed)))?;
    let signed = match verify_signed(&compact, &session.pins, now_recv, &env.ctx) {
        Err(PageError::UnknownKid) if !session.keyset_tried => {
            resolve_keyset(env, session, &compact)?;
            verify_signed(&compact, &session.pins, now_recv, &env.ctx)
        }
        other => other,
    }
    .map_err(page_error)?;

    let lag = signed.iat_ms - now_recv;
    if lag > CLOCK_BEHIND_MS {
        (env.on_clock)(ClockEvidence::Behind {
            lag_lower_bound_ms: lag,
        });
    }
    let expectations = PageExpectations {
        nonce: &session.nonce,
        high_water,
    };
    let mut page = check_page(signed, now_recv, &env.ctx, &expectations).map_err(page_error)?;
    page.received_mono_ms = received_mono;
    let rtt_ms = received_mono.saturating_sub(sent_mono);
    (env.on_clock)(ClockEvidence::Measured {
        upper_bound_ms: lag.saturating_add(i64::try_from(rtt_ms).unwrap_or(i64::MAX)),
        rtt_ms,
    });
    Ok(Answer::Page(Box::new(page)))
}

fn fresh_session(env: &FetchEnv) -> Result<Session, FetchFailure> {
    Ok(Session {
        nonce: token(32)?,
        cursor: None,
        keyset_tried: false,
        pins: env.pins.clone(),
    })
}

/// Fetches and verifies one complete list. Nothing is applied.
pub fn fetch_list(env: &mut FetchEnv, params: &FetchParams) -> Result<VerifiedList, FetchFailure> {
    let started = env.clock.mono_ms();
    let mut budget_ms = INITIAL_LIST_BUDGET_MS;
    let mut session = fresh_session(env)?;
    let mut pages: Vec<VerifiedPage> = Vec::new();
    let mut expired_410s = 0;
    let mut replay_retried = false;
    let mut expiry_retried = false;
    loop {
        if env.clock.mono_ms().saturating_sub(started) > budget_ms {
            return Err(FetchFailure::Deadline);
        }
        match exchange(env, params, &mut session, params.high_water)? {
            Answer::Transport(TransportError::Timeout) => return Err(FetchFailure::Deadline),
            Answer::Transport(TransportError::Unreachable) => {
                return Err(FetchFailure::Unreachable {
                    retry_after_s: None,
                });
            }
            Answer::Page(page) => {
                replay_retried = false;
                expiry_retried = false;
                match pages.first() {
                    None if page.index != 0 => {
                        return Err(FetchFailure::Invalid(PageError::Consistency(
                            "first page index",
                        )));
                    }
                    None => budget_ms = whole_list_budget_ms(page.head.items_total),
                    Some(first)
                        if usize::try_from(page.index).ok() != Some(pages.len())
                            || page.head != first.head =>
                    {
                        return Err(FetchFailure::Invalid(PageError::Consistency(
                            "pages disagree",
                        )));
                    }
                    Some(_) => {}
                }
                let next = page.next_cursor.clone();
                pages.push(*page);
                match next {
                    None => return assemble(pages).map_err(FetchFailure::Invalid),
                    Some(cursor) => session.cursor = Some(cursor),
                }
            }
            Answer::Problem {
                status,
                code,
                retry_after_s,
            } => match (status, code.as_deref()) {
                (410, _) => {
                    expired_410s += 1;
                    if expired_410s >= LIVELOCK_410S {
                        return Err(FetchFailure::Livelock);
                    }
                    pages.clear();
                    session.cursor = None;
                    session.nonce = token(32)?;
                }
                (401, Some("replay")) if !replay_retried => replay_retried = true,
                (401, Some("replay")) => {
                    return Err(FetchFailure::Unreachable {
                        retry_after_s: None,
                    });
                }
                (401, Some("proof_expired")) if !expiry_retried => expiry_retried = true,
                (401, Some("proof_expired")) => return Err(FetchFailure::ProofExpiredTwice),
                (401, _) => return Err(FetchFailure::Unauthorized),
                (403, _) => return Err(FetchFailure::NotAuthorized),
                (503, Some("authority_epoch_mismatch")) => {
                    return Err(FetchFailure::RollbackSuspected);
                }
                (400 | 409 | 413, _) => return Err(FetchFailure::Rejected { status }),
                _ => return Err(FetchFailure::Unreachable { retry_after_s }),
            },
        }
    }
}

/// One `page_size: 1` request whose only purpose is clock evidence. Its
/// result is never applied.
pub fn probe_clock(env: &mut FetchEnv, params: &FetchParams) -> Result<(), FetchFailure> {
    let mut session = fresh_session(env)?;
    let probe = FetchParams {
        high_water: params.high_water,
        last_list_ref: params.last_list_ref,
        page_size: 1,
    };
    match exchange(env, &probe, &mut session, params.high_water)? {
        Answer::Page(_) => Ok(()),
        Answer::Transport(TransportError::Timeout) => Err(FetchFailure::Deadline),
        Answer::Transport(TransportError::Unreachable) => Err(FetchFailure::Unreachable {
            retry_after_s: None,
        }),
        Answer::Problem {
            status,
            code,
            retry_after_s,
        } => Err(match (status, code.as_deref()) {
            (401, Some("replay" | "proof_expired")) => FetchFailure::Unreachable { retry_after_s },
            (401, _) => FetchFailure::Unauthorized,
            (403, _) => FetchFailure::NotAuthorized,
            (503, Some("authority_epoch_mismatch")) => FetchFailure::RollbackSuspected,
            (400 | 409 | 413, _) => FetchFailure::Rejected { status },
            _ => FetchFailure::Unreachable { retry_after_s },
        }),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn whole_list_budget_scales_with_page_zero_items_total() {
        use super::whole_list_budget_ms as budget;
        assert_eq!(budget(0), 120_000);
        assert_eq!(budget(500 * 45), 120_000);
        assert_eq!(budget(500 * 46), 122_000);
        assert_eq!(budget(100_000), 430_000);
        assert_eq!(budget(442_500), 1_800_000);
        assert_eq!(budget(10_000_000), 1_800_000);
        assert_eq!(budget(u64::MAX), 1_800_000);
    }
}
