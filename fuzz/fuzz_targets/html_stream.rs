#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{BackendKind, HtmlReader, HtmlStreamGrammar, IndexMode, Parser};

// The event stream over the stage-1 index must equal the stream over a
// reference index that lists every offset, for every backend and mode.
fuzz_target!(|input: &[u8]| {
    let expected: Vec<_> = HtmlReader::from_positions(input, 0..input.len() as u32).collect();

    for backend in [BackendKind::Scalar, BackendKind::Ssse3, BackendKind::Avx2] {
        for mode in [IndexMode::Tape, IndexMode::Masks] {
            let Ok(parser) = Parser::<HtmlStreamGrammar>::with_backend((), backend) else {
                continue;
            };
            let mut parser = parser.with_index_mode(mode).unwrap();
            let mut pushed = Vec::new();
            parser.parse_with(input, |event| pushed.push(event)).unwrap();
            assert_eq!(pushed, expected);
        }
    }
});
