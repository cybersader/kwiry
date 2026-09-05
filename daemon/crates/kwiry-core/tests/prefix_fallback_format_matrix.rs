// SPDX-License-Identifier: MIT OR Apache-2.0

#[cfg_attr(not(feature = "native"), allow(dead_code))]
#[path = "support/format_matrix_fixtures.rs"]
mod format_matrix_fixtures;

use std::collections::BTreeSet;
#[cfg(feature = "native")]
use std::{fs, path::Path};

use format_matrix_fixtures::{RawFormatFixture, format_matrix_fixtures};
#[cfg(feature = "native")]
use format_matrix_fixtures::{STANDARD_PATH, standard_markdown};
#[cfg(feature = "native")]
use kwiry_core::{
    Config, LexicalSearchRequest, SearchHit, VaultRegistration, build_index, search_index,
};
use kwiry_core::{
    ExtractionCoverage, SourceDescriptor, SourceFormat, SourceLocator, SourcePreparationKind,
    prepare_source_buffer,
};
#[cfg(feature = "native")]
use tempfile::tempdir;

const VAULT_ID: &str = "format-matrix-vault";

#[test]
fn compact_raw_fixtures_prepare_all_supported_formats_without_invented_metadata() {
    let fixtures = format_matrix_fixtures();
    assert_eq!(fixtures.len(), 9);
    assert_eq!(
        fixtures
            .iter()
            .map(|fixture| fixture.format)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            SourceFormat::Markdown,
            SourceFormat::Text,
            SourceFormat::Base,
            SourceFormat::Canvas,
            SourceFormat::Docx,
            SourceFormat::Pdf,
            SourceFormat::Excalidraw,
            SourceFormat::Excel,
            SourceFormat::Html,
        ])
    );

    for fixture in fixtures {
        let preparation = prepare_fixture(&fixture);
        assert_eq!(
            preparation.kind,
            SourcePreparationKind::Indexed,
            "{}",
            fixture.path
        );
        assert_eq!(
            preparation.coverage,
            ExtractionCoverage::IndexedComplete,
            "{}",
            fixture.path
        );
        assert!(!preparation.chunks.is_empty(), "{}", fixture.path);
        assert!(preparation.retrieval.aliases.is_empty(), "{}", fixture.path);

        let terms = fixture.query.split_whitespace().collect::<Vec<_>>();
        assert_eq!(terms.len(), 3);
        let stem_terms = preparation
            .retrieval
            .stem
            .split(|character: char| !character.is_alphanumeric())
            .collect::<Vec<_>>();
        assert!(stem_terms.iter().any(|value| value.starts_with(terms[0])));
        assert!(stem_terms.iter().any(|value| value.starts_with(terms[1])));
        assert!(!stem_terms.iter().any(|value| value.starts_with(terms[2])));
        for term in &terms {
            assert!(
                preparation.chunks.iter().all(|chunk| {
                    !chunk.content.to_lowercase().contains(term)
                        && !chunk.heading_text.to_lowercase().contains(term)
                }),
                "{} must receive query evidence from its name rather than authored content",
                fixture.path
            );
        }

        match fixture.format {
            SourceFormat::Markdown => {
                assert!(
                    preparation
                        .chunks
                        .iter()
                        .any(|chunk| chunk.heading_path == ["Fixture"])
                );
                assert!(
                    preparation
                        .chunks
                        .iter()
                        .all(|chunk| chunk.source_locator.is_none())
                );
                assert_eq!(preparation.normalized_exact.title, None);
            }
            SourceFormat::Text => assert_plain_chunks(&preparation.chunks),
            SourceFormat::Base => {
                assert_eq!(
                    preparation.normalized_exact.title.as_deref(),
                    Some("compact base")
                );
                assert!(preparation.chunks.iter().any(|chunk| {
                    chunk.source_locator
                        == Some(SourceLocator::BaseView {
                            view: "Inventory".to_owned(),
                        })
                }));
            }
            SourceFormat::Canvas => assert_plain_chunks(&preparation.chunks),
            SourceFormat::Docx => assert_plain_chunks(&preparation.chunks),
            SourceFormat::Pdf => {
                assert!(
                    preparation
                        .chunks
                        .iter()
                        .all(|chunk| chunk.heading_path.is_empty())
                );
                assert!(preparation.chunks.iter().all(|chunk| {
                    chunk.source_locator == Some(SourceLocator::PdfPage { page: 1 })
                }));
                assert_eq!(preparation.normalized_exact.title, None);
            }
            SourceFormat::Excalidraw => assert_plain_chunks(&preparation.chunks),
            SourceFormat::Excel => {
                assert_eq!(preparation.normalized_exact.title, None);
                assert!(preparation.chunks.iter().any(|chunk| {
                    chunk.source_locator
                        == Some(SourceLocator::ExcelCell {
                            sheet: "Ledger".to_owned(),
                            cell: "A1".to_owned(),
                        })
                }));
            }
            SourceFormat::Html => {
                assert_eq!(
                    preparation.normalized_exact.title.as_deref(),
                    Some("compact html")
                );
                assert_plain_chunks(&preparation.chunks);
            }
        }
    }
}

#[cfg(feature = "native")]
#[test]
fn native_sparse_prefix_fallback_admits_each_real_format_below_a_standard_three_term_hit() {
    let temporary = tempdir().expect("temporary matrix root");
    let vault = temporary.path().join("vault");
    let data = temporary.path().join("data");
    write_vault(&vault);
    let config = Config {
        vaults: vec![VaultRegistration {
            id: VAULT_ID.to_owned(),
            path: vault,
            room: None,
        }],
        ..Config::default()
    };

    build_index(&config, &data).expect("build first matrix index");
    let first = assert_matrix_searches(&data);
    let repeated = assert_matrix_searches(&data);
    assert_eq!(first, repeated, "repeated searches must be deterministic");

    build_index(&config, &data).expect("rebuild matrix index");
    let rebuilt = assert_matrix_searches(&data);
    assert_eq!(
        first, rebuilt,
        "rebuilding the same raw sources must preserve result identity"
    );
}

fn prepare_fixture(fixture: &RawFormatFixture) -> kwiry_core::SourcePreparation {
    let descriptor = SourceDescriptor {
        vault_id: VAULT_ID.to_owned(),
        room: None,
        path: fixture.path.to_owned(),
        format: fixture.format,
        byte_length: fixture.bytes.len() as u64,
        mtime: 1,
        mtime_nanos: 1_000_001,
    };
    prepare_source_buffer(&descriptor, &fixture.bytes).expect("prepare compact format fixture")
}

fn assert_plain_chunks(chunks: &[kwiry_core::PreparedChunk]) {
    assert!(chunks.iter().all(|chunk| chunk.heading_path.is_empty()));
    assert!(chunks.iter().all(|chunk| chunk.source_locator.is_none()));
}

#[cfg(feature = "native")]
fn write_vault(vault: &Path) {
    for fixture in format_matrix_fixtures() {
        let path = vault.join(fixture.path);
        fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        fs::write(path, fixture.bytes).expect("write compact raw fixture");
    }
    let standard = vault.join(STANDARD_PATH);
    fs::create_dir_all(standard.parent().expect("standard parent"))
        .expect("create standard directory");
    fs::write(standard, standard_markdown()).expect("write standard source");
}

#[cfg(feature = "native")]
type HitIdentity = (
    String,
    String,
    Vec<String>,
    SourceFormat,
    ExtractionCoverage,
    Option<SourceLocator>,
);

#[cfg(feature = "native")]
fn assert_matrix_searches(data: &Path) -> Vec<(String, Vec<HitIdentity>)> {
    let mut results = Vec::new();
    for fixture in format_matrix_fixtures() {
        let terms = fixture.query.split_whitespace().collect::<Vec<_>>();
        for width in [1, 2] {
            let query = terms[..width].join(" ");
            let hits = search(data, &query);
            assert!(
                hits.iter().any(|hit| hit.path == fixture.path),
                "{width}-term query {query:?} must retain {}",
                fixture.path
            );
        }

        let hits = search(data, fixture.query);
        assert_eq!(
            hits.first().map(|hit| hit.path.as_str()),
            Some(STANDARD_PATH),
            "the complete three-term standard source must precede the partial name match for {:?}",
            fixture.format
        );
        let target_hits = hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.path == fixture.path)
            .collect::<Vec<_>>();
        assert!(
            target_hits.iter().any(|(index, _)| *index > 0),
            "the two-name-signal source must be admitted below the standard hit for {:?}",
            fixture.format
        );
        assert!(
            hits.iter()
                .all(|hit| hit.path == STANDARD_PATH || hit.path == fixture.path),
            "the format query must not admit an unrelated source: {:?}",
            fixture.format
        );
        assert!(target_hits.iter().all(|(_, hit)| {
            hit.format == fixture.format && hit.coverage == ExtractionCoverage::IndexedComplete
        }));
        assert_search_metadata(
            fixture.format,
            &target_hits.iter().map(|(_, hit)| *hit).collect::<Vec<_>>(),
        );

        results.push((
            fixture.query.to_owned(),
            hits.iter().map(hit_identity).collect(),
        ));
    }
    results
}

#[cfg(feature = "native")]
fn search(data: &Path, query: &str) -> Vec<SearchHit> {
    search_index(
        data,
        &LexicalSearchRequest {
            query: query.to_owned(),
            limit: 100,
            vault_id: Some(VAULT_ID.to_owned()),
        },
    )
    .expect("search format matrix")
}

#[cfg(feature = "native")]
fn assert_search_metadata(format: SourceFormat, hits: &[&SearchHit]) {
    match format {
        SourceFormat::Pdf => assert!(hits.iter().all(|hit| {
            hit.heading_path.is_empty() && hit.locator == Some(SourceLocator::PdfPage { page: 1 })
        })),
        SourceFormat::Excel => assert!(hits.iter().any(|hit| {
            hit.locator
                == Some(SourceLocator::ExcelCell {
                    sheet: "Ledger".to_owned(),
                    cell: "A1".to_owned(),
                })
        })),
        SourceFormat::Base => assert!(hits.iter().any(|hit| {
            hit.locator
                == Some(SourceLocator::BaseView {
                    view: "Inventory".to_owned(),
                })
        })),
        SourceFormat::Markdown
        | SourceFormat::Text
        | SourceFormat::Canvas
        | SourceFormat::Docx
        | SourceFormat::Excalidraw
        | SourceFormat::Html => {
            assert!(hits.iter().all(|hit| hit.locator.is_none()));
        }
    }
}

#[cfg(feature = "native")]
fn hit_identity(hit: &SearchHit) -> HitIdentity {
    (
        hit.chunk_id.clone(),
        hit.path.clone(),
        hit.heading_path.clone(),
        hit.format,
        hit.coverage,
        hit.locator.clone(),
    )
}
