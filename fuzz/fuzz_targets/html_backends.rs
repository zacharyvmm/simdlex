#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{BackendKind, HtmlConfig, HtmlEvent, HtmlGrammar, IndexMode, Parser};

fuzz_target!(|input: &[u8]| {
    let mut scalar =
        Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), BackendKind::Scalar)
            .unwrap();
    let mut expected = Vec::new();
    scalar
        .parse_with(input, |event| expected.push(event))
        .unwrap();
    let expected_offsets = scalar.index().positions().collect::<Vec<_>>();

    for (backend, mode) in [
        (BackendKind::Scalar, IndexMode::Masks),
        (BackendKind::Ssse3, IndexMode::Tape),
        (BackendKind::Ssse3, IndexMode::Masks),
        (BackendKind::Avx2, IndexMode::Tape),
        (BackendKind::Avx2, IndexMode::Masks),
    ] {
        let Ok(parser) = Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), backend)
        else {
            continue;
        };
        let mut parser = parser.with_index_mode(mode).unwrap();
        let mut events: Vec<HtmlEvent<'_>> = Vec::new();
        parser
            .parse_with(input, |event| events.push(event))
            .unwrap();
        assert_eq!(events, expected);
        assert_eq!(parser.index().positions().collect::<Vec<_>>(), expected_offsets);
    }
});
