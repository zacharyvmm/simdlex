use simdlex_index::{IndexSpec, Indexer, NibbleTables, StructuralIndex};
use simdlex_tablegen::{ByteSet, Tables, generate, unwrap};

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

const TABLES: Tables<2> = unwrap(generate(&[
    ByteSet::from_bytes(b" "),
    ByteSet::from_bytes(b"<>"),
]));

struct Spec;

impl IndexSpec for Spec {
    type State = ();
    type Error = ();
    const TABLES: NibbleTables = NibbleTables {
        low: TABLES.low,
        high: TABLES.high,
    };
    const CLASS_BITS: [u8; 8] = TABLES.padded_class_bits();
    const CLASS_COUNT: usize = 2;
    const MASK_CLASSES: usize = 1;
}

#[test]
fn reserved_windows_allocate_nothing() {
    let input = b"<a> <b> ".repeat(500);
    let indexer = Indexer::<Spec>::new();
    let mut index = StructuralIndex::new();
    Indexer::<Spec>::reserve(&mut index, 1_024).unwrap();

    // The counter must see a known allocation, or this test proves nothing.
    ENABLED.store(true, Ordering::SeqCst);
    std::hint::black_box(Vec::<u8>::with_capacity(8));
    ENABLED.store(false, Ordering::SeqCst);
    assert!(ALLOCATIONS.load(Ordering::Relaxed) > 0);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::SeqCst);
    for start in (0..input.len()).step_by(1_024) {
        let end = (start + 1_024).min(input.len());
        indexer
            .index_window(&input, start..end, &mut (), &mut index)
            .unwrap();
    }
    ENABLED.store(false, Ordering::SeqCst);

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
