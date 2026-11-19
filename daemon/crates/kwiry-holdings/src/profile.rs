//! Authority-specific wire constants, supplied by the composition layer
//! (v2 Part I §1, §2.3, §2.4, §3.3; §C.1 profile hooks).
//!
//! The protocol's routes, claim names, identifier grammar and state machines
//! are fixed here. The values that name a particular authority, such as `typ`
//! strings, the audience prefix and the resource kinds, are not, so any
//! authority that speaks this wire can be configured without code changes.

use crate::engine::EngineProfile;

/// Route of the list request, appended to the authority base URL.
pub const ROUTE_LIST: &str = "/holdings/list";
/// Route of the key-set statement, appended to the authority base URL.
pub const ROUTE_KEYSET: &str = "/keyset";
/// Route of an obligation report, appended to the authority base URL.
pub const ROUTE_REPORT: &str = "/obligations/report";
/// Route of a holder status report, appended to the authority base URL
/// (v2 §11.3, H.4).
pub const ROUTE_STATUS: &str = "/holders/status";
/// Content type of RFC 9457 problem bodies.
pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireProfile {
    /// `typ` of a holder-signed list request.
    pub typ_list_request: String,
    /// `typ` of an authority-signed list page.
    pub typ_list_page: String,
    /// `typ` of an authority-signed key-set statement.
    pub typ_keyset: String,
    /// `typ` of a holder-signed obligation report (v2 §4.1).
    pub typ_report_request: String,
    /// `typ` of an authority-signed report ack (v2 §4.3).
    pub typ_report_ack: String,
    /// `typ` of a holder-signed status report (v2 §11.4.1).
    pub typ_status_request: String,
    /// `typ` of an authority-signed status ack (v2 §11.4.1).
    pub typ_status_ack: String,
    /// Response header that marks a convergence answer to a report (v2
    /// H.5, value `already_complete`).
    pub outcome_header: String,
    /// Prefix of every authority-signed audience; the holder-specific
    /// audience is `prefix + holder_id`, the broadcast audience `prefix + "*"`.
    pub audience_prefix: String,
    /// Prefix every pinned authority key id must carry.
    pub authority_key_id_prefix: String,
    /// The closed list of resource kinds the wire defines.
    pub resource_kinds: Vec<String>,
    /// The kinds this holder accepts; a list naming any other kind is refused.
    pub accepted_resource_kinds: Vec<String>,
    /// Obligation reasons this holder class declares (v2 §C.1). Others are
    /// accepted as opaque (§3.3) and only counted in an alert.
    pub obligation_reasons: Vec<String>,
    /// The search-engine holder profile, or `None` for holders that use the
    /// generic core alone.
    pub engine: Option<EngineProfile>,
}

impl WireProfile {
    pub fn list_request_content_type(&self) -> String {
        format!("application/{}", self.typ_list_request)
    }

    pub fn list_page_content_type(&self) -> String {
        format!("application/{}", self.typ_list_page)
    }

    pub fn keyset_content_type(&self) -> String {
        format!("application/{}", self.typ_keyset)
    }

    pub fn report_request_content_type(&self) -> String {
        format!("application/{}", self.typ_report_request)
    }

    pub fn report_ack_content_type(&self) -> String {
        format!("application/{}", self.typ_report_ack)
    }

    pub fn status_request_content_type(&self) -> String {
        format!("application/{}", self.typ_status_request)
    }

    pub fn status_ack_content_type(&self) -> String {
        format!("application/{}", self.typ_status_ack)
    }

    pub fn holder_audience(&self, holder_id: &str) -> String {
        format!("{}{holder_id}", self.audience_prefix)
    }

    pub fn broadcast_audience(&self) -> String {
        format!("{}*", self.audience_prefix)
    }

    /// Whether items of `resource_kind` carry the storage-pin member for a
    /// holder of `holder_kind`. Always false without the engine profile.
    pub fn pins(&self, holder_kind: &str, resource_kind: &str) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|engine| engine.pins(holder_kind, resource_kind))
    }

    /// Refuses a profile whose values could not describe a coherent wire.
    pub fn validate(&self) -> Result<(), &'static str> {
        let typs = [
            &self.typ_list_request,
            &self.typ_list_page,
            &self.typ_keyset,
            &self.typ_report_request,
            &self.typ_report_ack,
            &self.typ_status_request,
            &self.typ_status_ack,
        ];
        if typs
            .iter()
            .any(|t| !t.ends_with("+jwt") || t.contains(char::is_whitespace))
        {
            return Err("every typ must be a +jwt media subtype");
        }
        let distinct: std::collections::HashSet<&&String> = typs.iter().collect();
        if distinct.len() != typs.len() {
            return Err("typ values must be distinct");
        }
        if self.outcome_header.is_empty()
            || !self
                .outcome_header
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("the outcome header must be a token");
        }
        if self.audience_prefix.is_empty() || self.authority_key_id_prefix.is_empty() {
            return Err("audience and key-id prefixes are required");
        }
        if self.accepted_resource_kinds.is_empty()
            || self
                .accepted_resource_kinds
                .iter()
                .any(|k| !self.resource_kinds.contains(k))
        {
            return Err("accepted kinds must be a non-empty subset of the wire kinds");
        }
        if self.obligation_reasons.is_empty() {
            return Err("at least one obligation reason is required");
        }
        if let Some(engine) = &self.engine
            && (engine.holder_kind.is_empty()
                || !self.resource_kinds.contains(&engine.pinned_resource_kind))
        {
            return Err("the engine section needs a holder kind and a wire resource kind");
        }
        Ok(())
    }
}
