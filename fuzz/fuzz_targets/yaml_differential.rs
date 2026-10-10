#![no_main]

use libfuzzer_sys::fuzz_target;
use simdlex::{BackendKind, available_backends};
use simdlex_yaml::{YamlConfig, YamlEvent, YamlParser, YamlScalarStyle, decode_yaml_scalar};
use yaml_rust2::parser::{Event, EventReceiver, Parser as ReferenceParser};
use yaml_rust2::scanner::TScalarStyle;

fn block_marker(style: YamlScalarStyle) -> &'static str {
    match style {
        YamlScalarStyle::Literal(_) | YamlScalarStyle::Folded(_) => "|",
        _ => "-",
    }
}

/// See `equivalent` in `src/grammar/yaml/tests.rs`: yaml-rust2 appends a
/// break to block scalars ending at end of input, and lets zero-indented
/// block scalars run over `---`/`...` lines.
fn equivalent(ours: &[String], reference: &[String]) -> bool {
    let crosses_marker = |line: &String| {
        line.starts_with("=VAL | ") && (line.contains("---") || line.contains("..."))
    };
    if reference.iter().any(crosses_marker) {
        return true;
    }
    ours.len() == reference.len()
        && ours.iter().zip(reference).all(|(ours, reference)| {
            ours == reference || (ours.starts_with("=VAL | ") && *reference == format!("{ours}\n"))
        })
}

/// Event kinds and decoded scalar values, which both parsers report.
fn ours(
    input: &[u8],
    backend: BackendKind,
) -> Option<Result<Vec<String>, simdlex_yaml::YamlError>> {
    let mut parser = YamlParser::with_backend(YamlConfig::default(), backend).ok()?;
    let mut events = Vec::new();
    if let Err(error) = parser.parse_with(input, |event| events.push(event)) {
        return Some(Err(error));
    }
    let mut scratch = String::new();
    let mut out = Vec::new();
    for event in events {
        out.push(match event {
            YamlEvent::DocumentStart { .. } => "+DOC".into(),
            YamlEvent::DocumentEnd { .. } => "-DOC".into(),
            YamlEvent::MappingStart { .. } => "+MAP".into(),
            YamlEvent::SequenceStart { .. } => "+SEQ".into(),
            YamlEvent::MappingEnd { .. } => "-MAP".into(),
            YamlEvent::SequenceEnd { .. } => "-SEQ".into(),
            YamlEvent::Alias { .. } => "=ALI".into(),
            YamlEvent::Scalar(scalar) => match decode_yaml_scalar(&scalar, &mut scratch) {
                Ok(value) => format!("=VAL {} {value}", block_marker(scalar.style)),
                Err(error) => return Some(Err(error)),
            },
        });
    }
    Some(Ok(out))
}

struct Reference(Vec<String>);

impl EventReceiver for Reference {
    fn on_event(&mut self, event: Event) {
        self.0.push(match event {
            Event::StreamStart | Event::StreamEnd | Event::Nothing => return,
            Event::DocumentStart => "+DOC".into(),
            Event::DocumentEnd => "-DOC".into(),
            Event::MappingStart(..) => "+MAP".into(),
            Event::SequenceStart(..) => "+SEQ".into(),
            Event::MappingEnd => "-MAP".into(),
            Event::SequenceEnd => "-SEQ".into(),
            Event::Alias(_) => "=ALI".into(),
            Event::Scalar(value, style, ..) => {
                let marker = match style {
                    TScalarStyle::Literal | TScalarStyle::Folded => "|",
                    _ => "-",
                };
                format!("=VAL {marker} {value}")
            }
        });
    }
}

// Every backend must agree exactly, and when both parsers accept a document
// (and it has no byte-order mark, which yaml-rust2 keeps as content) they
// must report the same events and values.
fuzz_target!(|input: &[u8]| {
    let expected = ours(input, BackendKind::Scalar).unwrap();
    for backend in available_backends() {
        if let Some(actual) = ours(input, backend) {
            assert_eq!(actual, expected);
        }
    }

    let (Ok(expected), Ok(text)) = (expected, std::str::from_utf8(input)) else {
        return;
    };
    if text.starts_with('\u{feff}') {
        return;
    }
    let mut reference = Reference(Vec::new());
    if ReferenceParser::new_from_str(text)
        .load(&mut reference, true)
        .is_ok()
    {
        assert!(equivalent(&expected, &reference.0), "{text:?}");
    }
});
