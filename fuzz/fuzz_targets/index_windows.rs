#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{StructuralIndex, available_backends};
use simdlex_json::JsonSpec;

// For a stateful spec (JSON strings and escapes), every backend and every
// window size must produce the tape of a whole-input scan, and the same
// error.
type Indexer = simdlex::Indexer<JsonSpec>;

fuzz_target!(|data: &[u8]| {
    let Some((&window_blocks, input)) = data.split_first() else {
        return;
    };
    let window = (usize::from(window_blocks % 8) + 1) * 64;

    let mut whole = StructuralIndex::new();
    let mut state = Default::default();
    let expected = Indexer::with_backend(simdlex::BackendKind::Scalar)
        .unwrap()
        .index(input, &mut state, &mut whole);

    for backend in available_backends() {
        let indexer = Indexer::with_backend(backend).unwrap();
        let mut index = StructuralIndex::new();
        let mut state = Default::default();
        let mut tape = Vec::new();
        let mut start = 0;
        let result = loop {
            let end = (start + window).min(input.len());
            if let Err(error) = indexer.index_window(input, start..end, &mut state, &mut index) {
                break Err(error);
            }
            tape.extend_from_slice(index.tape());
            if end == input.len() {
                break Ok(());
            }
            start = end;
        };
        assert_eq!(result, expected);
        if expected.is_ok() {
            assert_eq!(tape, whole.tape());
        }
    }
});
