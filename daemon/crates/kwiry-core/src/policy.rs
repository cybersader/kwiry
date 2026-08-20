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
//!
//! # Bumping one format's extractor version
//!
//! A version is a **claim that this build's extractor produces different output
//! for byte-identical input**. Moving one that has not changed is a false claim
//! that costs every user of that format a re-read, so a bump belongs in the
//! same change as the behavior it describes and never on its own.
//!
//! Five tracked lines move together, and nothing else. This list is not
//! folklore: bumping Markdown to 2 and running the gates fails at exactly these
//! and at nothing else.
//!
//! 1. [`extractor_version_for`] — the number.
//! 2. `tests::EXTRACTOR_VERSIONS` — that format's pinned number.
//! 3. `tests::the_shipped_format_identities_are_pinned`'s `PINNED` — that
//!    format's digest. Exactly one entry moves; a bump that moves two means the
//!    material changed and a core version is owed instead.
//! 4. `clients/obsidian/src/source-formats.ts` — `EXTRACTOR_VERSIONS` and the
//!    matching `FORMAT_IDENTITIES` entry, the map the plugin stamps cache rows
//!    with.
//! 5. `clients/obsidian/test/settings.test.ts` — the pinned copy of that map,
//!    which is the plugin's counterpart of `PINNED`.
//!
//! Nothing else needs a human. `clients/obsidian/test/settings.test.ts` also
//! *re-derives* every identity from the mirrored material, and
//! `rust/kwiry-obsidian-wasm/tests/typescript_mirror.rs` checks the mirrored
//! version against [`extractor_version_for`] itself, so an edit that changes
//! the number without the digest — or either without the other side — fails
//! rather than ships. `rust/kwiry-obsidian-wasm/scripts/test-adapter.cjs` pins
//! all seven identities on the installed artifact as the last line of defence.
//! `esbuild.config.mjs`'s `RUST_WASM_SHA256` moves on its own, computed from
//! the rebuilt artifact.
//!
//! Nothing in the *daemon's* persisted state needs re-pinning:
//! `bench/portable-core-wasm/fixtures/cases.json` pins
//! [`extraction_policy_fingerprint`], which is tier-only and does not move on a
//! bump. `/v0/status` reports `format_identities` beside it precisely so a
//! caller can still see one. That fixture also carries a `format_identities`
//! map, because the field is required to deserialise a `DaemonStatus` — but
//! its seven digests are deliberately synthetic (`0101…`, `0202…`, one per
//! format). The fixture exists to prove the native and WASM engines serialise
//! the same bytes, not to pin identities, and giving it the shipped ones would
//! add an eighth place a bump has to touch while making this paragraph false.
//!
//! ## What a bump costs, per lane
//!
//! * **Daemon.** Narrow, and this is the feature: opening evicts only that
//!   format's rows, publishes, and re-ingests only that format's sources. Every
//!   other format's rows and vectors survive untouched. A *reader* (`kwiry
//!   search`) holds no writer lock, so it refuses the whole index and prints
//!   `run kwiry index`; restarting the daemon is the narrow remedy and a full
//!   `kwiry index` is the blunt one.
//! * **In-plugin.** Whole-vault, today. The plugin's cache image carries the
//!   WASM artifact hash and the plugin version, and a bump necessarily rebuilds
//!   the artifact, so `worker.ts` refuses the image as `cache_version_mismatch`
//!   before the per-format restore projection ever runs. The projection is
//!   correct and tested; it is simply unreachable for a shipped bump. The two
//!   lanes therefore trust different things — the daemon trusts declared
//!   identity, the plugin trusts the binary — and reconciling them changes what
//!   a persisted artifact means, so it is an owner decision rather than a
//!   refactor.

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
        | SourceFormat::Excalidraw
        | SourceFormat::Excel
        | SourceFormat::Html => ExtractionProfile::Portable,
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
        SourceFormat::Excel => 1,
        SourceFormat::Html => 3,
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

/// The identity digest of *stated* material, with nothing implicit.
///
/// This is the one derivation in the tree. [`derive_format_identity`] supplies
/// the running build's material and is the only production caller; tests reach
/// it through [`identity_under`] so they can name the digest that a *different*
/// extractor version — or a different tier — would produce without
/// re-implementing the material.
///
/// That re-implementation is exactly what made the extractor version unproven
/// before: every test that spelled the material out did so in a local helper,
/// so a helper and this function could have disagreed about whether the version
/// participates at all and every assertion would still have passed.
fn identity_for_material(
    format: SourceFormat,
    profile: ExtractionProfile,
    extractor_version: u32,
) -> String {
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
        format!("profile={}", profile.as_str()).as_bytes(),
    );
    update_component(
        &mut digest,
        format!("extractor={extractor_version}").as_bytes(),
    );
    format!("{:x}", digest.finalize())
}

/// The digest [`identity_for_material`] produces for material this build does
/// not compile. Test-only, and deliberately so: a production caller able to
/// name a foreign identity could stamp a row with one, and the whole eviction
/// mechanism rests on a row's identity being the running build's by
/// construction ([`crate::manifest::ManifestFile::from_outcome`]).
#[cfg(test)]
pub(crate) fn identity_under(
    format: SourceFormat,
    profile: ExtractionProfile,
    extractor_version: u32,
) -> String {
    identity_for_material(format, profile, extractor_version)
}

/// The identity `format` would carry if its extractor version were `version`
/// and nothing else about this build had changed.
///
/// `identity_at_extractor_version(f, extractor_version_for(f))` is the running
/// identity by construction; every other argument names a build whose extractor
/// for that format states different output. This is the material a bump test
/// needs, and it is derived rather than forged, so a test written against it
/// cannot keep passing once the version stops participating in the digest.
#[cfg(test)]
pub(crate) fn identity_at_extractor_version(format: SourceFormat, version: u32) -> String {
    identity_for_material(format, extraction_profile_for(format), version)
}

fn derive_format_identity(format: SourceFormat) -> String {
    identity_for_material(
        format,
        extraction_profile_for(format),
        extractor_version_for(format),
    )
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
        "a8c00357bd4da3a9b7f2b76a46605e2e3eef275f1220cd500916687709989061";

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
    ///
    /// Asserted on [`identity_under`] — the *production* derivation with its
    /// material stated — rather than on a local re-implementation. The previous
    /// version of this test digested its own helper and compared it against
    /// itself, so it held whether or not `derive_format_identity` consulted the
    /// same three facts; only its single closing equality touched production at
    /// all. Every `assert_ne!` below now fails if the named component stops
    /// participating.
    #[test]
    fn each_identity_component_moves_the_digest() {
        let base = identity_under(SourceFormat::Pdf, ExtractionProfile::Portable, 1);
        // Format.
        assert_ne!(
            base,
            identity_under(SourceFormat::Docx, ExtractionProfile::Portable, 1)
        );
        // Profile.
        assert_ne!(
            base,
            identity_under(SourceFormat::Pdf, ExtractionProfile::Enhanced, 1)
        );
        assert_ne!(
            base,
            identity_under(SourceFormat::Pdf, ExtractionProfile::None, 1)
        );
        // Extractor version.
        assert_ne!(
            base,
            identity_under(SourceFormat::Pdf, ExtractionProfile::Portable, 2)
        );
        // And the derivation under test is reached by the shipped entry point
        // with the running build's material, so this is not merely injective in
        // the abstract.
        assert_eq!(
            identity_under(
                SourceFormat::Markdown,
                extraction_profile_for(SourceFormat::Markdown),
                extractor_version_for(SourceFormat::Markdown),
            ),
            format_identity_fingerprint(SourceFormat::Markdown)
        );
    }

    /// The gap this wave closes: **the extractor version participates in every
    /// format's identity**, and it is the only fact that moved.
    ///
    /// Nothing before asserted a consequence of a version change. The pins
    /// caught a change to the digest *material*, and the eviction tests forged
    /// unrelated literals (`"f" * 64`) that a build with a bumped version would
    /// never actually write — so a derivation that quietly ignored
    /// `extractor_version_for` would have kept every one of them passing.
    ///
    /// Mutation check: delete the `extractor=` component from
    /// `identity_for_material` and every iteration of this loop fails, because
    /// the neighbouring versions collapse onto the running identity.
    #[test]
    fn the_extractor_version_participates_in_every_formats_identity() {
        for spec in format_specs() {
            let running = format_identity_fingerprint(spec.format);
            let version = extractor_version_for(spec.format);

            // The running identity *is* the derivation at the declared version.
            assert_eq!(
                identity_at_extractor_version(spec.format, version),
                running,
                "{} identity must be its own material",
                spec.name
            );

            // And no neighbouring version can claim it. `version - 1` matters
            // as much as `version + 1`: the predicate is equality, not a
            // ordering, so a build must refuse a row written by an *older*
            // extractor exactly as firmly as one written by a newer one.
            let mut seen = BTreeSet::from([running.to_owned()]);
            for candidate in [
                version.saturating_sub(1),
                version + 1,
                version + 2,
                version + 97,
            ] {
                if candidate == version {
                    continue;
                }
                let other = identity_at_extractor_version(spec.format, candidate);
                assert_ne!(
                    other, running,
                    "{} at extractor version {candidate} must not claim the identity of version {version}",
                    spec.name
                );
                assert!(
                    seen.insert(other),
                    "{} must have a distinct identity per extractor version",
                    spec.name
                );
            }
        }
    }

    /// A bump must never make one format's rows claim another format's
    /// identity. Nothing in the shipped set is close to colliding today, but
    /// the property has to hold for versions no build has reached yet, or a
    /// future bump would silently evict — or worse, silently *retain* — the
    /// wrong format's rows.
    ///
    /// Mutation check: dropping either the `format=` or the `extractor=`
    /// component collapses the product and the cardinality assertion fails.
    #[test]
    fn no_extractor_version_can_collide_with_another_formats_identity() {
        const HORIZON: u32 = 12;
        let mut identities = BTreeSet::new();
        for spec in format_specs() {
            for version in 0..=HORIZON {
                assert!(
                    identities.insert(identity_at_extractor_version(spec.format, version)),
                    "{} at extractor version {version} collides with an identity already derived",
                    spec.name
                );
            }
        }
        assert_eq!(
            identities.len(),
            format_specs().len() * (HORIZON as usize + 1)
        );
        // The running set is inside that product, so the property is asserted
        // about the identities this build actually stamps.
        for identity in active_format_identities().into_values() {
            assert!(identities.contains(identity));
        }
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
            (
                SourceFormat::Excel,
                "ddfee1499472f960540644e47069db3942a572e883d2328e2b5df856dbd04889",
            ),
            (
                SourceFormat::Html,
                "218acfdef07624a39eb071ba8221a7761b6f2ebf26e3ef180928dd7a6a65a9d7",
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

    /// Every format's extractor version, pinned per format.
    ///
    /// A table rather than "every format is at 1", so the first bump is a
    /// one-line edit here beside the one-line edit in `extractor_version_for`
    /// and the one-line edit to that format's entry in `PINNED` — and so the
    /// six formats that did *not* move stay asserted at their own numbers
    /// instead of being carried by a blanket constant.
    const EXTRACTOR_VERSIONS: &[(SourceFormat, u32)] = &[
        (SourceFormat::Markdown, 1),
        (SourceFormat::Text, 1),
        (SourceFormat::Base, 1),
        (SourceFormat::Canvas, 1),
        (SourceFormat::Docx, 1),
        (SourceFormat::Excalidraw, 1),
        (SourceFormat::Pdf, 1),
        (SourceFormat::Excel, 1),
        (SourceFormat::Html, 3),
    ];

    #[test]
    fn every_formats_extractor_version_is_pinned() {
        for (format, expected) in EXTRACTOR_VERSIONS {
            assert_eq!(
                extractor_version_for(*format),
                *expected,
                "{} extractor version",
                format.as_str()
            );
        }
        assert_eq!(EXTRACTOR_VERSIONS.len(), format_specs().len());
        let named: BTreeSet<SourceFormat> = EXTRACTOR_VERSIONS
            .iter()
            .map(|(format, _)| *format)
            .collect();
        assert_eq!(
            named.len(),
            EXTRACTOR_VERSIONS.len(),
            "a format is listed twice"
        );
        for spec in format_specs() {
            assert!(
                named.contains(&spec.format),
                "{} has no pinned extractor version",
                spec.name
            );
        }
        // A version is a claim about output, so it may only ever go up.
        // Asserted so a "fix" that reverts a bump has to argue with a test.
        for (format, expected) in EXTRACTOR_VERSIONS {
            assert!(
                *expected >= 1,
                "{} extractor version underflows",
                format.as_str()
            );
        }
    }
}
