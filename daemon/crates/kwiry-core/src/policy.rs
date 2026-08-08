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
//! # Split identity: what gates what
//!
//! Identity is split in two, because the two facts it used to conflate
//! invalidate different amounts of work.
//!
//! * **Core identity** is the container and the shared, format-blind pipeline:
//!   the manifest record shape, the index schema, the chunker, the preparation
//!   schema, and [`FORMAT_IDENTITY_SCHEMA_VERSION`] itself. A core mismatch
//!   means no row of any format is usable, and it is refused whole at
//!   [`Manifest::validate_readable`].
//! * **Per-format identity** is [`format_identity_fingerprint`]: exactly three
//!   facts about one format — its name, the profile this build compiled for it,
//!   and [`extractor_version_for`]. It is written onto every
//!   [`ManifestFile`] and compared per row. A format whose identity moved has
//!   its rows evicted; every other format's rows survive untouched.
//!
//! [`Manifest::validate_readable`]: crate::manifest::Manifest::validate_readable
//! [`ManifestFile`]: crate::manifest::ManifestFile
//!
//! An index is therefore no longer a single-*policy* artifact, but it is still
//! single-identity-*per-format* at serve time: eviction is a mandatory
//! open-time transformation ([`ManifestOnDisk::adopt`]), not a read-time
//! filter, so no row is ever served under an identity it was not built under.
//!
//! [`ManifestOnDisk::adopt`]: crate::manifest::ManifestOnDisk::adopt
//!
//! [`extraction_policy_fingerprint`] survives as a **report-only** value. It is
//! served by `/v0/status` and `/v0/search`, and the WASM host mirrors it, so it
//! still answers "which extractor set is this build running"; it no longer
//! gates anything and it is no longer persisted.
//!
//! # The editorial rule the mechanism cannot enforce
//!
//! Neither digest can detect an extractor whose *implementation* changed while
//! its profile name stayed `portable`. The only defense is discipline:
//!
//! * any change that can alter what a format's extractor produces for
//!   byte-identical input **must** bump that format's [`extractor_version_for`];
//! * any change to the container, the chunker, the identity derivation, the
//!   normalizer, the property-projection shape, or a persisted record shape
//!   **must** bump a core version;
//! * if a change is both, bump both. **If it is unclear which, bump core** —
//!   fail toward the blunt instrument.
//!
//! `tests::the_shipped_format_identities_are_pinned` makes every such bump a
//! deliberate edit to a pinned constant rather than a silent drift.

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

/// Advances when the *shape* of the per-format identity changes — a new
/// component, a new ordering rule, a new domain separator.
///
/// This constant is **core**: adding a fourth component to
/// [`format_identity_fingerprint`] bumps it, and a core mismatch invalidates
/// every row of every format. That is the intended escape hatch, and it is the
/// reason the per-format digest may stay deliberately small.
pub const FORMAT_IDENTITY_SCHEMA_VERSION: u32 = 1;

/// How many times this format's extractor has been given the ability to produce
/// different output for byte-identical input.
///
/// This is **not** a capability level, **not** a release number, and **not** a
/// tier. It is the field that lets a behavior change be stated without lying
/// about a schema: bumping it evicts exactly that format's rows and reads
/// exactly that format's sources again.
///
/// Every format starts at 1. The split-identity wave is the first record shape
/// that can carry the number, so no pre-wave build's output is describable by
/// it — which is precisely why `MANIFEST_VERSION` moved rather than the numbers
/// here starting anywhere else.
pub const fn extractor_version_for(format: SourceFormat) -> u32 {
    match format {
        SourceFormat::Markdown => 1,
        SourceFormat::Text => 1,
        SourceFormat::Base => 1,
        SourceFormat::Canvas => 1,
        SourceFormat::Docx => 1,
        SourceFormat::Pdf => 1,
        SourceFormat::Excalidraw => 1,
    }
}

/// The identity of one format's extraction, as this build compiles it.
///
/// SHA-256 over a domain separator, [`FORMAT_IDENTITY_SCHEMA_VERSION`], and
/// exactly three length-prefixed components: the format, its profile, and its
/// extractor version. Nothing else belongs here — not whether a host has the
/// format enabled (a configuration fact, never persisted), not the preparation
/// schema, not the chunker (both core).
///
/// The value is constant for a build, so every format's digest is computed once.
pub fn format_identity_fingerprint(format: SourceFormat) -> &'static str {
    static IDENTITIES: OnceLock<BTreeMap<SourceFormat, String>> = OnceLock::new();
    IDENTITIES
        .get_or_init(|| {
            format_specs()
                .iter()
                .map(|spec| (spec.format, derive_format_identity(spec.format)))
                .collect()
        })
        .get(&format)
        .map(String::as_str)
        .expect("every source format has a registry entry")
}

/// Every format's compiled identity, in `SourceFormat` declaration order.
///
/// Report-only in this crate; it exists so a host can mirror the map and a test
/// can assert the key set equals the `SourceFormat` variant set.
pub fn active_format_identities() -> BTreeMap<SourceFormat, &'static str> {
    format_specs()
        .iter()
        .map(|spec| (spec.format, format_identity_fingerprint(spec.format)))
        .collect()
}

fn derive_format_identity(format: SourceFormat) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kwiry-format-identity-v1\0");
    update_component(
        &mut digest,
        FORMAT_IDENTITY_SCHEMA_VERSION.to_string().as_bytes(),
    );
    update_component(
        &mut digest,
        format!("format={}", format.as_str()).as_bytes(),
    );
    update_component(
        &mut digest,
        format!("profile={}", extraction_profile_for(format).as_str()).as_bytes(),
    );
    update_component(
        &mut digest,
        format!("extractor={}", extractor_version_for(format)).as_bytes(),
    );
    format!("{:x}", digest.finalize())
}

fn update_component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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

    #[test]
    fn every_format_has_an_identity_and_the_map_is_total() {
        let identities = active_format_identities();
        assert_eq!(identities.len(), format_specs().len());
        for spec in format_specs() {
            let identity = identities[&spec.format];
            assert_eq!(identity, format_identity_fingerprint(spec.format));
            assert_eq!(identity.len(), 64);
            assert!(identity.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    /// The whole point of splitting the identity: two formats must never share
    /// one, or evicting one would evict the other.
    #[test]
    fn distinct_formats_have_distinct_identities() {
        let identities: BTreeSet<&str> = active_format_identities().into_values().collect();
        assert_eq!(identities.len(), format_specs().len());
    }

    /// Each of the three components must move the digest on its own; otherwise
    /// a change to that component would be invisible to the row predicate.
    #[test]
    fn each_identity_component_moves_the_digest() {
        fn identity_of(format: &str, profile: &str, extractor: u32) -> String {
            let mut digest = Sha256::new();
            digest.update(b"kwiry-format-identity-v1\0");
            update_component(
                &mut digest,
                FORMAT_IDENTITY_SCHEMA_VERSION.to_string().as_bytes(),
            );
            update_component(&mut digest, format!("format={format}").as_bytes());
            update_component(&mut digest, format!("profile={profile}").as_bytes());
            update_component(&mut digest, format!("extractor={extractor}").as_bytes());
            format!("{:x}", digest.finalize())
        }

        let base = identity_of("pdf", "portable", 1);
        assert_ne!(base, identity_of("docx", "portable", 1));
        assert_ne!(base, identity_of("pdf", "enhanced", 1));
        assert_ne!(base, identity_of("pdf", "none", 1));
        assert_ne!(base, identity_of("pdf", "portable", 2));
        // And the derivation under test agrees with the material spelled out
        // here, so this is not merely injective in the abstract.
        assert_eq!(
            identity_of(
                SourceFormat::Markdown.as_str(),
                extraction_profile_for(SourceFormat::Markdown).as_str(),
                extractor_version_for(SourceFormat::Markdown),
            ),
            format_identity_fingerprint(SourceFormat::Markdown)
        );
    }

    /// The pinned per-format digests of the shipped configuration.
    ///
    /// This is the mechanical half of the editorial rule in the module doc: a
    /// profile change, an `extractor_version_for` bump, or a change to the
    /// digest material all land here as a deliberate edit rather than as
    /// silent drift. Adding a format adds a line; it does not change any other.
    #[test]
    fn the_shipped_format_identities_are_pinned() {
        const PINNED: &[(SourceFormat, &str)] = &[
            (
                SourceFormat::Markdown,
                "b678d0ea2d77d7a79ccc79f4f8a3a1d96aed9bb98757afb1381e5661a1fb96f7",
            ),
            (
                SourceFormat::Text,
                "c89bb1c6cb87c1e6371d7d03956f1c6bf8bff605c847441c2c72d7599bbd464b",
            ),
            (
                SourceFormat::Base,
                "d3eeb5a8e3246a07f0c1e41782a7f61628921f43f7afdd722f3a060104e7e079",
            ),
            (
                SourceFormat::Canvas,
                "01eae3d6859de3287237e366b7fcd9f346dbab395453ef9422bcd67dc527858c",
            ),
            (
                SourceFormat::Docx,
                "b4f9cff615a917e09d800c2784e17c836ef79cc767c49091818a7b1f8598a38e",
            ),
            (
                SourceFormat::Excalidraw,
                "e1f6868bd320172f6b8d9afc3ac716e309499b065c62fa1b17ae4c2c09d98348",
            ),
            (
                SourceFormat::Pdf,
                "980924c70d64fc5de65ddc2141d043e9188f8856ec6196d30c0d5c11d363c3bc",
            ),
        ];

        // The enhanced daemon tier is a deliberate non-shipped divergence; only
        // PDF's identity moves under it, which is the narrowing this wave buys.
        let enhanced_pdf = extraction_profile_for(SourceFormat::Pdf) == ExtractionProfile::Enhanced;
        for (format, expected) in PINNED {
            if enhanced_pdf && *format == SourceFormat::Pdf {
                assert_ne!(
                    format_identity_fingerprint(*format),
                    *expected,
                    "the enhanced PDF tier must not claim the portable identity"
                );
                continue;
            }
            assert_eq!(
                format_identity_fingerprint(*format),
                *expected,
                "pinned identity for {}",
                format.as_str()
            );
        }
        assert_eq!(PINNED.len(), format_specs().len());
    }

    /// Every format is at 1 and nothing has been bumped yet. When the first
    /// extractor behavior change lands, this assertion is the line that has to
    /// be edited alongside it.
    #[test]
    fn no_extractor_version_has_been_bumped_yet() {
        for spec in format_specs() {
            assert_eq!(
                extractor_version_for(spec.format),
                1,
                "{} extractor version",
                spec.name
            );
        }
    }
}
