use simdlex_html::HtmlParser;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts allocations made by the measuring thread only, so the test
/// harness's own threads cannot make the test flaky.
struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

fn set_counting(enabled: bool) {
    COUNTING.with(|counting| counting.set(enabled));
}

fn counting() -> bool {
    COUNTING.try_with(Cell::get).unwrap_or(false)
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if counting() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if counting() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn bounded_warm_parser_allocates_nothing() {
    let html = br#"<div class="px-4 py-2"><span className='font-bold'></span></div>"#;
    let mut parser = HtmlParser::with_capacity(256).unwrap();
    let warm = parser.events(html).count();

    // The counter must see a known allocation, or this test proves nothing.
    set_counting(true);
    std::hint::black_box(Vec::<u8>::with_capacity(8));
    set_counting(false);
    assert!(ALLOCATIONS.load(Ordering::Relaxed) > 0);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    set_counting(true);
    let count = parser.events(html).count();
    set_counting(false);

    assert_eq!(count, warm);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
