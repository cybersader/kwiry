// SPDX-License-Identifier: MIT OR Apache-2.0

//! Segmentation: positioned runs become the text of one page.
//!
//! The reader below this module produces runs and refuses to guess what they
//! mean. This module makes the guesses, and every one of them is a threshold
//! written down here with the reason it has the value it has.
//!
//! # What a PDF does not contain
//!
//! A PDF has no words, no lines, no paragraphs, and no reading order. It has
//! glyphs at coordinates. Every separator this module emits is inferred from
//! geometry, so the two failure modes are not symmetric:
//!
//! * A **fusion** (`nameqty`) destroys two tokens and creates a third that
//!   nobody wrote. No query for `name` or `qty` will ever match it.
//! * A **spurious split** (`na me`) costs one phrase match and leaves both
//!   halves of the evidence in the index.
//!
//! So every ambiguous boundary resolves toward separation. That is not a
//! preference, it is the asymmetry of the index: a search index can recover
//! from a split it should not have made and cannot recover from a join.
//!
//! # Reading order
//!
//! Sorting runs by descending `y` and concatenating each baseline is the
//! recorded wrong answer for multi-column text: the two columns of a page share
//! their baselines by construction, so a `y` sort splices the left column's line
//! to the right column's line beside it and emits sentences that were never
//! written. No tolerance tuning rescues it, because the interleave is exact.
//!
//! The order here is a recursive XY-cut with **vertical precedence**: a column
//! corridor (a vertical band no glyph crosses) is cut before any horizontal
//! band gap. The precedence matters because a blank line between paragraphs
//! produces a horizontal gap statistically indistinguishable from a gutter;
//! cutting horizontally first interleaves the columns at paragraph granularity
//! instead of at line granularity, which is the same defect wearing a disguise.
//! Requiring a corridor to be crossed by no run in the whole region is what
//! makes vertical precedence safe.
//!
//! # Why a table needs a guard
//!
//! A positioned table's column gaps are corridors too, and reading a table
//! column-major destroys every row. So a region whose corridors look like a
//! table — three or more children, rows that span them, and corridors wide
//! relative to the content they separate — is emitted row-major with tab
//! separators instead of being recursed into.

use std::collections::BTreeSet;

use super::{PdfPageGeometry, PdfWritingMode};

/// `Tr 7` adds to the clipping path and paints nothing. Mode 3 (invisible) is
/// deliberately *not* filtered: OCR-under-image text is the searchable content
/// of a scanned page.
const CLIP_ONLY_RENDER_MODE: i64 = 7;

/// tan(1°). Beyond this the run is not axis-aligned, and placing it in a
/// left-to-right reading order would be inventing an order it does not have.
const AXIS_TANGENT: f64 = 0.017_5;

/// Glyph-space width of `U+0020` used when the font ships no metric for it,
/// per mille. The median of the standard-14 space widths.
const SPACE_CLASS_WIDTH: f64 = 260.0;

/// Floor on the inferred space advance, in points. Without it a `Tf 0` run
/// collapses every threshold to zero and every boundary becomes a separation.
const MIN_SPACE_ADVANCE: f64 = 0.1;

/// A gap of at least this many space-advances separates two runs. Latin space
/// widths are 250–330/1000 em; justification compresses a space to about
/// 0.7 space-advances at worst, and tracking or kerning at an explicit
/// reposition stays under 0.15. 0.45 sits about three times above the kerning
/// ceiling and about a third below the compressed-space floor.
const GAP_SEPARATES: f64 = 0.45;

/// Below this many space-advances the join is confident even when the widths
/// that produced the gap were estimated rather than measured.
const GAP_JOINS: f64 = 0.10;

/// Relative error budget charged against an advance that came from estimated
/// widths rather than from `/Widths` or `/W`. The estimate table below is
/// within about 9% on Latin text; 25% is roughly three times that.
const WIDTH_UNCERTAINTY: f64 = 0.25;

/// Cap on that error budget, in space-advances. Without it a full-width run of
/// estimated glyphs would make every gap on the line ambiguous.
const UNCERTAINTY_CAP: f64 = 4.0;

/// Baseline jitter admitted within one line, as a fraction of the line's
/// dominant font size. Far below any real leading, which is at least 1.15.
const LINE_TOLERANCE: f64 = 0.30;

/// A run at most this fraction of the line's size is a candidate superscript or
/// subscript rather than a new line.
const SCRIPT_SIZE_RATIO: f64 = 0.75;

/// Baseline shift admitted for such a run, as a fraction of the line's size.
const SCRIPT_TOLERANCE: f64 = 0.60;

/// Baseline delta, relative to the block's median leading, that starts a new
/// paragraph. Leading is constant inside a paragraph; inter-paragraph space is
/// at least half an em on top of it.
const PARAGRAPH_LEADING: f64 = 1.35;

/// Dominant font-size ratio between adjacent lines that starts a new paragraph.
/// Catches a title followed by a byline, where the leading alone does not move
/// enough.
const PARAGRAPH_SIZE_RATIO: f64 = 1.20;

/// Baseline delta, relative to the region's median leading, that splits a
/// region horizontally.
const BAND_CUT_LEADING: f64 = 1.6;

/// A corridor must be at least this many median space-advances wide …
const CORRIDOR_SPACE_FACTOR: f64 = 1.5;

/// … and at least this fraction of the median font size, whichever is larger.
/// The width floor alone does not discriminate a gutter from a stretched
/// justified word space; being crossed by no run in the region does.
const CORRIDOR_SIZE_FACTOR: f64 = 0.75;

/// Corridors narrower than this are noise regardless of the computed floor.
const MIN_CORRIDOR_WIDTH: f64 = 1.0;

/// A region with fewer baseline bands than this is not cut into columns. Two
/// short headings side by side are not a two-column layout.
const MIN_CORRIDOR_BANDS: usize = 4;

/// Corridors a region may have before it stops being treated as a column
/// layout at all. A page with hundreds of full-height holes is scattered marks,
/// not columns, and the cap is also what keeps the child lookup — which is
/// linear in the corridor count and runs once per run — from becoming
/// quadratic on a page holding the run budget.
const MAX_COLUMN_CHILDREN: usize = 64;

/// Children a set of corridors can have and still be read column-major without
/// a table test. Two children are *usually* two-column prose, which the table
/// guard must not steal — but they are also every label/value table, glossary,
/// table of contents and invoice, which is the dominant real two-column table
/// shape. So two children are admitted as a table under
/// [`TABLE_PAIR_CORRIDOR_RATIO`] instead of being excluded outright.
const TABLE_MIN_COLUMNS: usize = 2;

/// Fraction of the region's bands that must span two or more children.
const TABLE_ROW_COINCIDENCE: f64 = 0.60;

/// Median corridor width relative to median child content width. Prose columns
/// are wide with a thin gutter; table columns are narrow with a wide gap. This
/// is what separates a three-column newspaper from a three-column table.
const TABLE_CORRIDOR_RATIO: f64 = 0.5;

/// The same ratio for a two-child region, where the cost of being wrong is
/// higher: mistaking two-column prose for a table would tab-join two unrelated
/// sentences on every line.
///
/// A prose gutter is a fraction of its columns — 20pt between 220pt columns is
/// 0.09 — while a label/value table's gap is comparable to or wider than its
/// cells: 168pt between an 80pt label column and a 50pt value column is 2.6.
/// Requiring the corridor to be at least as wide as the median column sits an
/// order of magnitude clear of prose on one side and well clear of the observed
/// table shape on the other.
const TABLE_PAIR_CORRIDOR_RATIO: f64 = 1.0;

/// Recursion bound on the XY-cut.
const MAX_CUT_DEPTH: usize = 6;

/// One page's composed text.
pub(super) struct PageText {
    pub(super) text: String,
    /// Runs that could not be placed in a left-to-right reading order —
    /// rotated, skewed, or vertical. They are appended after the ordered text
    /// rather than dropped (dropping loses content) and rather than interleaved
    /// (interleaving invents an order).
    pub(super) unordered: usize,
}

/// A run reduced to what segmentation needs: an interval on the baseline, a
/// size, the text, and how much of the interval was measured rather than
/// estimated.
struct Item {
    x0: f64,
    x1: f64,
    y: f64,
    size: f64,
    /// Inferred advance of `U+0020` at this run's size.
    space: f64,
    /// Absolute uncertainty in `x1`, in points. Zero when every glyph width in
    /// the run came from the font.
    slack: f64,
    text: String,
    /// `Ts` was non-zero: the producer said this is a raised or lowered run.
    raised: bool,
}

struct Line {
    members: Vec<usize>,
    /// Baseline of the largest run on the line.
    y: f64,
    /// Size of the largest run on the line.
    size: f64,
    x0: f64,
    x1: f64,
}

/// Corridor-separated children of one region, plus the corridors themselves.
struct Columns {
    children: Vec<Vec<usize>>,
    corridors: Vec<(f64, f64)>,
}

impl Columns {
    /// Which child an `x` belongs to. No run crosses a corridor, so testing the
    /// run's left edge against each corridor's right edge is exact.
    fn child_of(&self, x0: f64) -> usize {
        self.corridors
            .iter()
            .filter(|(_, end)| x0 >= *end)
            .count()
            .min(self.children.len().saturating_sub(1))
    }
}

pub(super) fn compose_page(page: &PdfPageGeometry) -> PageText {
    let (items, unordered) = collect(page);

    let mut blocks: Vec<String> = Vec::new();
    if !items.is_empty() {
        let region: Vec<usize> = (0..items.len()).collect();
        cut(&items, &region, 0, &mut blocks);
    }
    if !unordered.is_empty() {
        blocks.push(unordered.join("\n"));
    }

    PageText {
        text: normalize(&blocks.join("\n\n")),
        unordered: unordered.len(),
    }
}

fn collect(page: &PdfPageGeometry) -> (Vec<Item>, Vec<String>) {
    let mut items = Vec::new();
    let mut unordered = Vec::new();

    for run in &page.runs {
        if run.text.is_empty() || run.render_mode == CLIP_ONLY_RENDER_MODE {
            continue;
        }
        let finite = run
            .origin
            .iter()
            .chain(run.end.iter())
            .all(|v| v.is_finite())
            && run.font_size.is_finite();
        if !finite {
            continue;
        }

        // Orientation comes from the rendering matrix, never from
        // `end - origin`. The displacement is only a direction when the widths
        // behind it were measured: a base-14 font with no `/Widths` reports a
        // zero width per glyph, so its advance is the accumulated `Tc`/`Tw`
        // alone, and an ordinary negative letterspacing (`-0.35 Tc`) then makes
        // upright prose look like it advances leftward. Reading that as
        // rotation appended a whole line after the page's ordered text and
        // downgraded the page to `IndexedPartial`.
        let direction = run.advance_direction;
        let degenerate = direction[0] == 0.0 && direction[1] == 0.0;
        let upright =
            degenerate || (direction[0] > 0.0 && direction[1].abs() <= AXIS_TANGENT * direction[0]);
        if run.writing_mode == PdfWritingMode::Vertical
            || !direction.iter().all(|value| value.is_finite())
            || !upright
        {
            unordered.push(run.text.clone());
            continue;
        }

        let size = run.font_size.abs();
        // A run whose font shipped no metrics reports a zero advance rather
        // than a guess. Segmentation needs an extent, so the guess is made
        // here, where it can be labelled as one and charged to `slack`.
        //
        // The guess is taken whenever the advance was not measured, including
        // when it came out *positive*: a `+2.6 Tc` run of 67 metrics-less
        // glyphs reports 137.8 points for a line that is nearly 290 wide, and
        // trusting that number under-measures `x1`, which manufactures corridors
        // that are not there.
        let dx = run.end[0] - run.origin[0];
        let width = if run.geometry_exact {
            dx
        } else {
            estimated_width(&run.text, size)
        };
        let slack = if run.geometry_exact {
            0.0
        } else {
            WIDTH_UNCERTAINTY * width
        };

        items.push(Item {
            x0: run.origin[0],
            x1: run.origin[0] + width,
            y: run.origin[1],
            size,
            space: (SPACE_CLASS_WIDTH / 1000.0 * size).max(MIN_SPACE_ADVANCE),
            slack,
            text: run.text.clone(),
            raised: run.rise != 0.0,
        });
    }

    (items, unordered)
}

/// Width of `text` at `size`, in points, from glyph classes rather than from
/// font metrics.
///
/// The dominant real case is a base-14 Type1 font that carries no `/Widths`, no
/// `/FirstChar`, and no `/FontDescriptor`: the file simply does not contain its
/// own advances. The values are medians of the Helvetica, Times, and Courier
/// metrics. Shipping the real AFM tables is an owner decision (licensing and a
/// third-party-notice entry), not one this module may take, so until then every
/// advance derived from this table is charged [`WIDTH_UNCERTAINTY`].
fn estimated_width(text: &str, size: f64) -> f64 {
    text.chars().map(class_width).sum::<f64>() / 1000.0 * size
}

fn class_width(character: char) -> f64 {
    match character {
        ' ' | '\u{00a0}' => SPACE_CLASS_WIDTH,
        'W' | 'M' | '@' | '%' => 900.0,
        'w' | 'm' => 720.0,
        'i' | 'l' | 'j' | 'I' | 't' | 'f' | 'r' | '\'' | '`' | '.' | ',' | ';' | ':' | '!'
        | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '-' => 300.0,
        '\u{0300}'..='\u{036f}' => 300.0,
        _ if character.is_ascii_uppercase() => 720.0,
        _ if is_wide(character) => 1000.0,
        _ => 500.0,
    }
}

/// East Asian Wide and Fullwidth, approximated by block rather than by the full
/// UAX #11 table. Everything inside advances a full em; nothing outside it is
/// wide enough for the difference to change a separation decision.
fn is_wide(character: char) -> bool {
    matches!(character,
        '\u{1100}'..='\u{115f}'
        | '\u{2e80}'..='\u{303e}'
        | '\u{3041}'..='\u{33ff}'
        | '\u{3400}'..='\u{4dbf}'
        | '\u{4e00}'..='\u{9fff}'
        | '\u{a000}'..='\u{a4cf}'
        | '\u{ac00}'..='\u{d7a3}'
        | '\u{f900}'..='\u{faff}'
        | '\u{fe30}'..='\u{fe4f}'
        | '\u{ff00}'..='\u{ff60}'
        | '\u{ffe0}'..='\u{ffe6}'
        | '\u{20000}'..='\u{2fffd}'
        | '\u{30000}'..='\u{3fffd}')
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

fn assemble_lines(items: &[Item], region: &[usize]) -> Vec<Line> {
    let mut ordered = region.to_vec();
    ordered.sort_by(|left, right| {
        items[*left]
            .y
            .total_cmp(&items[*right].y)
            .then(items[*left].x0.total_cmp(&items[*right].x0))
    });

    let mut lines: Vec<Line> = Vec::new();
    for index in ordered {
        let item = &items[index];
        match lines.last_mut() {
            Some(line) if shares_baseline(line, item) => {
                line.members.push(index);
                if item.size > line.size {
                    line.size = item.size;
                    line.y = item.y;
                }
                line.x0 = line.x0.min(item.x0);
                line.x1 = line.x1.max(item.x1);
            }
            _ => lines.push(Line {
                members: vec![index],
                y: item.y,
                size: item.size,
                x0: item.x0,
                x1: item.x1,
            }),
        }
    }

    for line in &mut lines {
        line.members
            .sort_by(|left, right| items[*left].x0.total_cmp(&items[*right].x0));
    }
    lines
}

/// A run joins the open line when its baseline is within the jitter tolerance,
/// or when it is small enough and close enough to be a superscript or subscript
/// of it. The second clause is not optional: a footnote marker drawn at 6.5pt
/// and raised 4pt sits on no baseline of its own, and starting a new line for it
/// puts the marker on a line by itself in the middle of a sentence.
fn shares_baseline(line: &Line, item: &Item) -> bool {
    let scale = line.size.max(item.size);
    let delta = (item.y - line.y).abs();
    if delta <= LINE_TOLERANCE * scale {
        return true;
    }
    let smaller = line.size.min(item.size);
    smaller <= SCRIPT_SIZE_RATIO * scale && delta <= SCRIPT_TOLERANCE * scale
}

fn is_script(item: &Item, line: &Line) -> bool {
    item.raised || item.size < SCRIPT_SIZE_RATIO * line.size
}

/// The space-versus-nothing decision for one boundary between two runs.
///
/// The gap between adjacent glyphs inside a single show sequence is exactly the
/// operator-specified adjustment and carries no width error at all, because the
/// pen position that ends one glyph is the same pen position that starts the
/// next. Width error therefore only affects gaps that cross an explicit
/// reposition, which is exactly where `slack` is charged.
fn separated(previous: &Item, next: &Item, line: &Line) -> bool {
    if previous.text.ends_with(char::is_whitespace) || next.text.starts_with(char::is_whitespace) {
        // The producer drew a real space glyph. Adding another one here is how
        // "a b" becomes "a  b" and then a different token stream.
        return false;
    }
    // A superscript marker is drawn at the measured end of the word it annotates,
    // so its gap is zero and the gap test alone glues the digit onto the word.
    if is_script(previous, line) != is_script(next, line) {
        return true;
    }

    let space = previous.space.max(next.space);
    let slack = previous.slack.min(UNCERTAINTY_CAP * space);
    let gap = next.x0 - previous.x1;
    let joins_confidently = gap < (GAP_JOINS * space).max(GAP_SEPARATES * space - slack);
    !joins_confidently
}

fn render_members(items: &[Item], members: &[usize], line: &Line) -> String {
    let mut out = String::new();
    let mut previous: Option<&Item> = None;
    for index in members {
        let item = &items[*index];
        if let Some(previous) = previous
            && separated(previous, item, line)
        {
            out.push(' ');
        }
        out.push_str(&item.text);
        previous = Some(item);
    }
    out
}

// ---------------------------------------------------------------------------
// Reading order
// ---------------------------------------------------------------------------

fn cut(items: &[Item], region: &[usize], depth: usize, out: &mut Vec<String>) {
    let lines = assemble_lines(items, region);
    if lines.len() <= 1 || depth >= MAX_CUT_DEPTH {
        out.push(render_block(items, &lines));
        return;
    }

    let columns = columns(items, region, lines.len());
    let table = columns
        .as_ref()
        .is_some_and(|columns| is_table(items, &lines, columns));

    if let Some(columns) = &columns {
        // A region that answered the table tests is emitted row-major here and
        // not band-split first. Trying `horizontal_split` ahead of this cut a
        // table apart at its own header gap: the header row lost its cell
        // separators to a plain block render while the body kept its tabs, so
        // any consumer of the tab structure saw a headerless table.
        if table {
            out.push(render_table(items, &lines, columns));
        } else {
            for child in &columns.children {
                cut(items, child, depth + 1, out);
            }
        }
        return;
    }

    if let Some((upper, lower)) = horizontal_split(&lines) {
        cut(items, &upper, depth + 1, out);
        cut(items, &lower, depth + 1, out);
        return;
    }

    out.push(render_block(items, &lines));
}

/// Maximal vertical bands inside the region that no run crosses.
///
/// This is x-clustering computed as the complement of the x-density profile
/// rather than as centroid clustering on left edges: it needs no cluster count,
/// it is exact about the invariant that matters (no glyph crosses this x), and
/// it is stable under ragged right edges, which clustering on left edges is not.
/// Because the profile is taken over the whole region at once, a corridor is
/// full-height by construction — a hole in one line is not a corridor.
fn columns(items: &[Item], region: &[usize], bands: usize) -> Option<Columns> {
    if bands < MIN_CORRIDOR_BANDS || region.len() < 2 {
        return None;
    }

    let space = median(region.iter().map(|index| items[*index].space));
    let size = median(region.iter().map(|index| items[*index].size));
    let slack = median(region.iter().map(|index| items[*index].slack)).min(UNCERTAINTY_CAP * space);
    let minimum = ((CORRIDOR_SPACE_FACTOR * space).max(CORRIDOR_SIZE_FACTOR * size) + 2.0 * slack)
        .max(MIN_CORRIDOR_WIDTH);

    let mut intervals: Vec<(f64, f64)> = region
        .iter()
        .map(|index| (items[*index].x0, items[*index].x1))
        .collect();
    intervals.sort_by(|left, right| left.0.total_cmp(&right.0));

    let mut corridors = Vec::new();
    let mut reach = intervals[0].1;
    for (x0, x1) in &intervals[1..] {
        if *x0 - reach >= minimum {
            corridors.push((reach, *x0));
        }
        reach = reach.max(*x1);
    }
    if corridors.is_empty() || corridors.len() >= MAX_COLUMN_CHILDREN {
        return None;
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); corridors.len() + 1];
    for index in region {
        let child = corridors
            .iter()
            .filter(|(_, end)| items[*index].x0 >= *end)
            .count();
        children[child].push(*index);
    }
    if children.iter().any(Vec::is_empty) {
        return None;
    }

    Some(Columns {
        children,
        corridors,
    })
}

/// Three tests, all required. Each one alone admits a layout the others reject:
/// two-column prose passes row coincidence, three-column prose passes both the
/// child count and row coincidence, and only the corridor-to-content ratio
/// separates a three-column table from a three-column newspaper.
fn is_table(items: &[Item], lines: &[Line], columns: &Columns) -> bool {
    if columns.children.len() < TABLE_MIN_COLUMNS {
        return false;
    }

    let coincident = lines
        .iter()
        .filter(|line| {
            line.members
                .iter()
                .map(|index| columns.child_of(items[*index].x0))
                .collect::<BTreeSet<_>>()
                .len()
                >= 2
        })
        .count();
    if (coincident as f64) < TABLE_ROW_COINCIDENCE * lines.len() as f64 {
        return false;
    }

    let corridor = median(columns.corridors.iter().map(|(start, end)| end - start));
    let content = median(columns.children.iter().map(|child| {
        let left = child
            .iter()
            .map(|index| items[*index].x0)
            .fold(f64::INFINITY, f64::min);
        let right = child
            .iter()
            .map(|index| items[*index].x1)
            .fold(f64::NEG_INFINITY, f64::max);
        right - left
    }));
    let ratio = if columns.children.len() == 2 {
        TABLE_PAIR_CORRIDOR_RATIO
    } else {
        TABLE_CORRIDOR_RATIO
    };
    corridor >= ratio * content
}

fn horizontal_split(lines: &[Line]) -> Option<(Vec<usize>, Vec<usize>)> {
    if lines.len() < 2 {
        return None;
    }
    let deltas: Vec<f64> = lines.windows(2).map(|pair| pair[1].y - pair[0].y).collect();
    let leading = median(deltas.iter().copied());
    if leading <= 0.0 {
        return None;
    }
    let (at, widest) = deltas
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))?;
    if *widest < BAND_CUT_LEADING * leading {
        return None;
    }

    let upper = lines[..=at]
        .iter()
        .flat_map(|line| line.members.iter().copied())
        .collect();
    let lower = lines[at + 1..]
        .iter()
        .flat_map(|line| line.members.iter().copied())
        .collect();
    Some((upper, lower))
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

fn render_block(items: &[Item], lines: &[Line]) -> String {
    let leading = median(lines.windows(2).map(|pair| pair[1].y - pair[0].y));
    let left = lines
        .iter()
        .map(|line| line.x0)
        .fold(f64::INFINITY, f64::min);

    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push_str(
                if starts_paragraph(&lines[index - 1], line, leading, left) {
                    "\n\n"
                } else {
                    "\n"
                },
            );
        }
        out.push_str(&render_members(items, &line.members, line));
    }
    out
}

/// Line-level breaks are never repaired: a hyphen at a line end stays where the
/// producer put it. De-hyphenating would silently rewrite the author's text on a
/// guess about whether the hyphen is a break or a spelling.
fn starts_paragraph(previous: &Line, line: &Line, leading: f64, left: f64) -> bool {
    if leading > 0.0 && line.y - previous.y >= PARAGRAPH_LEADING * leading {
        return true;
    }
    let smaller = previous.size.min(line.size);
    if smaller > 0.0 && previous.size.max(line.size) / smaller >= PARAGRAPH_SIZE_RATIO {
        return true;
    }
    line.size > 0.0 && line.x0 - left >= line.size
}

fn render_table(items: &[Item], lines: &[Line], columns: &Columns) -> String {
    let mut rows = Vec::with_capacity(lines.len());
    for line in lines {
        let mut cells: Vec<Vec<usize>> = vec![Vec::new(); columns.children.len()];
        for index in &line.members {
            cells[columns.child_of(items[*index].x0)].push(*index);
        }
        let mut row: Vec<String> = cells
            .iter()
            .map(|members| render_members(items, members, line))
            .collect();
        while row.last().is_some_and(String::is_empty) {
            row.pop();
        }
        rows.push(row.join("\t"));
    }
    rows.join("\n")
}

/// Collapse repeated spaces, trim each line, and cap blank runs at one. Tabs
/// survive: they carry the table's cell boundaries.
fn normalize(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let mut collapsed = String::with_capacity(line.len());
        let mut spacing = false;
        for character in line.chars() {
            let character = if character == '\u{00a0}' {
                ' '
            } else {
                character
            };
            if character == ' ' {
                if !spacing {
                    collapsed.push(' ');
                }
                spacing = true;
            } else {
                spacing = false;
                collapsed.push(character);
            }
        }
        let trimmed = collapsed.trim().to_owned();
        if trimmed.is_empty() && out.last().is_some_and(String::is_empty) {
            continue;
        }
        out.push(trimmed);
    }
    while out.first().is_some_and(String::is_empty) {
        out.remove(0);
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out.join("\n")
}

fn median(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}
