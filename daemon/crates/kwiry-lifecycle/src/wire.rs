//! Pure seam rules shared by the lifecycle transitions and the Kwiry-side H.10
//! fake. Booleans/IDs represent already-checked cryptographic artefacts; this
//! module is not a JWS, JCS, HMAC or HTTP implementation.

use crate::design::{Design, Fix};
use crate::state::Rev;

pub const PROOF_TTL: i64 = 300;
pub const SKEW: i64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proof {
    pub jti: u32,
    pub iat: i64,
    pub exp: i64,
}

impl Proof {
    pub fn sign(jti: u32, iat: i64) -> Self {
        Self {
            jti,
            iat,
            exp: iat + PROOF_TTL,
        }
    }

    pub fn timely_at_receipt(self, now: i64) -> bool {
        now.abs_diff(self.iat) <= SKEW as u64 && self.timely_at_commit(now)
    }

    pub fn timely_at_commit(self, now: i64) -> bool {
        now <= self.exp.saturating_add(SKEW)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    Resign,
    BackoffClockAlert,
    Halt,
}

pub fn expiry(design: Design, already_retried: bool) -> Expiry {
    if !design.has(Fix::ProofExpiryRetry) {
        Expiry::Halt
    } else if already_retried {
        Expiry::BackoffClockAlert
    } else {
        Expiry::Resign
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefVerdict {
    CurrentOrAncestor,
    Stale,
    Rollback,
}

/// The bounded epoch lineage is 0, or 0 → 1. `tag_valid` means the tag was
/// recomputed for this holder and all five list-ref fields.
pub fn reference(design: Design, store: Rev, echo: Rev, tag_valid: bool) -> RefVerdict {
    if !tag_valid {
        return RefVerdict::Stale;
    }
    if echo.epoch > store.epoch || (echo.epoch == store.epoch && echo.seq > store.seq) {
        if design.has(Fix::AuthorityFidelity) {
            RefVerdict::Rollback
        } else {
            RefVerdict::Stale
        }
    } else {
        RefVerdict::CurrentOrAncestor
    }
}

pub fn list_echo(design: Design, last_applied: Option<Rev>) -> Option<Rev> {
    if design.has(Fix::EchoListRef) {
        last_applied
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyPin {
    pub seq: u8,
    pub kids: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Keyset {
    pub seq: u8,
    pub signed_by: u8,
    pub verified: bool,
    pub keys: u8,
    pub retire: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownKid;

impl KeyPin {
    pub const ENROLLED: Self = Self { seq: 0, kids: 1 };

    /// `statement` is an attached verified object or the one allowed fetch;
    /// `verified` includes signature, typ, issuer, audience and key grammar.
    /// A successful call returns the set to persist before admitting the list.
    pub fn authorize(
        self,
        design: Design,
        kid: u8,
        statement: Option<Keyset>,
    ) -> Result<Self, UnknownKid> {
        let known = self.kids & kid != 0;
        if !design.has(Fix::SignedKeyRotation) {
            return Ok(Self {
                kids: self.kids | kid,
                ..self
            }); // TLS-only refresh mutation
        }
        let Some(s) =
            statement.filter(|s| s.verified && s.signed_by & self.kids != 0 && s.seq > self.seq)
        else {
            return if known { Ok(self) } else { Err(UnknownKid) };
        };
        let kids = s.keys & !s.retire;
        if kids & kid == 0 {
            return Err(UnknownKid);
        }
        Ok(Self { seq: s.seq, kids })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckClass {
    Receipt,
    Convergence,
    AuditAndRelist,
    Unverifiable,
}

/// `current_ack` is the signed extension in the problem body, not the body
/// itself. `outcome_header` denotes the exact `already_complete` header.
pub fn classify_ack<K: Eq>(
    design: Design,
    sent_key: K,
    ack_key: K,
    current_ack: bool,
    outcome_header: bool,
    same_epoch: bool,
    known_attempt: bool,
) -> AckClass {
    if sent_key != ack_key
        || (design.has(Fix::ConvergenceEnvelope) && (current_ack || outcome_header))
    {
        if same_epoch {
            AckClass::Convergence
        } else {
            AckClass::AuditAndRelist
        }
    } else if !same_epoch {
        AckClass::AuditAndRelist
    } else if known_attempt {
        AckClass::Receipt
    } else {
        AckClass::Unverifiable
    }
}

/// G.5 release authorization: every bound unit must still be unattributed.
pub fn release_matches(retained: Option<u8>, approved_count: u8, unattributed: u8) -> bool {
    retained.is_some_and(|m| {
        m != 0 && m.count_ones() == u32::from(approved_count) && m & !unattributed == 0
    })
}

/// K.9 total budget after verified page 0, measured from the first start.
pub fn list_budget_seconds(items_total: u64) -> u64 {
    let pages = items_total.div_ceil(500).max(1);
    pages.saturating_mul(2).saturating_add(30).clamp(120, 1800)
}
