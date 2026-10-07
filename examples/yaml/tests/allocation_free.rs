use simdlex_yaml::{YamlConfig, YamlEvent, YamlParser, decode_yaml_scalar};

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
    ENABLED.store(true, Ordering::SeqCst);
    std::hint::black_box(Vec::<u8>::with_capacity(8));
    ENABLED.store(false, Ordering::SeqCst);
    assert!(ALLOCATIONS.load(Ordering::Relaxed) > 0);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::SeqCst);
    parse(&mut parser, &mut scratch);
    ENABLED.store(false, Ordering::SeqCst);

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
