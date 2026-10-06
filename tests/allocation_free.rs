#![cfg(feature = "json-test-grammar")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use simdlex::{HtmlConfig, HtmlEmit, HtmlGrammar, IndexMode, JsonConfig, JsonGrammar, Parser};

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
fn bounded_warm_parsers_allocate_nothing() {
    let json = br#"{"items":[1,2,3,{"name":"value","ok":true}],"none":null}"#;
    let html = br#"<div class="px-4 py-2"><span className='font-bold'></span></div>"#;
    let mut json_parser = Parser::<JsonGrammar>::with_capacity(JsonConfig::default(), 256).unwrap();
    let mut html_parser =
        Parser::<HtmlGrammar>::with_capacity(HtmlConfig::default().emit(HtmlEmit::Tokens), 256)
            .unwrap();
    let mut html_mask_parser = Parser::<HtmlGrammar>::with_capacity(HtmlConfig::default(), 256)
        .unwrap()
        .with_index_mode(IndexMode::Masks)
        .unwrap();

    json_parser.parse_with(json, |_| {}).unwrap();
    html_parser.parse_with(html, |_| {}).unwrap();
    html_mask_parser.parse_with(html, |_| {}).unwrap();

    ALLOCATIONS.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::SeqCst);
    json_parser.parse_with(json, |_| {}).unwrap();
    html_parser.parse_with(html, |_| {}).unwrap();
    html_mask_parser.parse_with(html, |_| {}).unwrap();
    ENABLED.store(false, Ordering::SeqCst);

    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
