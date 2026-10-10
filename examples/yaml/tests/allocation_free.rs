use simdlex_yaml::{YamlConfig, YamlEvent, YamlParser, decode_yaml_scalar};

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
fn bounded_warm_parser_and_decoder_allocate_nothing() {
    let yaml = b"a: [1, 'two']\nb:\n  - \"x\\ty\"\n  - |\n    block\nc: folded\n  plain\n";
    let mut parser = YamlParser::with_capacity(YamlConfig::default(), 256).unwrap();
    let mut scratch = String::with_capacity(64);
    let parse = |parser: &mut YamlParser, scratch: &mut String| {
        parser
            .parse_with(yaml, |event| {
                if let YamlEvent::Scalar(scalar) = event {
                    decode_yaml_scalar(&scalar, scratch).unwrap();
                }
            })
            .unwrap();
    };
    parse(&mut parser, &mut scratch);

    // The counter must see a known allocation, or this test proves nothing.
    set_counting(true);
    std::hint::black_box(Vec::<u8>::with_capacity(8));
    set_counting(false);
    assert!(ALLOCATIONS.load(Ordering::Relaxed) > 0);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    set_counting(true);
    parse(&mut parser, &mut scratch);
    set_counting(false);

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
