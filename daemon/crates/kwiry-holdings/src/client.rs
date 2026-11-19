//! Fetching one complete, verified list (v2 Part I §3.1, §3.7, §4.5, §7,
//! H.2, H.3, H.6, H.8, H.12, H.13, H.17).
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
use crate::keys::{HolderKey, KeyError, PinnedKeys, parse_keyset, verify_keyset};
use crate::profile::{PROBLEM_CONTENT_TYPE, ROUTE_KEYSET, ROUTE_LIST};
use crate::verify::{
    PageError, PageExpectations, SignedPage, VerifiedList, VerifiedPage, VerifyContext, assemble,
    check_audience, check_page, verify_signature,
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
    /// `400` or `413` (v2 H.40): back off and rebuild; never resend.
    RequestInvalid {
        status: u16,
    },
    /// An answer the list route never gives (`409`).
    Rejected {
        status: u16,
    },
    /// A verified key-set statement would leave no pinned key (H.37.6).
    KeysetInvalid,
    /// Entropy or pin persistence failed locally.
    Local(&'static str),
    /// Fatal: holder state could not be made durable and consistent; the
    /// holder fail-stopped (`holder_state_unwritable`).
    HolderStateUnwritable,
}

/// Clock measurements from one page. The client only measures; a holder
/// profile decides what they mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockEvidence {
    /// A page whose signature and `iss` verified (v2 H.36), whatever its
    /// later checks say: `lag_ms = iat − now_recv`.
    Signed { lag_ms: i64 },
    /// A page that fully verified and answers this process's own request
    /// with the matching nonce, with its monotonic round trip.
    Answered { lag_ms: i64, rtt_ms: u64 },
}

pub struct FetchEnv<'a> {
    pub transport: &'a dyn Transport,
    pub clock: &'a dyn Clock,
    pub key: &'a HolderKey,
    pub ctx: VerifyContext<'a>,
    pub pins: &'a PinnedKeys,
    pub hooks: &'a mut dyn FetchHooks,
}

/// What a fetch hands back while it runs. One object takes both, so a
/// holder can apply each in observation order with its single writer.
pub trait FetchHooks {
    /// Persists an accepted key-set statement's pins durably; false refuses.
    fn accept_pins(&mut self, pins: &PinnedKeys) -> bool;
    /// Each signed page's evidence, at once, in observation order (raises).
    fn on_clock(&mut self, evidence: ClockEvidence);
    /// The whole fetch's evidence, in observation order, once a complete
    /// list (or a complete probe) verified (H.23, H.29). Only this may clear.
    fn on_verified(&mut self, evidence: &[ClockEvidence]);
}

pub struct FetchParams<'a> {
    pub high_water: Option<&'a Rev>,
    pub last_list_ref: Option<&'a ListRef>,
    pub page_size: u16,
}

struct Session {
    nonce: String,
    cursor: Option<String>,
    /// The key-set route was already fetched in this list.
    route_fetched: bool,
    pins: PinnedKeys,
    /// A key-set statement changed the pins since buffered pages were last
    /// verified (v2 H.37.3).
    pins_changed: bool,
    /// Clock evidence of the current list attempt, in observation order.
    evidence: Vec<ClockEvidence>,
}

enum Answer {
    /// A page that passed every page-level check, its compact form (kept to
    /// re-verify it if the pins change) and its nonce-bound clock
    /// measurement, delivered only once the whole list verifies (H.29).
    Page {
        page: Box<VerifiedPage>,
        compact: String,
        answered: ClockEvidence,
    },
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

/// Applies a key-set statement (v2 H.6, H.37): skipped if its `keyset_seq`
/// is not above the stored one, otherwise verified under the current pins
/// and persisted before any key it adds is used.
fn apply_statement(
    env: &mut FetchEnv,
    session: &mut Session,
    statement: &str,
) -> Result<(), KeyError> {
    // v2 H.35: the whole grammar is checked first, so a malformed statement
    // is refused whatever its sequence; only a well-formed stale one is
    // skipped.
    let parsed = parse_keyset(statement, env.ctx.profile, env.ctx.issuer)?;
    if session
        .pins
        .keyset_seq
        .is_some_and(|current| parsed.keyset_seq <= current)
    {
        return Ok(());
    }
    let next = verify_keyset(
        statement,
        &session.pins,
        env.ctx.profile,
        env.ctx.issuer,
        env.clock.wall_ms(),
    )?;
    if !env.hooks.accept_pins(&next) {
        return Err(KeyError::Keyset("pin persistence"));
    }
    session.pins = next;
    session.pins_changed = true;
    Ok(())
}

fn fetch_route_statement(env: &FetchEnv) -> Option<String> {
    let url = format!("{}{ROUTE_KEYSET}", env.ctx.issuer);
    let response = env.transport.get(&url, LIST_TIMEOUTS).ok()?;
    (response.status == 200
        && response.content_type.as_deref() == Some(env.ctx.profile.keyset_content_type().as_str()))
    .then(|| String::from_utf8(response.body).ok())
    .flatten()
}

fn keyset_failure(error: &KeyError, page_kid_pinned: bool) -> FetchFailure {
    match error {
        KeyError::KeysetInvalid => FetchFailure::KeysetInvalid,
        KeyError::Keyset("pin persistence") => FetchFailure::Local("pin persistence"),
        _ if page_kid_pinned => FetchFailure::Invalid(PageError::Keyset),
        _ => FetchFailure::UnknownKid,
    }
}

/// Verifies the signature stage, first applying any attached key-set
/// statement (always processed, even when the page's own key is pinned), and
/// on an unknown `kid` without one, the key-set route once per list.
fn verify_with_keysets(
    env: &mut FetchEnv,
    session: &mut Session,
    compact: &str,
    now_recv: i64,
) -> Result<SignedPage, FetchFailure> {
    let unverified = crate::jws::parse(compact).ok();
    let page_kid = unverified
        .as_ref()
        .map(|p| p.kid.clone())
        .unwrap_or_default();
    let prefix = &env.ctx.profile.authority_key_id_prefix;
    let attached = unverified.and_then(|page| {
        page.claims
            .get("keyset")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    if let Some(statement) = &attached {
        let pinned = session.pins.live(&page_kid, prefix, now_recv).is_some();
        apply_statement(env, session, statement).map_err(|e| keyset_failure(&e, pinned))?;
    }
    match verify_signature(compact, &session.pins, now_recv, &env.ctx) {
        Err(PageError::UnknownKid) if attached.is_none() && !session.route_fetched => {
            session.route_fetched = true;
            let statement = fetch_route_statement(env).ok_or(FetchFailure::UnknownKid)?;
            apply_statement(env, session, &statement).map_err(|e| keyset_failure(&e, false))?;
            verify_signature(compact, &session.pins, now_recv, &env.ctx).map_err(page_error)
        }
        other => other.map_err(page_error),
    }
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
    let signed = verify_with_keysets(env, session, &compact, now_recv)?;
    let lag_ms = signed.iat_ms - now_recv;
    env.hooks.on_clock(ClockEvidence::Signed { lag_ms });
    session.evidence.push(ClockEvidence::Signed { lag_ms });
    check_audience(&signed, &env.ctx).map_err(page_error)?;
    let expectations = PageExpectations {
        nonce: &session.nonce,
        high_water,
    };
    let mut page = check_page(signed, now_recv, &env.ctx, &expectations).map_err(page_error)?;
    page.received_mono_ms = received_mono;
    let rtt_ms = received_mono.saturating_sub(sent_mono);
    Ok(Answer::Page {
        page: Box::new(page),
        compact,
        answered: ClockEvidence::Answered { lag_ms, rtt_ms },
    })
}

/// After a key-set statement changed the pins mid-fetch, every buffered
/// page must still verify under the current pins (a retired signer fails).
fn reverify(
    env: &FetchEnv,
    session: &mut Session,
    compacts: &[String],
) -> Result<(), FetchFailure> {
    if !std::mem::take(&mut session.pins_changed) {
        return Ok(());
    }
    let now = env.clock.wall_ms();
    for compact in compacts {
        verify_signature(compact, &session.pins, now, &env.ctx).map_err(page_error)?;
    }
    Ok(())
}

fn fresh_session(env: &FetchEnv) -> Result<Session, FetchFailure> {
    Ok(Session {
        nonce: token(32)?,
        cursor: None,
        route_fetched: false,
        pins: env.pins.clone(),
        pins_changed: false,
        evidence: Vec::new(),
    })
}

/// Fetches and verifies one complete list. Nothing is applied.
pub fn fetch_list(env: &mut FetchEnv, params: &FetchParams) -> Result<VerifiedList, FetchFailure> {
    let started = env.clock.mono_ms();
    let mut budget_ms = INITIAL_LIST_BUDGET_MS;
    let mut session = fresh_session(env)?;
    let mut pages: Vec<VerifiedPage> = Vec::new();
    let mut compacts: Vec<String> = Vec::new();
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
            Answer::Page {
                page,
                compact,
                answered,
            } => {
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
                compacts.push(compact);
                session.evidence.push(answered);
                reverify(env, &mut session, &compacts)?;
                match next {
                    None => {
                        let list = assemble(pages).map_err(FetchFailure::Invalid)?;
                        // Only now may a measurement clear the clock hold:
                        // the whole list verified (H.29).
                        env.hooks.on_verified(&session.evidence);
                        return Ok(list);
                    }
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
                    compacts.clear();
                    session.evidence.clear();
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
                (400 | 413, _) => return Err(FetchFailure::RequestInvalid { status }),
                (409, _) => return Err(FetchFailure::Rejected { status }),
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
        Answer::Page { page, answered, .. } => {
            if page.index != 0 {
                return Err(FetchFailure::Invalid(PageError::Consistency(
                    "first page index",
                )));
            }
            // A probe clears only as a complete list that verifies under the
            // normal rules, `items_total` included (H.29). A probe that sees
            // only the first of several pages measures nothing it may use.
            if page.next_cursor.is_none() {
                assemble(vec![*page]).map_err(FetchFailure::Invalid)?;
                session.evidence.push(answered);
                env.hooks.on_verified(&session.evidence);
            }
            Ok(())
        }
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
            (400 | 413, _) => FetchFailure::RequestInvalid { status },
            (409, _) => FetchFailure::Rejected { status },
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
