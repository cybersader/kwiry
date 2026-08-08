// SPDX-License-Identifier: MIT OR Apache-2.0

//! Allocation bound for the Excalidraw property projection.
//!
//! This lives in its own integration binary because it installs a counting
//! `#[global_allocator]`: Rust builds each `tests/*.rs` file as a separate
//! executable, so the measurement is not perturbed by other tests running in
//! parallel.
//!
//! The invariant under test is that the retained-property budget is charged
//! *while* the projection is built rather than after it is finished. A
//! post-hoc check rejects the result but still pays for it: a drawing at the
//! `MAX_FILE_BYTES` ceiling — which §2.7 itself describes as "mostly points,
//! seeds, and nonces" — could clone a fresh `BTreeMap` per element for hundreds
//! of thousands of elements before the budget it was declared to obey could say
//! no. Extraction is then dozens of times the size of the file that triggered
//! it, and the outcome is `skipped-no-extractable-text`: nothing retained,
//! everything paid for.
//!
//! The assertion is expressed against the cost of parsing the same bytes into
//! `PropertyValue`, not against an absolute byte count, so it is independent of
//! machine, allocator, and optimisation level. Parsing is the irreducible floor
//! every JSON format pays (Canvas pays exactly it, by moving its already-parsed
//! root into the bag); the extractor is allowed a modest margin above that floor
//! and nothing more.

#![cfg(feature = "internal-excalidraw-extractor")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};

use kwiry_core::{ExtractionCoverage, PropertyValue, extract_excalidraw_candidate};

/// Peak live heap bytes the extractor may hold, as a multiple of the peak the
/// parse of the same bytes holds. The measured ratio is ~1.09; charging the
/// budget only after the projection is built pushes it past 2.
const MAX_PEAK_OVER_PARSE_PEAK: f64 = 1.5;

#[test]
fn projection_allocation_is_bounded_by_the_cost_of_parsing_the_same_bytes() {
    // A structurally valid drawing that yields no text at all: every byte is
    // machine-generated element scaffolding, and every element is projected.
    let document = line_document(2 * 1024 * 1024);

    let parse_peak = measure(|| {
        let parsed = serde_json::from_str::<PropertyValue>(&document).expect("fixture parses");
        assert!(matches!(parsed, PropertyValue::Map(_)));
    });

    let mut coverage = None;
    let extract_peak = measure(|| {
        let extracted = extract_excalidraw_candidate(document.as_bytes()).expect("no limit error");
        coverage = Some(extracted.coverage);
    });

    assert_eq!(
        coverage,
        Some(ExtractionCoverage::SkippedNoExtractableText),
        "the fixture must retain nothing, so every byte it costs is waste"
    );
    assert!(parse_peak > 0 && extract_peak > 0);

    let ratio = extract_peak as f64 / parse_peak as f64;
    assert!(
        ratio <= MAX_PEAK_OVER_PARSE_PEAK,
        "extraction peaked at {extract_peak} bytes against a {parse_peak}-byte parse peak \
         ({ratio:.2}x, limit {MAX_PEAK_OVER_PARSE_PEAK:.2}x) for a {}-byte drawing: the \
         retained-property budget must be charged during projection, not after it",
        document.len()
    );
}

/// Minimal well-formed elements with no authored text, packed to `target` bytes.
fn line_document(target: usize) -> String {
    let mut document = String::from("{\"elements\":[");
    let mut index: u64 = 0;
    while document.len() < target {
        if index != 0 {
            document.push(',');
        }
        write!(document, "{{\"id\":\"{index:016x}\",\"type\":\"line\"}}").unwrap();
        index += 1;
    }
    document.push_str("]}");
    document
}

/// Runs `body` and returns the peak live heap in bytes reached during it.
fn measure(body: impl FnOnce()) -> usize {
    PEAK.store(LIVE.load(Ordering::SeqCst), Ordering::SeqCst);
    let baseline = LIVE.load(Ordering::SeqCst);
    body();
    PEAK.load(Ordering::SeqCst).saturating_sub(baseline)
}

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Counts live heap bytes and records their high-water mark.
///
/// `realloc` and `alloc_zeroed` are intentionally left to their trait defaults,
/// which route through this `alloc`/`dealloc` pair and so stay counted.
struct PeakAllocator;

unsafe impl GlobalAlloc for PeakAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: PeakAllocator = PeakAllocator;
