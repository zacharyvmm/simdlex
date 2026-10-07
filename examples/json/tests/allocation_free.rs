use simdlex_json::{JsonConfig, JsonParser};

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct CountingAllocator;

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ENABLED.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn bounded_warm_parser_allocates_nothing() {
    let json = br#"{"items":[1,2,3,{"name":"value","ok":true}],"none":null}"#;
    let mut parser = JsonParser::with_capacity(JsonConfig::default(), 256).unwrap();
    parser.parse_with(json, |_| {}).unwrap();

    // The counter must see a known allocation, or this test proves nothing.
    ENABLED.store(true, Ordering::SeqCst);
    std::hint::black_box(Vec::<u8>::with_capacity(8));
    ENABLED.store(false, Ordering::SeqCst);
    assert!(ALLOCATIONS.load(Ordering::Relaxed) > 0);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::SeqCst);
    parser.parse_with(json, |_| {}).unwrap();
    ENABLED.store(false, Ordering::SeqCst);

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
