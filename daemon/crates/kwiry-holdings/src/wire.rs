//! Typed forms of the list wire. Parsing and validation live in `verify`.

use crate::engine::PinMember;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// An authority revision: governance `seq` within an epoch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rev {
    pub epoch: String,
    pub seq: u64,
}

impl Rev {
    pub fn to_value(&self) -> Value {
        json!({ "epoch": self.epoch, "seq": self.seq })
    }
}

/// `{list_id, rev, nonce, state_digest, list_tag}`: the holder's reference to
/// a list it applied. Carries no resource id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRef {
    pub list_id: String,
    pub rev: Rev,
    pub nonce: String,
    pub state_digest: String,
    pub list_tag: String,
}

impl ListRef {
    pub fn to_value(&self) -> Value {
        json!({
            "list_id": self.list_id,
            "rev": self.rev.to_value(),
            "nonce": self.nonce,
            "state_digest": self.state_digest,
            "list_tag": self.list_tag,
        })
    }
}

/// A resource as the holder names it: a wire kind and an opaque,
/// holder-scoped identifier.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceKey {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Active,
    Terminating,
    Retired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObligationState {
    Open,
    RetrievalRevoked,
    Quarantined,
    Blocked,
    Complete,
    Escalated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportState {
    Revoked,
    Incomplete,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub at_ms: i64,
    pub status_report_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Obligation {
    pub obligation_id: String,
    pub reason: String,
    pub required: String,
    pub state: ObligationState,
    pub created_seq: u64,
    pub created_at_ms: i64,
    pub deadline_ms: i64,
    pub last_accepted: Option<(ReportState, i64)>,
    pub blocked: Option<Blocked>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub holding_id: String,
    pub resource: ResourceKey,
    pub phase: Phase,
    /// The engine profile's storage-pin member; `NotApplicable` for every
    /// other holder and kind.
    pub root_pin: PinMember,
    pub quarantine_after_ms: i64,
    pub obligation: Option<Obligation>,
}

impl Item {
    /// Listed `active`: the only phase that can ever be served.
    pub fn is_active(&self) -> bool {
        self.phase == Phase::Active
    }

    /// Terminating, or retired with an escalated obligation: stop serving,
    /// keep purging and reporting.
    pub fn is_obligated(&self) -> bool {
        match self.phase {
            Phase::Active => false,
            Phase::Terminating => true,
            Phase::Retired => self
                .obligation
                .as_ref()
                .is_some_and(|o| o.state == ObligationState::Escalated),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    pub serve_seconds: u64,
    pub relist_seconds: u64,
}

/// Claims of a holder-signed list request.
pub struct ListRequest<'a> {
    pub holder_id: &'a str,
    pub audience: &'a str,
    pub htu: &'a str,
    pub iat: i64,
    pub jti: &'a str,
    pub nonce: &'a str,
    pub high_water: Option<&'a Rev>,
    pub page_size: u16,
    pub cursor: Option<&'a str>,
    pub last_list_ref: Option<&'a ListRef>,
}

/// Every request object is signed with `exp = iat + 300`.
pub const REQUEST_LIFETIME_SECONDS: i64 = 300;

impl ListRequest<'_> {
    pub fn claims(&self) -> Value {
        let mut claims = Map::new();
        claims.insert("iss".into(), self.holder_id.into());
        claims.insert("sub".into(), self.holder_id.into());
        claims.insert("aud".into(), self.audience.into());
        claims.insert("htu".into(), self.htu.into());
        claims.insert("iat".into(), self.iat.into());
        claims.insert("exp".into(), (self.iat + REQUEST_LIFETIME_SECONDS).into());
        claims.insert("jti".into(), self.jti.into());
        claims.insert("op".into(), "list".into());
        claims.insert("nonce".into(), self.nonce.into());
        claims.insert(
            "high_water".into(),
            self.high_water.map_or(Value::Null, Rev::to_value),
        );
        claims.insert("page_size".into(), self.page_size.into());
        claims.insert(
            "cursor".into(),
            self.cursor
                .map_or(Value::Null, |c| Value::String(c.to_owned())),
        );
        if let Some(reference) = self.last_list_ref {
            claims.insert("last_list_ref".into(), reference.to_value());
        }
        Value::Object(claims)
    }
}
