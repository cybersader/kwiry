// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extraction-policy identity.
//!
//! Two builds of this crate can compile different extractor sets for the same
//! format. Nothing else in the tree notices: [`chunk_id`] digests only the
//! vault, the path, the heading path, and the chunk ordinal, and freshness
//! compares only the registration fingerprint, the byte length, and the mtime.
//! So swapping extractor tiers over an untouched vault would re-ingest nothing
//! and every stale row would keep an identity that a differently-segmented row
//! now also claims. This module is the missing gate.
//!
//! [`chunk_id`]: crate::source
//!
//! # What a profile names
//!
//! A profile names **what was compiled, not who ran it**. A daemon built
//! without the enhanced PDF extractor reports [`ExtractionProfile::Portable`]
//! for PDF and is therefore fully interchangeable with the WASM host — which is
//! the correct semantic, and one a host-named enum (`Wasm`/`Daemon`) would get
//! wrong.
//!
//! [`ExtractionProfile::None`] is a first-class value rather than an absent
//! field, so "this build compiled no extractor for that format" is expressible
//! without the field being optional. Since PDF admission no shipped
//! configuration reports it — the portable PDF tier is in `portable` — but the
//! vocabulary keeps it, because a format admitted ahead of its extractor is the
//! state the digest most needs to be able to name.
//!
//! # What the fingerprint gates
//!
//! An index is a **single-profile artifact**. The fingerprint is written into
//! the manifest and re-derived on load; a mismatch is a full rebuild, never a
//! partial reuse. The plugin folds the same value into its cache policy hash.
//! Neither side has a mechanism that can produce a mixed index, which is why
//! there is no mixed state to reason about.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::format::{SourceFormat, format_specs};

/// Advances when the *shape* of the policy digest changes — a new component,
/// a new ordering rule, a new profile vocabulary. It does not advance when a
/// format's profile merely changes value; the digest already covers that.
pub const EXTRACTION_POLICY_SCHEMA_VERSION: u32 = 1;

/// Which extractor set a build compiled for a format.
///
/// The ordering is deliberate and is *not* a capability ranking used anywhere:
/// it exists only so the enum is `Ord` for use as a `BTreeMap` value in tests.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionProfile {
    /// No extractor is compiled for this format; the source is never read.
    None,
    /// The bundle-constrained extractor set. Byte-identical output on every
    /// host that reports it, which is what makes a portable index and a
    /// portable cache image interchangeable.
    Portable,
    /// The size-unconstrained extractor set. Superset coverage only: it may
    /// index a source the portable set declines, and it must segment a source
    /// both sets index identically.
    Enhanced,
}

impl ExtractionProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Portable => "portable",
            Self::Enhanced => "enhanced",
        }
    }
}

/// The profile this build compiled for `format`.
///
/// Every format is answered, including the ones whose profile cannot vary, so
/// the digest below is total and a newly added format is visible in it rather
/// than silently absent.
pub const fn extraction_profile_for(format: SourceFormat) -> ExtractionProfile {
    match format {
        // These extractors live in `portable` and have exactly one
        // implementation. There is no enhanced variant to select.
        SourceFormat::Markdown
        | SourceFormat::Text
        | SourceFormat::Base
        | SourceFormat::Canvas
        | SourceFormat::Docx
        | SourceFormat::Excalidraw => ExtractionProfile::Portable,
        SourceFormat::Pdf => PDF_PROFILE,
    }
}

/// PDF is the only format with a tiered extractor set. The portable tier is in
/// `portable`, so every build that can extract anything can extract PDF and
/// `None` is no longer reachable — the variant stays because it is the honest
/// answer for a format admitted without an extractor and removing it would make
/// the vocabulary unable to say that. `native-pdf-extractor` is the one feature
/// that still changes what is compiled; what it adds is in
/// `formats::pdf::embedded`.
#[cfg(feature = "native-pdf-extractor")]
const PDF_PROFILE: ExtractionProfile = ExtractionProfile::Enhanced;
#[cfg(not(feature = "native-pdf-extractor"))]
const PDF_PROFILE: ExtractionProfile = ExtractionProfile::Portable;

/// Every format's compiled profile, in `SourceFormat` declaration order.
pub fn active_extraction_policy() -> BTreeMap<SourceFormat, ExtractionProfile> {
    format_specs()
        .iter()
        .map(|spec| (spec.format, extraction_profile_for(spec.format)))
        .collect()
}

/// SHA-256 over a domain separator, the policy schema version, and every
/// `<format>=<profile>` pair in `SourceFormat` order.
///
/// Each component is length-prefixed, so no rearrangement of the same bytes
/// produces the same digest. The value is constant for a build, so it is
/// computed once.
pub fn extraction_policy_fingerprint() -> &'static str {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        let mut digest = Sha256::new();
        digest.update(b"kwiry-extraction-policy-v1\0");
        update_component(
            &mut digest,
            EXTRACTION_POLICY_SCHEMA_VERSION.to_string().as_bytes(),
        );
        for (format, profile) in active_extraction_policy() {
            update_component(
                &mut digest,
                format!("{}={}", format.as_str(), profile.as_str()).as_bytes(),
            );
        }
        format!("{:x}", digest.finalize())
    })
}

fn update_component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest the shipped configuration — every format `portable`,
    /// including PDF — must produce. This is the value the TypeScript host
    /// mirrors in `src/source-formats.ts` and the bench fixture pins in
    /// `bench/portable-core-wasm/fixtures/cases.json`. Pinned so a change to the
    /// material is a deliberate edit here rather than a silent drift away from
    /// the plugin's copy.
    const SHIPPED_FINGERPRINT: &str =
        "efbc627c533ae797104dcf65540dcf6f96edd7b9d96826c4bac7e93672f26ff2";

    #[test]
    fn every_format_has_a_profile() {
        let policy = active_extraction_policy();
        assert_eq!(policy.len(), format_specs().len());
        for spec in format_specs() {
            assert!(policy.contains_key(&spec.format));
        }
    }

    #[test]
    fn the_fingerprint_is_stable_within_a_build() {
        assert_eq!(
            extraction_policy_fingerprint(),
            extraction_policy_fingerprint()
        );
        assert_eq!(extraction_policy_fingerprint().len(), 64);
    }

    /// The whole point of the digest: two builds that compiled different
    /// extractor sets must not be able to claim the same policy identity.
    #[test]
    fn distinct_policies_produce_distinct_identities() {
        fn fingerprint_of(policy: &BTreeMap<SourceFormat, ExtractionProfile>) -> String {
            let mut digest = Sha256::new();
            digest.update(b"kwiry-extraction-policy-v1\0");
            update_component(
                &mut digest,
                EXTRACTION_POLICY_SCHEMA_VERSION.to_string().as_bytes(),
            );
            for (format, profile) in policy {
                update_component(
                    &mut digest,
                    format!("{}={}", format.as_str(), profile.as_str()).as_bytes(),
                );
            }
            format!("{:x}", digest.finalize())
        }

        let mut none = BTreeMap::new();
        for spec in format_specs() {
            none.insert(
                spec.format,
                if spec.format == SourceFormat::Pdf {
                    ExtractionProfile::None
                } else {
                    ExtractionProfile::Portable
                },
            );
        }
        let mut portable = none.clone();
        portable.insert(SourceFormat::Pdf, ExtractionProfile::Portable);
        let mut enhanced = none.clone();
        enhanced.insert(SourceFormat::Pdf, ExtractionProfile::Enhanced);

        let none = fingerprint_of(&none);
        let portable = fingerprint_of(&portable);
        let enhanced = fingerprint_of(&enhanced);

        assert_ne!(none, portable);
        assert_ne!(none, enhanced);
        assert_ne!(portable, enhanced);
        // And the build actually running this test reports one of exactly
        // these three, so the digest is not merely injective in the abstract.
        assert!(
            [none.as_str(), portable.as_str(), enhanced.as_str()]
                .contains(&extraction_policy_fingerprint())
        );
    }

    /// Before PDF admission this test early-returned unless PDF was `None`,
    /// which after admission would make it vacuous on exactly the configuration
    /// it exists to pin. The guard is inverted: the shipped build is the
    /// portable one, and only the enhanced daemon tier — a deliberate
    /// non-shipped divergence — is excused.
    #[test]
    fn the_shipped_policy_digest_is_pinned() {
        if extraction_profile_for(SourceFormat::Pdf) == ExtractionProfile::Enhanced {
            return;
        }
        assert_eq!(
            extraction_profile_for(SourceFormat::Pdf),
            ExtractionProfile::Portable,
            "the shipped configuration extracts PDF with the portable tier"
        );
        assert_eq!(extraction_policy_fingerprint(), SHIPPED_FINGERPRINT);
    }

    #[cfg(not(feature = "native-pdf-extractor"))]
    #[test]
    fn the_portable_pdf_tier_reports_portable() {
        assert_eq!(
            extraction_profile_for(SourceFormat::Pdf),
            ExtractionProfile::Portable
        );
    }

    #[cfg(feature = "native-pdf-extractor")]
    #[test]
    fn the_enhanced_pdf_tier_reports_enhanced() {
        assert_eq!(
            extraction_profile_for(SourceFormat::Pdf),
            ExtractionProfile::Enhanced
        );
    }
}
