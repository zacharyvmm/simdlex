#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{JsonConfig, JsonGrammar, Parser};

fuzz_target!(|input: &[u8]| {
    let expected = serde_json::from_slice::<serde_json::Value>(input).is_ok();
    let mut parser = Parser::<JsonGrammar>::new(JsonConfig::default());
    let actual = parser.parse_with(input, |_| {}).is_ok();
    assert_eq!(actual, expected);
});
