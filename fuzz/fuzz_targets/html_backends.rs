#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{BackendKind, HtmlConfig, HtmlEvent, HtmlGrammar, Parser};

fuzz_target!(|input: &[u8]| {
    let mut scalar =
        Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), BackendKind::Scalar)
            .unwrap();
    let mut expected = Vec::new();
    scalar
        .parse_with(input, |event| expected.push(event))
        .unwrap();
    let expected_offsets = scalar.structural_offsets().to_vec();

    for backend in [BackendKind::Ssse3, BackendKind::Avx2] {
        let Ok(mut parser) =
            Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), backend)
        else {
            continue;
        };
        let mut events: Vec<HtmlEvent<'_>> = Vec::new();
        parser
            .parse_with(input, |event| events.push(event))
            .unwrap();
        assert_eq!(events, expected);
        assert_eq!(parser.structural_offsets(), expected_offsets);
    }
});
