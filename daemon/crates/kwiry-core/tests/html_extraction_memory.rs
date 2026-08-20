// SPDX-License-Identifier: MIT OR Apache-2.0

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use kwiry_core::{SourceDescriptor, SourceFormat, prepare_source_buffer};

const MAX_INCREMENTAL_LIVE_BYTES: usize = 48 * 1024 * 1024;

struct CountingAllocator;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            if new_size >= layout.size() {
                record_allocation(new_size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::SeqCst);
            }
        }
        replacement
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn record_allocation(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::SeqCst) + bytes;
    let mut peak = PEAK.load(Ordering::SeqCst);
    while live > peak {
        match PEAK.compare_exchange_weak(peak, live, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break,
            Err(actual) => peak = actual,
        }
    }
}

#[test]
fn maximum_scale_html_stays_below_the_native_incremental_live_heap_ceiling() {
    let payload_bytes = 9 * 1024 * 1024;
    let mut source = String::with_capacity(payload_bytes + 64);
    source.push_str("<title>Bounded memory</title><main><p>");
    while source.len() + 8 < payload_bytes {
        source.push_str("reader ");
    }
    source.push_str("</p></main>");

    let baseline = LIVE.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    let prepared = prepare_source_buffer(
        &SourceDescriptor {
            vault_id: "memory".to_owned(),
            room: None,
            path: "maximum.html".to_owned(),
            format: SourceFormat::Html,
            byte_length: source.len() as u64,
            mtime: 1,
            mtime_nanos: 1,
        },
        source.as_bytes(),
    )
    .expect("maximum-scale HTML prepares within fixed budgets");
    let incremental_peak = PEAK.load(Ordering::SeqCst).saturating_sub(baseline);

    assert!(prepared.coverage.is_indexed());
    assert!(!prepared.chunks.is_empty());
    assert!(
        incremental_peak <= MAX_INCREMENTAL_LIVE_BYTES,
        "incremental live heap {incremental_peak} exceeded {MAX_INCREMENTAL_LIVE_BYTES}"
    );
}
