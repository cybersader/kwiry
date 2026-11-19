//! Authority-specific wire constants, supplied by the composition layer.
//!
//! The protocol's routes, claim names, identifier grammar and state machines
//! are fixed here. The values that name a particular authority, such as `typ`
//! strings, the audience prefix and the resource kinds, are not, so any
//! authority that speaks this wire can be configured without code changes.

/// Route of the list request, appended to the authority base URL.
pub const ROUTE_LIST: &str = "/holdings/list";
/// Route of the key-set statement, appended to the authority base URL.
pub const ROUTE_KEYSET: &str = "/keyset";
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
    /// Prefix of every authority-signed audience; the holder-specific
    /// audience is `prefix + holder_id`, the broadcast audience `prefix + "*"`.
    pub audience_prefix: String,
    /// Prefix every pinned authority key id must carry.
    pub authority_key_id_prefix: String,
    /// The closed list of resource kinds the wire defines.
    pub resource_kinds: Vec<String>,
    /// The kinds this holder accepts; a list naming any other kind is refused.
    pub accepted_resource_kinds: Vec<String>,
    /// Holder kind whose items of `pinned_resource_kind` carry a storage pin.
    pub pinned_holder_kind: Option<String>,
    /// Resource kind that carries the storage pin member for that holder kind.
    pub pinned_resource_kind: Option<String>,
    /// Obligation reasons this holder kind may receive.
    pub obligation_reasons: Vec<String>,
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

    pub fn holder_audience(&self, holder_id: &str) -> String {
        format!("{}{holder_id}", self.audience_prefix)
    }

    pub fn broadcast_audience(&self) -> String {
        format!("{}*", self.audience_prefix)
    }

    /// Whether an item of `resource_kind` carries the storage-pin member for
    /// a holder of `holder_kind`.
    pub fn is_pinned(&self, holder_kind: &str, resource_kind: &str) -> bool {
        self.pinned_holder_kind.as_deref() == Some(holder_kind)
            && self.pinned_resource_kind.as_deref() == Some(resource_kind)
    }

    /// Refuses a profile whose values could not describe a coherent wire.
    pub fn validate(&self) -> Result<(), &'static str> {
        let typs = [
            &self.typ_list_request,
            &self.typ_list_page,
            &self.typ_keyset,
        ];
        if typs
            .iter()
            .any(|t| !t.ends_with("+jwt") || t.contains(char::is_whitespace))
        {
            return Err("every typ must be a +jwt media subtype");
        }
        if typs[0] == typs[1] || typs[1] == typs[2] || typs[0] == typs[2] {
            return Err("typ values must be distinct");
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
        if self.pinned_holder_kind.is_some() != self.pinned_resource_kind.is_some() {
            return Err("pinned holder kind and pinned resource kind go together");
        }
        if self.obligation_reasons.is_empty() {
            return Err("at least one obligation reason is required");
        }
        Ok(())
    }
}
