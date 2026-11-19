//! The design document's §5.8 is generated from the model and must not drift.

use kwiry_lifecycle::table::{BEGIN, END, render};

const DOC: &str = include_str!("../../../../docs/roadmap/ig2a-governed-semantic-design.md");
const REGENERATE: &str = "regenerate §5.8: cargo run -q -p kwiry-lifecycle --example lifecycle_table, \
     then replace the block between the GENERATED markers in docs/roadmap/ig2a-governed-semantic-design.md";

#[test]
fn design_doc_section_matches_model() {
    // Windows checkouts may convert line endings.
    let doc = DOC.replace("\r\n", "\n");
    let start = doc
        .find(BEGIN)
        .expect("the design doc has the BEGIN marker");
    let end = doc.find(END).expect("the design doc has the END marker") + END.len();
    let block = &doc[start..end];
    let generated = render();
    if block != generated {
        let first = block
            .lines()
            .zip(generated.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| block.lines().count().min(generated.lines().count()));
        panic!(
            "§5.8 differs from the model at line {} of the block\n  doc:   {:?}\n  model: {:?}\n{REGENERATE}",
            first + 1,
            block.lines().nth(first),
            generated.lines().nth(first)
        );
    }
}

#[test]
fn every_row_id_appears_once() {
    let mut seen = std::collections::HashSet::new();
    for m in kwiry_lifecycle::rows::MACHINES {
        for r in m.rows {
            assert!(seen.insert(r.id), "row {} appears twice", r.id.name());
        }
    }
    for id in kwiry_lifecycle::row_id::RowId::ALL {
        assert!(seen.contains(id), "row id {} has no table row", id.name());
    }
}
