#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::available_backends;
use simdlex_html::{HtmlParser, HtmlReader};

// The event stream over the stage-1 tape must equal the stream over a
// reference tape that lists every offset, for every backend.
fuzz_target!(|input: &[u8]| {
    let every: Vec<u32> = (0..input.len() as u32).collect();
    let expected: Vec<_> = HtmlReader::new(input, &every).collect();

    for backend in available_backends() {
        let mut parser = HtmlParser::with_backend(backend).unwrap();
        let actual: Vec<_> = parser.events(input).collect();
        assert_eq!(actual, expected);
    }
});
