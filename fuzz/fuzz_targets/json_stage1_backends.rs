#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{BackendKind, JsonConfig, JsonGrammar, Parser};

fuzz_target!(|input: &[u8]| {
    let mut scalar =
        Parser::<JsonGrammar>::with_backend(JsonConfig::default(), BackendKind::Scalar)
            .unwrap();
    let scalar_result = scalar.stage1(input);
    let scalar_offsets = scalar.index().positions().collect::<Vec<_>>();

    for backend in [BackendKind::Ssse3, BackendKind::Avx2] {
        let Ok(mut parser) =
            Parser::<JsonGrammar>::with_backend(JsonConfig::default(), backend)
        else {
            continue;
        };
        assert_eq!(parser.stage1(input), scalar_result);
        assert_eq!(parser.index().positions().collect::<Vec<_>>(), scalar_offsets);
    }
});
