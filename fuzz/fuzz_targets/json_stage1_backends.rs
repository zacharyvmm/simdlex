#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{BackendKind, available_backends};
use simdlex_json::{JsonConfig, JsonParser};

// Every backend builds the same tape and reports the same stage-1 error.
fuzz_target!(|input: &[u8]| {
    let mut scalar = JsonParser::with_backend(JsonConfig::default(), BackendKind::Scalar).unwrap();
    let scalar_result = scalar.stage1(input);
    let scalar_tape = scalar.tape().to_vec();

    for backend in available_backends() {
        let mut parser = JsonParser::with_backend(JsonConfig::default(), backend).unwrap();
        assert_eq!(parser.stage1(input), scalar_result);
        if scalar_result.is_ok() {
            assert_eq!(parser.tape(), scalar_tape);
        }
    }
});
