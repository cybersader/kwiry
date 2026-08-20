// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::extract::{ContentRole, ExtractionCoverage};

use super::limits::{Budget, HtmlLimits};
use super::tokenizer::Tokenizer;
use super::*;

fn complete(source: &str) -> crate::extract::ExtractedSource {
    let extracted = extract(source.as_bytes()).expect("HTML extraction succeeds");
    assert_eq!(extracted.coverage, ExtractionCoverage::IndexedComplete);
    extracted
}

fn texts(source: &str) -> Vec<(ContentRole, Vec<String>, String)> {
    complete(source)
        .sections
        .into_iter()
        .map(|section| (section.role, section.heading_path, section.content))
        .collect()
}

fn assert_limit(source: &[u8], limits: &HtmlLimits) {
    let error = extract_with_limits(source, limits).expect_err("mandatory budget must fail");
    assert_eq!(error.code, "index_limit_exceeded");
    assert_eq!(error.message, "HTML extraction exceeded a mandatory budget");
}

#[test]
fn title_is_canonical_once_and_description_is_a_latent_prelude() {
    let extracted = complete(
        "<!doctype html><html><head><meta name='description' content='  Hidden &amp; useful  '>\
         <title>  Canonical &copy; title </title><title>Ignored</title></head>\
         <body><h1>Reader heading</h1><p>Reader body</p></body></html>",
    );

    assert_eq!(
        extracted.frontmatter.title.as_deref(),
        Some("Canonical © title")
    );
    assert!(extracted.properties.is_empty());
    assert!(extracted.aliases.is_empty());
    assert!(extracted.links_out.is_empty());
    assert_eq!(
        extracted
            .sections
            .iter()
            .filter(|section| section.content.contains("Canonical"))
            .count(),
        0
    );
    assert_eq!(extracted.sections[0].role, ContentRole::Latent);
    assert_eq!(extracted.sections[0].content, "Hidden & useful");
    assert!(extracted.sections[0].heading_path.is_empty());
    assert_eq!(extracted.sections[1].heading_path, ["Reader heading"]);
    assert_eq!(extracted.sections[2].heading_path, ["Reader heading"]);
}

#[test]
fn title_uses_rcdata_and_retains_tag_like_text_literally() {
    let extracted = complete("<title>One <b>literal</b> &amp; Two</title><p>Body</p>");
    assert_eq!(
        extracted.frontmatter.title.as_deref(),
        Some("One <b>literal</b> & Two")
    );
    assert_eq!(extracted.sections[0].content, "Body");
}

#[test]
fn title_capture_accepts_a_token_larger_than_the_reservation_slice() {
    let title = "x".repeat(9_000);
    let source = format!("<title>{title}</title><p>Body</p>");
    let extracted = complete(&source);
    assert_eq!(extracted.frontmatter.title.as_deref(), Some(title.as_str()));
}

#[test]
fn title_only_document_remains_indexed_without_body_duplication() {
    let extracted = complete("<title>Metadata only</title>");
    assert_eq!(
        extracted.frontmatter.title.as_deref(),
        Some("Metadata only")
    );
    assert!(extracted.sections.is_empty());
}

#[test]
fn literal_heading_stack_replaces_levels_and_ignores_role_headings() {
    let sections = texts(
        "<h1>One</h1><p>A</p><h3>Three</h3><p>B</p>\
         <h2 role='heading'>Two</h2><p>C</p><div role='heading'>Not a heading</div><p>D</p>",
    );
    assert_eq!(sections[0].1, ["One"]);
    assert_eq!(sections[1].1, ["One"]);
    assert_eq!(sections[2].1, ["One", "Three"]);
    assert_eq!(sections[3].1, ["One", "Three"]);
    assert_eq!(sections[4].1, ["One", "Two"]);
    assert_eq!(sections[5].1, ["One", "Two"]);
    assert_eq!(sections[6].1, ["One", "Two"]);
    assert_eq!(sections[7].1, ["One", "Two"]);
}

#[test]
fn latent_subtrees_and_attributes_never_share_primary_sections_or_boost_paths() {
    let sections = texts(
        "<h1>Main</h1><p>primary <span aria-label='label CVE-2026-9999'>body</span></p>\
         <nav><h2>Local</h2><p>chrome</p></nav><p>after</p>\
         <div hidden><h3>Hidden</h3><p>secret</p></div><p>last</p>",
    );
    assert!(
        sections.iter().any(|(role, path, text)| {
            *role == ContentRole::Latent && path == &["Main"] && text == "label CVE-2026-9999"
        }),
        "{sections:?}"
    );
    assert!(sections.iter().any(|(role, path, text)| {
        *role == ContentRole::Latent && path == &["Main", "Local"] && text == "chrome"
    }));
    assert!(sections.iter().any(|(role, path, text)| {
        *role == ContentRole::Primary && path == &["Main"] && text == "after"
    }));
    assert!(sections.iter().any(|(role, path, text)| {
        *role == ContentRole::Primary && path == &["Main"] && text == "last"
    }));
    assert!(!sections.iter().any(|(role, _, text)| {
        *role == ContentRole::Primary
            && (text.contains("label") || text.contains("chrome") || text.contains("secret"))
    }));
}

#[test]
fn omission_corpus_never_contributes_machine_values() {
    let extracted = complete(
        "<script>script-secret</script><style>.x{content:'style-secret'}</style>\
         <template>template-secret</template><form><input value='input-secret'>form-secret</form>\
         <svg><text>svg-secret</text></svg><math><mi>math-secret</mi></math>\
         <iframe srcdoc='frame-secret'>iframe-secret</iframe><object data='object-secret'>object-secret</object>\
         <meta name='keywords' content='keyword-secret'><meta http-equiv='refresh' content='refresh-secret'>\
         <!-- comment-secret --><a href='javascript:active-secret' onclick='handler-secret()'>link text</a>\
         <img src='payload-secret' alt='replacement text'><div>reader text</div>",
    );
    let joined = extracted
        .sections
        .iter()
        .map(|section| section.content.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for omitted in [
        "script-secret",
        "style-secret",
        "template-secret",
        "input-secret",
        "svg-secret",
        "math-secret",
        "frame-secret",
        "object-secret",
        "keyword-secret",
        "refresh-secret",
        "comment-secret",
        "active-secret",
        "handler-secret",
        "payload-secret",
    ] {
        assert!(
            !joined.contains(omitted),
            "omitted value escaped: {omitted}"
        );
    }
    assert!(joined.contains("form-secret"));
    assert!(joined.contains("link text"));
    assert!(joined.contains("replacement text"));
    assert!(joined.contains("reader text"));
    assert!(extracted.links_out.is_empty());
    assert!(
        extracted
            .sections
            .iter()
            .all(|section| section.locator.is_none())
    );
}

#[test]
fn named_numeric_and_bom_decoding_are_deterministic_and_encoding_hints_are_ignored() {
    let extracted = complete(
        "\u{FEFF}<?xml version='1.0' encoding='windows-1252'?><meta charset='shift_jis'>\
         <p>&amp; &copy; &NotEqualTilde; &#169; &#x1F9ED;</p>",
    );
    assert_eq!(extracted.sections[0].content, "& © ≂̸ © 🧭");
}

#[test]
fn invalid_utf8_is_unreadable_with_fixed_diagnostics_and_no_prefix() {
    let extracted = extract(&[b'<', b'p', b'>', b'x', 0xff]).unwrap();
    assert_eq!(extracted.coverage, ExtractionCoverage::Unreadable);
    assert!(extracted.sections.is_empty());
    assert!(extracted.frontmatter.title.is_none());
    assert_eq!(extracted.notices.len(), 1);
    assert_eq!(extracted.notices[0].code, "html_decode_unreadable");
    assert_eq!(
        extracted.notices[0].message,
        "HTML source is not valid UTF-8"
    );
}

#[test]
fn malformed_formatting_and_table_content_have_stable_recovered_order() {
    let source = "<p><b><i>one</b>two</i> three</p>\
                  <table>before<tr><td>cell</td></tr>after</table><p>end";
    let first = complete(source);
    let second = complete(source);
    assert_eq!(first.sections, second.sections);
    let joined = first
        .sections
        .iter()
        .map(|section| section.content.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(joined.contains("one two three"), "{joined}");
    let before = joined.find("before").unwrap();
    let after = joined.find("after").unwrap();
    let cell = joined.find("cell").unwrap();
    assert!(before < after && after < cell, "{joined}");
}

#[test]
fn head_recovery_reprocesses_body_tokens_and_ignores_stray_body_head_tags() {
    let implicit = texts("<head><title>T</title><p>visible body</p>");
    assert!(implicit.iter().any(|(_, _, text)| text == "visible body"));

    let stray =
        texts("<body><p>Before</p><head><p>Should survive recovery</p></head><p>After</p></body>");
    for expected in ["Before", "Should survive recovery", "After"] {
        assert!(
            stray.iter().any(|(_, _, text)| text == expected),
            "{stray:?}"
        );
    }
}

#[test]
fn head_noscript_descendants_are_retained_as_latent_text() {
    let sections = texts("<head><noscript><p>Fallback text</p></noscript></head><body>Body</body>");
    assert!(
        sections
            .iter()
            .any(|(role, _, text)| { *role == ContentRole::Latent && text == "Fallback text" })
    );
    assert!(
        sections
            .iter()
            .any(|(role, _, text)| { *role == ContentRole::Primary && text == "Body" })
    );
}

#[test]
fn block_and_list_recovery_never_duplicates_nested_text() {
    let extracted = complete("<p>first<div>nested</div>tail<ul><li>one<li>two</ul>");
    let joined = extracted
        .sections
        .iter()
        .map(|section| section.content.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for needle in ["first", "nested", "tail", "one", "two"] {
        assert_eq!(joined.matches(needle).count(), 1, "{joined}");
    }
}

#[test]
fn malformed_heading_start_closes_the_nearest_open_heading() {
    let sections = texts("<h1>One<h2>Two</h2><p>Body</p>");
    assert_eq!(
        sections,
        vec![
            (
                ContentRole::Primary,
                vec!["One".to_owned()],
                "One".to_owned()
            ),
            (
                ContentRole::Primary,
                vec!["One".to_owned(), "Two".to_owned()],
                "Two".to_owned(),
            ),
            (
                ContentRole::Primary,
                vec!["One".to_owned(), "Two".to_owned()],
                "Body".to_owned(),
            ),
        ]
    );
}

#[test]
fn inline_display_uses_the_last_syntactically_valid_declaration() {
    let sections = texts(
        "<div style='display:none;display:potato'>invalid later</div>\
         <div style='display:none;display:inline flex'>valid later</div>\
         <div style='display:none;display: ;broken'>empty later</div>\
         <div style='display:block;display:potato;display:'>visible remains</div>",
    );
    let role_for = |needle: &str| {
        sections
            .iter()
            .find(|(_, _, text)| text == needle)
            .map(|(role, _, _)| *role)
            .expect("display fixture text")
    };
    assert_eq!(role_for("invalid later"), ContentRole::Latent);
    assert_eq!(role_for("valid later"), ContentRole::Primary);
    assert_eq!(role_for("empty later"), ContentRole::Latent);
    assert_eq!(role_for("visible remains"), ContentRole::Primary);
}

#[test]
fn inline_display_parser_honors_css_comments() {
    let sections = texts("<div style='display/**/: none'>Hidden by inline CSS</div><p>Visible</p>");
    assert!(
        sections.iter().any(|(role, _, text)| {
            *role == ContentRole::Latent && text == "Hidden by inline CSS"
        })
    );
    assert!(
        sections
            .iter()
            .any(|(role, _, text)| { *role == ContentRole::Primary && text == "Visible" })
    );
}

#[test]
fn aria_role_resolution_uses_the_first_supported_landmark_token() {
    let sections = texts("<div role='unknown navigation'><p>Chrome</p></div><p>Body</p>");
    assert!(
        sections
            .iter()
            .any(|(role, _, text)| { *role == ContentRole::Latent && text == "Chrome" })
    );
    assert!(
        sections
            .iter()
            .any(|(role, _, text)| { *role == ContentRole::Primary && text == "Body" })
    );

    let first_recognized = texts("<div role='main navigation'>Primary</div>");
    assert!(
        first_recognized
            .iter()
            .any(|(role, _, text)| { *role == ContentRole::Primary && text == "Primary" })
    );
}

#[test]
fn synthetic_attribute_text_never_enters_literal_heading_paths() {
    let sections = texts("<nav><h2 aria-label='Injected'>Visible</h2><p>body</p></nav>");
    assert!(
        sections.iter().any(|(role, path, text)| {
            *role == ContentRole::Latent && path == &["Visible"] && text == "body"
        }),
        "{sections:?}"
    );
    assert!(
        !sections
            .iter()
            .any(|(_, path, _)| { path.iter().any(|heading| heading.contains("Injected")) }),
        "{sections:?}"
    );
}

#[test]
fn admitted_block_elements_preserve_section_boundaries() {
    let sections = texts("<fieldset>First</fieldset><fieldset>Second</fieldset>");
    assert_eq!(
        sections,
        vec![
            (ContentRole::Primary, Vec::new(), "First".to_owned()),
            (ContentRole::Primary, Vec::new(), "Second".to_owned()),
        ]
    );
}

#[test]
fn mandatory_budget_breach_is_transactional_quarantine() {
    let limits = HtmlLimits {
        nodes: 3,
        ..HtmlLimits::default()
    };
    let extracted = extract_with_limits(
        b"<title>early-title</title><p>early-body</p><div>breach</div>",
        &limits,
    )
    .expect_err("mandatory limits surface as quarantine errors");
    assert_eq!(extracted.code, "index_limit_exceeded");
    assert_eq!(
        extracted.message,
        "HTML extraction exceeded a mandatory budget"
    );
}

#[test]
fn source_and_tokenizer_budgets_charge_n_success_n_plus_one_failure() {
    let limits = HtmlLimits {
        source_bytes: 8,
        decoded_bytes: 8,
        ..HtmlLimits::default()
    };
    assert!(extract_with_limits(b"<p>x</p>", &limits).is_ok());
    assert_limit(b"<p>xx</p>", &limits);

    let limits = HtmlLimits {
        tokens: 3,
        ..HtmlLimits::default()
    };
    assert!(extract_with_limits(b"<p>x", &limits).is_ok());
    assert_limit(b"<p>x</p>", &limits);
}

#[test]
fn exhausted_token_and_step_budgets_gate_before_cursor_advancement() {
    let token_limits = HtmlLimits {
        tokens: 0,
        ..HtmlLimits::default()
    };
    let mut tokenizer = Tokenizer::new("<p>body</p>");
    let mut budget = Budget::default();
    assert!(tokenizer.next_token(&mut budget, &token_limits).is_err());
    assert_eq!(tokenizer.position(), 0);

    let step_limits = HtmlLimits {
        tokenizer_steps: 0,
        ..HtmlLimits::default()
    };
    let mut tokenizer = Tokenizer::new("body");
    let mut budget = Budget::default();
    assert!(tokenizer.next_token(&mut budget, &step_limits).is_err());
    assert_eq!(tokenizer.position(), 0);
}

#[test]
fn html_heading_components_match_the_host_persistence_bound() {
    let admitted = format!("<h1>{}</h1><p>body</p>", "x".repeat(1_024));
    assert!(extract_with_limits(admitted.as_bytes(), &HtmlLimits::default()).is_ok());

    let breached = format!("<h1>{}</h1><p>body</p>", "x".repeat(1_025));
    assert_limit(breached.as_bytes(), &HtmlLimits::default());
}

#[test]
fn projector_sibling_scratch_charges_before_frame_growth() {
    let limits = HtmlLimits {
        project_traversal_entries: 4,
        ..HtmlLimits::default()
    };
    assert!(extract_with_limits(b"<br><br><br><br>", &limits).is_ok());
    assert_limit(b"<br><br><br><br><br>", &limits);
}

#[test]
fn heading_run_scratch_charges_n_success_n_plus_one_failure() {
    fn heading_with_runs(runs: usize) -> String {
        let mut source = String::from("<h1>");
        for index in 0..runs {
            if index % 2 == 0 {
                source.push_str("<span>x</span>");
            } else {
                source.push_str("<span hidden>x</span>");
            }
        }
        source.push_str("</h1>");
        source
    }

    let limits = HtmlLimits {
        heading_run_entries: 4,
        ..HtmlLimits::default()
    };
    assert!(extract_with_limits(heading_with_runs(4).as_bytes(), &limits).is_ok());
    assert_limit(heading_with_runs(5).as_bytes(), &limits);
}

#[test]
fn recovery_adoption_scratch_charges_n_success_n_plus_one_failure() {
    let limits = HtmlLimits {
        recovery_scratch_entries: 3,
        ..HtmlLimits::default()
    };
    assert!(extract_with_limits(b"<b><i><em><strong>x</b>", &limits).is_ok());
    assert_limit(b"<b><i><em><strong><u>x</b>", &limits);
}

#[test]
fn parser_stack_boundary_uses_no_removal_snapshot() {
    fn nested_divs(depth: usize) -> String {
        let mut source = "<div>".repeat(depth);
        source.push('x');
        source
    }

    let limits = HtmlLimits {
        open_elements: 5,
        ..HtmlLimits::default()
    };
    assert!(extract_with_limits(nested_divs(4).as_bytes(), &limits).is_ok());
    assert_limit(nested_divs(5).as_bytes(), &limits);
}

#[test]
fn normalized_heading_scratch_charges_n_success_n_plus_one_failure() {
    let limits = HtmlLimits {
        heading_scratch_bytes: 4,
        ..HtmlLimits::default()
    };
    assert!(extract_with_limits(b"<title>xxxx</title>", &limits).is_ok());
    assert_limit(b"<title>xxxxx</title>", &limits);
}
