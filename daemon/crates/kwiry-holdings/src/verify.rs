//! List-page verification (v2 Part I §3.7, §3.8, §C.3, H.6, H.9, H.16.4) in
//! the checklist order, failing closed:
//! `typ` → `alg` → `kid` → signature → `iss` → `aud` → `sub` → `iat`/`exp` →
//! nonce echo → `rev` against the high-water → page grammar → item grammar
//! and phase/obligation combinations (including the storage-pin member).
//! Page-set consistency is checked when the list is assembled.

use crate::clock::parse_utc_ms;
use crate::grammar;
use crate::jws::{self, JwsError};
use crate::keys::PinnedKeys;
use crate::profile::WireProfile;
use crate::wire::{
    Blocked, Item, Lease, ListRef, Obligation, ObligationState, Phase, ReportState, ResourceKey,
    Rev,
};
use serde_json::{Map, Value};
use std::collections::HashSet;

/// Largest accepted list-page JWS.
pub const PAGE_MAX_BYTES: usize = 1024 * 1024;
/// Allowed difference between a page's `iat` and the holder's wall clock.
pub const SKEW_MS: i64 = 120_000;
pub const CURSOR_MAX_CHARS: usize = 512;
pub const EPOCH_CHAIN_MAX: usize = 64;

pub struct VerifyContext<'a> {
    pub profile: &'a WireProfile,
    /// The authority issuer, which is also the request audience.
    pub issuer: &'a str,
    pub holder_id: &'a str,
    pub holder_kind: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    #[error("page exceeds the size limit")]
    TooLarge,
    #[error("jws: {0}")]
    Jws(#[from] JwsError),
    #[error("typ")]
    Typ,
    #[error("alg")]
    Alg,
    #[error("unknown kid")]
    UnknownKid,
    #[error("signature")]
    Signature,
    #[error("iss")]
    Issuer,
    #[error("aud")]
    Audience,
    #[error("sub")]
    Subject,
    #[error("iat/exp outside the window")]
    Time,
    #[error("nonce echo")]
    Nonce,
    #[error("revision below the high-water")]
    Regressed,
    #[error("revision from another epoch")]
    UnexpectedEpoch,
    #[error("grammar: {0}")]
    Grammar(&'static str),
    #[error("phase/obligation combination")]
    Combination,
    #[error("root_pin: {0}")]
    RootPin(&'static str),
    #[error("consistency: {0}")]
    Consistency(&'static str),
    /// An attached key-set statement on a page signed by a pinned key was
    /// refused; the list is refused with it.
    #[error("attached key-set statement refused")]
    Keyset,
}

/// A page whose signature and audience binding have verified. Its `iat` is
/// usable as clock evidence even if later checks fail.
pub struct SignedPage {
    pub claims: Map<String, Value>,
    pub iat_ms: i64,
}

pub fn verify_signed(
    compact: &str,
    pins: &PinnedKeys,
    now_ms: i64,
    ctx: &VerifyContext,
) -> Result<SignedPage, PageError> {
    let signed = verify_signature(compact, pins, now_ms, ctx)?;
    check_audience(&signed, ctx)?;
    Ok(signed)
}

/// The signature stage of §3.8: size, `typ`, `alg`, a pinned `kid`, the
/// signature and an exact `iss`. A page that passes it is authority-signed,
/// so its `iat` is clock evidence (v2 H.36) whatever later checks say.
pub fn verify_signature(
    compact: &str,
    pins: &PinnedKeys,
    now_ms: i64,
    ctx: &VerifyContext,
) -> Result<SignedPage, PageError> {
    if compact.len() > PAGE_MAX_BYTES {
        return Err(PageError::TooLarge);
    }
    let page = jws::parse(compact)?;
    if page.typ != ctx.profile.typ_list_page {
        return Err(PageError::Typ);
    }
    if page.alg != "ES256" {
        return Err(PageError::Alg);
    }
    let key = pins
        .live(&page.kid, &ctx.profile.authority_key_id_prefix, now_ms)
        .ok_or(PageError::UnknownKid)?;
    if !page.verify_es256(&key) {
        return Err(PageError::Signature);
    }
    let claims = page.claims;
    if text(&claims, "iss") != Some(ctx.issuer) {
        return Err(PageError::Issuer);
    }
    let iat_ms = claims
        .get("iat")
        .and_then(Value::as_i64)
        .and_then(|s| s.checked_mul(1000))
        .ok_or(PageError::Time)?;
    Ok(SignedPage { claims, iat_ms })
}

/// The audience binding of §3.8: `aud` and `sub` name this holder.
pub fn check_audience(signed: &SignedPage, ctx: &VerifyContext) -> Result<(), PageError> {
    if text(&signed.claims, "aud") != Some(ctx.profile.holder_audience(ctx.holder_id).as_str()) {
        return Err(PageError::Audience);
    }
    if text(&signed.claims, "sub") != Some(ctx.holder_id) {
        return Err(PageError::Subject);
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageHead {
    pub list_id: String,
    pub rev: Rev,
    pub nonce: String,
    pub state_digest: String,
    pub list_tag: String,
    pub count: u64,
    pub items_total: u64,
    pub lease: Lease,
}

pub struct VerifiedPage {
    pub head: PageHead,
    pub index: u64,
    pub iat_ms: i64,
    pub items: Vec<Item>,
    pub next_cursor: Option<String>,
    pub instructions: Vec<Value>,
    pub skipped_instructions: usize,
    /// Monotonic receive time, set by the client.
    pub received_mono_ms: u64,
}

pub struct PageExpectations<'a> {
    pub nonce: &'a str,
    pub high_water: Option<&'a Rev>,
}

fn text<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    object.get(name).and_then(Value::as_str)
}

fn closed<'a>(
    value: &'a Value,
    allowed: &[&str],
    required: &[&str],
    what: &'static str,
) -> Result<&'a Map<String, Value>, PageError> {
    let object = value.as_object().ok_or(PageError::Grammar(what))?;
    if object.keys().any(|k| !allowed.contains(&k.as_str()))
        || required.iter().any(|k| !object.contains_key(*k))
    {
        return Err(PageError::Grammar(what));
    }
    Ok(object)
}

fn checked_text<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    valid: impl Fn(&str) -> bool,
    what: &'static str,
) -> Result<&'a str, PageError> {
    text(object, name)
        .filter(|t| valid(t))
        .ok_or(PageError::Grammar(what))
}

fn time(object: &Map<String, Value>, name: &str, what: &'static str) -> Result<i64, PageError> {
    text(object, name)
        .and_then(parse_utc_ms)
        .ok_or(PageError::Grammar(what))
}

fn unsigned(object: &Map<String, Value>, name: &str, what: &'static str) -> Result<u64, PageError> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .ok_or(PageError::Grammar(what))
}

pub fn parse_rev(value: &Value) -> Result<Rev, PageError> {
    let object = closed(value, &["epoch", "seq"], &["epoch", "seq"], "rev")?;
    Ok(Rev {
        epoch: checked_text(object, "epoch", grammar::epoch_id, "rev.epoch")?.to_owned(),
        seq: unsigned(object, "seq", "rev.seq")?,
    })
}

/// Checks a page that passed [`verify_signed`].
pub fn check_page(
    signed: SignedPage,
    now_ms: i64,
    ctx: &VerifyContext,
    expect: &PageExpectations,
) -> Result<VerifiedPage, PageError> {
    let SignedPage { claims, iat_ms } = signed;
    let exp_ms = claims
        .get("exp")
        .and_then(Value::as_i64)
        .and_then(|s| s.checked_mul(1000))
        .ok_or(PageError::Time)?;
    if (iat_ms - now_ms).abs() > SKEW_MS || exp_ms <= now_ms || exp_ms <= iat_ms {
        return Err(PageError::Time);
    }
    if text(&claims, "nonce") != Some(expect.nonce) {
        return Err(PageError::Nonce);
    }
    let rev = parse_rev(claims.get("rev").ok_or(PageError::Grammar("rev"))?)?;
    if let Some(high_water) = expect.high_water {
        if rev.epoch != high_water.epoch {
            return Err(PageError::UnexpectedEpoch);
        }
        if rev.seq < high_water.seq {
            return Err(PageError::Regressed);
        }
    }
    const REQUIRED: [&str; 15] = [
        "iss",
        "aud",
        "sub",
        "iat",
        "exp",
        "jti",
        "list_id",
        "nonce",
        "rev",
        "holder",
        "page",
        "state_digest",
        "list_tag",
        "items",
        "next_cursor",
    ];
    let allowed: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .chain(["instructions", "epoch_chain", "keyset"])
        .collect();
    let page_value = Value::Object(claims);
    let page = closed(&page_value, &allowed, &REQUIRED, "page members")?;

    checked_text(page, "jti", |t| (1..=64).contains(&t.len()), "jti")?;
    let list_id = checked_text(page, "list_id", grammar::list_id, "list_id")?.to_owned();
    let state_digest = checked_text(
        page,
        "state_digest",
        |t| grammar::tagged_mac(t, "hs1."),
        "state_digest",
    )?
    .to_owned();
    let list_tag = checked_text(
        page,
        "list_tag",
        |t| grammar::tagged_mac(t, "lt1."),
        "list_tag",
    )?
    .to_owned();

    let holder = closed(
        &page["holder"],
        &["holder_id", "kind", "lease"],
        &["holder_id", "kind", "lease"],
        "holder",
    )?;
    if text(holder, "holder_id") != Some(ctx.holder_id) {
        return Err(PageError::Grammar("holder.holder_id"));
    }
    if text(holder, "kind") != Some(ctx.holder_kind) {
        return Err(PageError::Grammar("holder.kind"));
    }
    let lease = closed(
        &holder["lease"],
        &["serve_seconds", "relist_seconds"],
        &["serve_seconds", "relist_seconds"],
        "holder.lease",
    )?;
    let lease = Lease {
        serve_seconds: unsigned(lease, "serve_seconds", "lease")?,
        relist_seconds: unsigned(lease, "relist_seconds", "lease")?,
    };
    if lease.serve_seconds == 0 || lease.relist_seconds == 0 {
        return Err(PageError::Grammar("lease"));
    }

    let page_info = closed(
        &page["page"],
        &["index", "count", "items_total"],
        &["index", "count", "items_total"],
        "page",
    )?;
    let index = unsigned(page_info, "index", "page.index")?;
    let count = unsigned(page_info, "count", "page.count")?;
    let items_total = unsigned(page_info, "items_total", "page.items_total")?;
    if count == 0 || index >= count {
        return Err(PageError::Grammar("page.index"));
    }

    let next_cursor = match &page["next_cursor"] {
        Value::Null => None,
        Value::String(c) if !c.is_empty() && c.chars().count() <= CURSOR_MAX_CHARS => {
            Some(c.clone())
        }
        _ => return Err(PageError::Grammar("next_cursor")),
    };
    if next_cursor.is_none() != (index + 1 == count) {
        return Err(PageError::Grammar("next_cursor"));
    }

    if let Some(chain) = page.get("epoch_chain") {
        let links = chain.as_array().ok_or(PageError::Grammar("epoch_chain"))?;
        if links.len() > EPOCH_CHAIN_MAX || links.iter().any(|l| !l.is_string()) {
            return Err(PageError::Grammar("epoch_chain"));
        }
    }
    if let Some(keyset) = page.get("keyset")
        && (index != 0 || !keyset.is_string())
    {
        return Err(PageError::Grammar("keyset"));
    }

    let (instructions, skipped_instructions) = match page.get("instructions") {
        None => (Vec::new(), 0),
        Some(_) if index != 0 => return Err(PageError::Grammar("instructions off page 0")),
        Some(value) => parse_instructions(value, rev.seq)?,
    };

    let raw_items = page["items"]
        .as_array()
        .ok_or(PageError::Grammar("items"))?;
    let mut items = Vec::with_capacity(raw_items.len());
    let mut resource_ids = HashSet::new();
    for raw in raw_items {
        let item = parse_item(raw, ctx, rev.seq)?;
        if let Some(previous) = items.last().map(|i: &Item| i.holding_id.as_str())
            && previous.as_bytes() >= item.holding_id.as_bytes()
        {
            return Err(PageError::Consistency("holding ids not strictly ascending"));
        }
        if !resource_ids.insert(item.resource.id.clone()) {
            return Err(PageError::Consistency("resource id listed twice"));
        }
        items.push(item);
    }

    Ok(VerifiedPage {
        head: PageHead {
            list_id,
            rev,
            nonce: expect.nonce.to_owned(),
            state_digest,
            list_tag,
            count,
            items_total,
            lease,
        },
        index,
        iat_ms,
        items,
        next_cursor,
        instructions,
        skipped_instructions,
        received_mono_ms: 0,
    })
}

fn parse_instructions(value: &Value, rev_seq: u64) -> Result<(Vec<Value>, usize), PageError> {
    const KNOWN: [&str; 5] = [
        "instruction_id",
        "kind",
        "bound_report_digest",
        "approved_count",
        "issued_seq",
    ];
    let entries = value.as_array().ok_or(PageError::Grammar("instructions"))?;
    let mut kept = Vec::new();
    let mut skipped = 0;
    for entry in entries {
        let object = entry.as_object().ok_or(PageError::Grammar("instruction"))?;
        match text(object, "kind") {
            None => return Err(PageError::Grammar("instruction.kind")),
            Some("quarantine_release") => {
                let object = closed(entry, &KNOWN, &KNOWN, "quarantine_release")?;
                checked_text(
                    object,
                    "instruction_id",
                    |t| t.strip_prefix("ins_").is_some_and(grammar::is_b64url),
                    "instruction_id",
                )?;
                checked_text(
                    object,
                    "bound_report_digest",
                    |t| t.strip_prefix("sha256:").is_some_and(|d| !d.is_empty()),
                    "bound_report_digest",
                )?;
                unsigned(object, "approved_count", "approved_count")?;
                if unsigned(object, "issued_seq", "issued_seq")? > rev_seq {
                    return Err(PageError::Grammar("issued_seq above rev"));
                }
                kept.push(entry.clone());
            }
            Some(_) => skipped += 1,
        }
    }
    Ok((kept, skipped))
}

fn parse_item(value: &Value, ctx: &VerifyContext, rev_seq: u64) -> Result<Item, PageError> {
    let item = closed(
        value,
        &[
            "holding_id",
            "resource",
            "phase",
            "root_pin",
            "quarantine_after",
            "obligation",
        ],
        &[
            "holding_id",
            "resource",
            "phase",
            "quarantine_after",
            "obligation",
        ],
        "item members",
    )?;
    let holding_id = checked_text(item, "holding_id", grammar::holding_id, "holding_id")?;
    let resource = closed(
        &item["resource"],
        &["kind", "id"],
        &["kind", "id"],
        "resource",
    )?;
    let kind = checked_text(
        resource,
        "kind",
        |k| ctx.profile.resource_kinds.iter().any(|known| known == k),
        "resource.kind",
    )?;
    if !ctx
        .profile
        .accepted_resource_kinds
        .iter()
        .any(|k| k == kind)
    {
        return Err(PageError::Grammar(
            "resource.kind not accepted by this holder",
        ));
    }
    let id = checked_text(resource, "id", grammar::resource_id, "resource.id")?;
    let phase = match text(item, "phase") {
        Some("active") => Phase::Active,
        Some("terminating") => Phase::Terminating,
        Some("retired") => Phase::Retired,
        _ => return Err(PageError::Grammar("phase")),
    };
    let root_pin =
        crate::engine::root_pin::parse_member(item, ctx.profile.pins(ctx.holder_kind, kind))?;
    let quarantine_after_ms = time(item, "quarantine_after", "quarantine_after")?;
    let obligation = match &item["obligation"] {
        Value::Null => None,
        value => Some(parse_obligation(value, rev_seq)?),
    };
    let combination_ok = matches!(
        (phase, obligation.as_ref().map(|o| o.state)),
        (Phase::Active, None)
            | (
                Phase::Terminating,
                Some(
                    ObligationState::Open
                        | ObligationState::RetrievalRevoked
                        | ObligationState::Quarantined
                        | ObligationState::Blocked
                        | ObligationState::Escalated
                )
            )
            | (
                Phase::Retired,
                Some(ObligationState::Complete | ObligationState::Escalated)
            )
    );
    if !combination_ok {
        return Err(PageError::Combination);
    }
    Ok(Item {
        holding_id: holding_id.to_owned(),
        resource: ResourceKey {
            kind: kind.to_owned(),
            id: id.to_owned(),
        },
        phase,
        root_pin,
        quarantine_after_ms,
        obligation,
    })
}

fn parse_obligation(value: &Value, rev_seq: u64) -> Result<Obligation, PageError> {
    const REQUIRED: [&str; 8] = [
        "obligation_id",
        "reason",
        "required",
        "state",
        "created_seq",
        "created_at",
        "deadline",
        "last_accepted",
    ];
    let allowed: Vec<&str> = REQUIRED.iter().copied().chain(["blocked"]).collect();
    let object = closed(value, &allowed, &REQUIRED, "obligation members")?;
    let obligation_id = checked_text(
        object,
        "obligation_id",
        grammar::obligation_id,
        "obligation_id",
    )?;
    // v2 §3.3, H.7 (core part): an unrecognised `reason` is opaque. It never
    // changes what the holder does, so it never rejects the item or the list.
    let reason = checked_text(object, "reason", |r| !r.is_empty(), "obligation.reason")?;
    let required = checked_text(object, "required", |r| r == "purge", "obligation.required")?;
    let state = match text(object, "state") {
        Some("open") => ObligationState::Open,
        Some("retrieval_revoked") => ObligationState::RetrievalRevoked,
        Some("quarantined") => ObligationState::Quarantined,
        Some("blocked") => ObligationState::Blocked,
        Some("complete") => ObligationState::Complete,
        Some("escalated") => ObligationState::Escalated,
        _ => return Err(PageError::Grammar("obligation.state")),
    };
    let created_seq = unsigned(object, "created_seq", "obligation.created_seq")?;
    if created_seq > rev_seq {
        return Err(PageError::Grammar("obligation.created_seq above rev"));
    }
    let last_accepted = match &object["last_accepted"] {
        Value::Null => None,
        value => {
            let accepted = closed(value, &["state", "at"], &["state", "at"], "last_accepted")?;
            let state = match text(accepted, "state") {
                Some("revoked") => ReportState::Revoked,
                Some("incomplete") => ReportState::Incomplete,
                Some("complete") => ReportState::Complete,
                _ => return Err(PageError::Grammar("last_accepted.state")),
            };
            Some((state, time(accepted, "at", "last_accepted.at")?))
        }
    };
    let blocked = match object.get("blocked") {
        None => None,
        Some(value) => {
            let blocked = closed(
                value,
                &["by", "at", "status_report_digest"],
                &["by", "at", "status_report_digest"],
                "blocked",
            )?;
            checked_text(blocked, "by", |b| b == "release_declined", "blocked.by")?;
            Some(Blocked {
                at_ms: time(blocked, "at", "blocked.at")?,
                status_report_digest: checked_text(
                    blocked,
                    "status_report_digest",
                    |d| !d.is_empty(),
                    "blocked.status_report_digest",
                )?
                .to_owned(),
            })
        }
    };
    if blocked.is_some() != (state == ObligationState::Blocked) {
        return Err(PageError::Grammar(
            "blocked member must accompany state blocked",
        ));
    }
    Ok(Obligation {
        obligation_id: obligation_id.to_owned(),
        reason: reason.to_owned(),
        required: required.to_owned(),
        state,
        created_seq,
        created_at_ms: time(object, "created_at", "obligation.created_at")?,
        deadline_ms: time(object, "deadline", "obligation.deadline")?,
        last_accepted,
        blocked,
    })
}

/// One listed item with the timing of the page that carried it.
#[derive(Clone, Debug)]
pub struct ListedItem {
    pub item: Item,
    pub page_iat_ms: i64,
    pub received_mono_ms: u64,
}

/// A complete list whose every page verified and whose pages agree.
#[derive(Clone, Debug)]
pub struct VerifiedList {
    pub head: PageHead,
    pub items: Vec<ListedItem>,
    pub instructions: Vec<Value>,
    pub skipped_instructions: usize,
    /// Only page 0's `iat` may support absence inference.
    pub page0_iat_ms: i64,
}

impl VerifiedList {
    pub fn list_ref(&self) -> ListRef {
        ListRef {
            list_id: self.head.list_id.clone(),
            rev: self.head.rev.clone(),
            nonce: self.head.nonce.clone(),
            state_digest: self.head.state_digest.clone(),
            list_tag: self.head.list_tag.clone(),
        }
    }
}

/// Assembles verified pages into one list, or refuses the whole list.
pub fn assemble(pages: Vec<VerifiedPage>) -> Result<VerifiedList, PageError> {
    let first = pages.first().ok_or(PageError::Consistency("no pages"))?;
    let head = first.head.clone();
    let page0_iat_ms = first.iat_ms;
    if usize::try_from(head.count).ok() != Some(pages.len()) {
        return Err(PageError::Consistency("page count"));
    }
    let mut items: Vec<ListedItem> = Vec::new();
    let mut resource_ids = HashSet::new();
    let mut instructions = Vec::new();
    let mut skipped_instructions = 0;
    for (position, page) in pages.into_iter().enumerate() {
        if usize::try_from(page.index).ok() != Some(position) {
            return Err(PageError::Consistency("page index"));
        }
        if page.head != head {
            return Err(PageError::Consistency("pages disagree"));
        }
        if position == 0 {
            instructions = page.instructions;
            skipped_instructions = page.skipped_instructions;
        }
        for item in page.items {
            if let Some(previous) = items.last()
                && previous.item.holding_id.as_bytes() >= item.holding_id.as_bytes()
            {
                return Err(PageError::Consistency("holding ids not strictly ascending"));
            }
            if !resource_ids.insert(item.resource.id.clone()) {
                return Err(PageError::Consistency("resource id listed twice"));
            }
            items.push(ListedItem {
                item,
                page_iat_ms: page.iat_ms,
                received_mono_ms: page.received_mono_ms,
            });
        }
    }
    if u64::try_from(items.len()).ok() != Some(head.items_total) {
        return Err(PageError::Consistency("items_total"));
    }
    Ok(VerifiedList {
        head,
        items,
        instructions,
        skipped_instructions,
        page0_iat_ms,
    })
}
