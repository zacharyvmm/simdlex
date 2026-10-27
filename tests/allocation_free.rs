use simdlex::tablegen::{ByteSet, generate, unwrap};
use simdlex::{IndexSpec, Indexer, NibbleTables, StructuralIndex};

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

struct Spec;

impl IndexSpec for Spec {
    type State = ();
    type Error = ();
    const TABLES: NibbleTables = unwrap(generate(&[
        ByteSet::from_bytes(b" "),
        ByteSet::from_bytes(b"<>"),
    ]));
    const MASK_CLASSES: usize = 1;
}

#[test]
fn reserved_windows_allocate_nothing() {
    let input = b"<a> <b> ".repeat(500);
    let indexer = Indexer::<Spec>::new();
    let mut index = StructuralIndex::new();
    Indexer::<Spec>::reserve(&mut index, 1_024).unwrap();

    // The counter must see a known allocation, or this test proves nothing.
    set_counting(true);
    std::hint::black_box(Vec::<u8>::with_capacity(8));
    set_counting(false);
    assert!(ALLOCATIONS.load(Ordering::Relaxed) > 0);

    ALLOCATIONS.store(0, Ordering::Relaxed);
    set_counting(true);
    for start in (0..input.len()).step_by(1_024) {
        let end = (start + 1_024).min(input.len());
        indexer
            .index_window(&input, start..end, &mut (), &mut index)
            .unwrap();
    }
    set_counting(false);

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
