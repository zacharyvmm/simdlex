use std::collections::HashMap;

use yaml_rust2::parser::{Event, EventReceiver, Parser as ReferenceParser};
use yaml_rust2::scanner::TScalarStyle;

use super::*;
use crate::{BackendKind, Parser};

/// Events in the yaml-test-suite notation, with decoded scalar values.
fn render(input: &str) -> Result<Vec<String>, YamlError> {
    let mut parser = Parser::<YamlGrammar>::new(YamlConfig::default());
    let mut events = Vec::new();
    parser.parse_with(input.as_bytes(), |event| events.push(event))?;
    let mut scratch = String::new();
    events
        .into_iter()
        .map(|event| render_event(event, &mut scratch))
        .collect()
}

fn render_event(event: YamlEvent<'_>, scratch: &mut String) -> Result<String, YamlError> {
    let properties = |properties: YamlProperties<'_>| {
        let mut out = String::new();
        if let Some(anchor) = properties.anchor {
            out.push_str(&format!(" &{}", String::from_utf8_lossy(anchor)));
        }
        if let Some(tag) = properties.tag {
            out.push_str(&format!(" <{}>", String::from_utf8_lossy(tag)));
        }
        out
    };
    let flow = |style| match style {
        YamlCollectionStyle::Flow => " {}",
        YamlCollectionStyle::Block => "",
    };
    Ok(match event {
        YamlEvent::DocumentStart { explicit, .. } => {
            if explicit { "+DOC ---" } else { "+DOC" }.into()
        }
        YamlEvent::DocumentEnd { explicit, .. } => {
            if explicit { "-DOC ..." } else { "-DOC" }.into()
        }
        YamlEvent::MappingStart {
            style,
            properties: p,
            ..
        } => format!("+MAP{}{}", flow(style), properties(p)),
        YamlEvent::SequenceStart {
            style,
            properties: p,
            ..
        } => format!("+SEQ{}{}", flow(style).replace("{}", "[]"), properties(p)),
        YamlEvent::MappingEnd { .. } => "-MAP".into(),
        YamlEvent::SequenceEnd { .. } => "-SEQ".into(),
        YamlEvent::Alias { name, .. } => format!("=ALI *{}", String::from_utf8_lossy(name)),
        YamlEvent::Scalar(scalar) => {
            let style = match scalar.style {
                YamlScalarStyle::Plain => ':',
                YamlScalarStyle::SingleQuoted => '\'',
                YamlScalarStyle::DoubleQuoted => '"',
                YamlScalarStyle::Literal(_) => '|',
                YamlScalarStyle::Folded(_) => '>',
            };
            let value = decode_yaml_scalar(&scalar, scratch)?;
            format!(
                "=VAL{} {style}{}",
                properties(scalar.properties),
                escape(value)
            )
        }
    })
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn lines(expected: &str) -> Vec<String> {
    expected
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

#[track_caller]
fn assert_events(input: &str, expected: &str) {
    assert_eq!(render(input).unwrap(), lines(expected), "input: {input:?}");
    assert_matches_reference(input);
}

// ----- reference comparison -----

/// Events normalized to what both parsers report: no tags, no explicit
/// document markers, anchors numbered by definition order.
fn normalize(input: &str) -> Result<Vec<String>, YamlError> {
    let mut parser = Parser::<YamlGrammar>::new(YamlConfig::default());
    let mut events = Vec::new();
    parser.parse_with(input.as_bytes(), |event| events.push(event))?;

    let mut anchors = HashMap::new();
    fn define(anchors: &mut HashMap<Vec<u8>, usize>, properties: YamlProperties<'_>) -> usize {
        match properties.anchor {
            Some(anchor) => {
                let id = anchors.len() + 1;
                anchors.insert(anchor.to_vec(), id);
                id
            }
            None => 0,
        }
    }
    let mut scratch = String::new();
    let mut out = Vec::new();
    for event in events {
        out.push(match event {
            YamlEvent::DocumentStart { .. } => "+DOC".into(),
            YamlEvent::DocumentEnd { .. } => "-DOC".into(),
            YamlEvent::MappingStart { properties, .. } => {
                format!("+MAP &{}", define(&mut anchors, properties))
            }
            YamlEvent::SequenceStart { properties, .. } => {
                format!("+SEQ &{}", define(&mut anchors, properties))
            }
            YamlEvent::MappingEnd { .. } => "-MAP".into(),
            YamlEvent::SequenceEnd { .. } => "-SEQ".into(),
            YamlEvent::Alias { name, .. } => format!("=ALI *{}", anchors.get(name).unwrap_or(&0)),
            YamlEvent::Scalar(scalar) => {
                let style = match scalar.style {
                    YamlScalarStyle::Plain => "plain",
                    YamlScalarStyle::SingleQuoted => "single",
                    YamlScalarStyle::DoubleQuoted => "double",
                    YamlScalarStyle::Literal(_) => "literal",
                    YamlScalarStyle::Folded(_) => "folded",
                };
                let value = decode_yaml_scalar(&scalar, &mut scratch)?;
                format!(
                    "=VAL &{} {style} {}",
                    define(&mut anchors, scalar.properties),
                    escape(value)
                )
            }
        });
    }
    Ok(out)
}

struct Reference(Vec<String>);

impl EventReceiver for Reference {
    fn on_event(&mut self, event: Event) {
        let line = match event {
            Event::StreamStart | Event::StreamEnd | Event::Nothing => return,
            Event::DocumentStart => "+DOC".into(),
            Event::DocumentEnd => "-DOC".into(),
            Event::MappingStart(anchor, _) => format!("+MAP &{anchor}"),
            Event::SequenceStart(anchor, _) => format!("+SEQ &{anchor}"),
            Event::MappingEnd => "-MAP".into(),
            Event::SequenceEnd => "-SEQ".into(),
            Event::Alias(anchor) => format!("=ALI *{anchor}"),
            Event::Scalar(value, style, anchor, _) => {
                let style = match style {
                    TScalarStyle::Plain => "plain",
                    TScalarStyle::SingleQuoted => "single",
                    TScalarStyle::DoubleQuoted => "double",
                    TScalarStyle::Literal => "literal",
                    TScalarStyle::Folded => "folded",
                };
                format!("=VAL &{anchor} {style} {}", escape(&value))
            }
        };
        self.0.push(line);
    }
}

fn reference(input: &str) -> Option<Vec<String>> {
    let mut receiver = Reference(Vec::new());
    ReferenceParser::new_from_str(input)
        .load(&mut receiver, true)
        .ok()?;
    Some(receiver.0)
}

#[track_caller]
fn assert_matches_reference(input: &str) {
    let expected = reference(input).expect("the reference parser rejected the input");
    assert_eq!(normalize(input).unwrap(), expected, "input: {input:?}");
}

// ----- event streams -----

#[test]
fn block_mappings_and_sequences() {
    assert_events(
        "name: simdlex\ntags:\n  - fast\n  - yaml\nnested:\n  a: 1\n  b:\n    c: d\n",
        "+DOC
         +MAP
         =VAL :name
         =VAL :simdlex
         =VAL :tags
         +SEQ
         =VAL :fast
         =VAL :yaml
         -SEQ
         =VAL :nested
         +MAP
         =VAL :a
         =VAL :1
         =VAL :b
         +MAP
         =VAL :c
         =VAL :d
         -MAP
         -MAP
         -MAP
         -DOC",
    );
}

#[test]
fn compact_and_same_indent_sequences() {
    assert_events(
        "a:\n- x\n- k: v\n  k2: v2\n- - n\n  - m\nb:\n",
        "+DOC
         +MAP
         =VAL :a
         +SEQ
         =VAL :x
         +MAP
         =VAL :k
         =VAL :v
         =VAL :k2
         =VAL :v2
         -MAP
         +SEQ
         =VAL :n
         =VAL :m
         -SEQ
         -SEQ
         =VAL :b
         =VAL :
         -MAP
         -DOC",
    );
}

#[test]
fn empty_values_and_entries() {
    assert_events(
        "- \n-\n- a\n-\n",
        "+DOC
         +SEQ
         =VAL :
         =VAL :
         =VAL :a
         =VAL :
         -SEQ
         -DOC",
    );
    assert_events(
        "k:\nj: # comment\nl: !t\n",
        "+DOC
         +MAP
         =VAL :k
         =VAL :
         =VAL :j
         =VAL :
         =VAL :l
         =VAL <!t> :
         -MAP
         -DOC",
    );
}

#[test]
fn scalar_styles() {
    assert_events(
        "plain: a b:c # comment\nsingle: 'it''s'\ndouble: \"a\\tb\\u00e9\\n\"\nmulti: one\n  two\n\n  three\nurl: http://x.y/z\n",
        "+DOC
         +MAP
         =VAL :plain
         =VAL :a b:c
         =VAL :single
         =VAL 'it's
         =VAL :double
         =VAL \"a\\tbé\\n
         =VAL :multi
         =VAL :one two\\nthree
         =VAL :url
         =VAL :http://x.y/z
         -MAP
         -DOC",
    );
}

#[test]
fn multi_line_quoted_scalars() {
    assert_events(
        "a: \"one\n  two\\\n  three \\\n\n  four\"\nb: 'x\n\n  y z'\n",
        "+DOC
         +MAP
         =VAL :a
         =VAL \"one twothree \\nfour
         =VAL :b
         =VAL 'x\\ny z
         -MAP
         -DOC",
    );
}

#[test]
fn block_scalars() {
    assert_events(
        "lit: |\n  a\n   b\n\n  c\n\nfold: >-\n  one\n  two\n\n  three\n    more\n  end\nkeep: |+\n  k\n\n\nexplicit: |2\n    indented\nlast: >\n x\n",
        "+DOC
         +MAP
         =VAL :lit
         =VAL |a\\n b\\n\\nc\\n
         =VAL :fold
         =VAL >one two\\nthree\\n  more\\nend
         =VAL :keep
         =VAL |k\\n\\n\\n
         =VAL :explicit
         =VAL |  indented\\n
         =VAL :last
         =VAL >x\\n
         -MAP
         -DOC",
    );
}

#[test]
fn whitespace_only_block_scalars() {
    // Matches libyaml; see `equivalent` for yaml-rust2's divergence.
    for (input, expected) in [
        ("a: |\n   ", ""),
        ("a: |\n   \n\n", ""),
        ("a: |+\n   \n\n", "\\n\\n"),
        ("a: |\n  x", "x"),
        ("a: |\n  x\n    \n", "x\\n  \\n"),
    ] {
        assert_eq!(
            render(input).unwrap()[3],
            format!("=VAL |{expected}"),
            "{input:?}"
        );
    }
}

#[test]
fn flow_collections() {
    assert_events(
        "a: [1, 'two', \"three\", [x, y], {k: v, e}]\nb: {\"j\":1, m: [], n: {}}\nc: [\n  multi\n  line,\n  # comment\n  end,\n]\n",
        "+DOC
         +MAP
         =VAL :a
         +SEQ []
         =VAL :1
         =VAL 'two
         =VAL \"three
         +SEQ []
         =VAL :x
         =VAL :y
         -SEQ
         +MAP {}
         =VAL :k
         =VAL :v
         =VAL :e
         =VAL :
         -MAP
         -SEQ
         =VAL :b
         +MAP {}
         =VAL \"j
         =VAL :1
         =VAL :m
         +SEQ []
         -SEQ
         =VAL :n
         +MAP {}
         -MAP
         -MAP
         =VAL :c
         +SEQ []
         =VAL :multi line
         =VAL :end
         -SEQ
         -MAP
         -DOC",
    );
}

#[test]
fn anchors_aliases_and_tags() {
    assert_events(
        "base: &b {x: 1}\nref: *b\nseq: &s\n  - !!str 1\n  - &v !local v\nmap: !!map\n  &k key: *v\n",
        "+DOC
         +MAP
         =VAL :base
         +MAP {} &b
         =VAL :x
         =VAL :1
         -MAP
         =VAL :ref
         =ALI *b
         =VAL :seq
         +SEQ &s
         =VAL <!!str> :1
         =VAL &v <!local> :v
         -SEQ
         =VAL :map
         +MAP <!!map>
         =VAL &k :key
         =ALI *v
         -MAP
         -MAP
         -DOC",
    );
}

#[test]
fn verbatim_tags_end_at_their_closing_bracket() {
    // The URI may contain flow indicators.
    assert_events(
        "a: !<tag:yaml.org,2002:str> foo\nb: [!<tag:yaml.org,2002:str> foo, !<x>]\n",
        "+DOC
         +MAP
         =VAL :a
         =VAL <!<tag:yaml.org,2002:str>> :foo
         =VAL :b
         +SEQ []
         =VAL <!<tag:yaml.org,2002:str>> :foo
         =VAL <!<x>> :
         -SEQ
         -MAP
         -DOC",
    );
    let kind = |input: &str| render(input).unwrap_err();
    assert_eq!(kind("a: !<x y"), error(YamlErrorKind::UnexpectedContent, 3));
    assert_eq!(kind("a: !<x>y"), error(YamlErrorKind::UnexpectedContent, 3));
    assert_eq!(kind("a: !<> y"), error(YamlErrorKind::UnexpectedContent, 3));
}

#[test]
fn flow_properties_span_lines() {
    assert_events(
        "[&a\n  !foo value, &b # comment\n  !bar\n]\n",
        "+DOC
         +SEQ []
         =VAL &a <!foo> :value
         =VAL &b <!bar> :
         -SEQ
         -DOC",
    );
    assert_events(
        "{k: !foo\n  &a v}\n",
        "+DOC
         +MAP {}
         =VAL :k
         =VAL &a <!foo> :v
         -MAP
         -DOC",
    );
}

#[test]
fn documents_comments_and_directives() {
    assert_events(
        "# leading comment\n%YAML 1.2\n---\na: 1 # trailing\n...\n--- text\n---\n- x\n",
        "+DOC ---
         +MAP
         =VAL :a
         =VAL :1
         -MAP
         -DOC ...
         +DOC ---
         =VAL :text
         -DOC
         +DOC ---
         +SEQ
         =VAL :x
         -SEQ
         -DOC",
    );
    assert_events("", "");
    assert_events("# only a comment\n", "");
    assert_events("---\n", "+DOC --- \n =VAL : \n -DOC");
    // yaml-rust2 keeps the byte-order mark as content; the spec strips it.
    assert_eq!(
        render("\u{feff}a: b\r\nc: d\r\n").unwrap(),
        lines("+DOC\n+MAP\n=VAL :a\n=VAL :b\n=VAL :c\n=VAL :d\n-MAP\n-DOC")
    );
}

#[test]
fn reports_errors_with_offsets() {
    let kind = |input: &str| render(input).unwrap_err();
    assert_eq!(kind("a: 'open"), error(YamlErrorKind::UnterminatedQuote, 3));
    assert_eq!(kind("a: [1, 2"), error(YamlErrorKind::UnterminatedFlow, 3));
    assert_eq!(
        kind("a: 1\n  b: 2"),
        error(YamlErrorKind::UnexpectedContent, 8)
    );
    assert_eq!(
        kind("a:\n  b: 1\n c: 2"),
        error(YamlErrorKind::BadIndentation, 11)
    );
    assert_eq!(kind("a:\n\tb: 1"), error(YamlErrorKind::TabIndentation, 3));
    assert_eq!(kind("a: b: c"), error(YamlErrorKind::UnexpectedContent, 4));
    assert_eq!(kind("? a\n: b"), error(YamlErrorKind::Unsupported, 0));
    assert_eq!(kind("[a: b]"), error(YamlErrorKind::Unsupported, 2));
    assert_eq!(kind("a: \"\\q\""), error(YamlErrorKind::InvalidEscape, 4));
    assert_eq!(kind("a\nb: c"), error(YamlErrorKind::UnexpectedContent, 3));
    assert_eq!(
        kind("a: |x\n"),
        error(YamlErrorKind::InvalidBlockScalarHeader, 4)
    );
    assert_eq!(kind("a: @b"), error(YamlErrorKind::InvalidPlainScalar, 3));

    assert_eq!(
        kind("a: |\n    \n  x\n"),
        error(YamlErrorKind::BadIndentation, 10)
    );
    let mut shallow = Parser::<YamlGrammar>::new(YamlConfig { max_depth: 2 });
    assert_eq!(
        shallow.parse_with(b"[[[a]]]", |_| {}).unwrap_err(),
        error(YamlErrorKind::Depth, 2)
    );
    assert_eq!(
        Parser::<YamlGrammar>::new(YamlConfig::default())
            .parse_with(b"a: \xff", |_| {})
            .unwrap_err(),
        error(YamlErrorKind::Utf8, 3)
    );
    let mut tape = Parser::<YamlGrammar>::new(YamlConfig::default())
        .with_index_mode(IndexMode::Tape)
        .unwrap();
    assert_eq!(
        tape.parse_with(b"a: 1", |_| {}).unwrap_err().kind,
        YamlErrorKind::RequiresMaskIndex
    );
}

// ----- differential and property tests -----

const CORPUS: &[&str] = &[
    "a: 1\nb: [x, y]\nc: {d: e}\n",
    "- a\n- - b\n  - c\n- d: e\n  f: g\n",
    "key:\n  - 1\n  - 2\nother: 'x'\n",
    "a:\n- 1\n- 2\nb: 3\n",
    "s: \"esc \\\" \\\\ \\x41 \\u263A\"\n",
    "f: >\n  folded\n  text\n\n  para\nl: |-\n  keep\n  lines\n",
    "deep:\n  er:\n    est:\n      - [1, {a: b}]\n",
    "anchor: &a value\nalias: *a\n",
    "--- a\n--- b\n...\n",
    "x: 'multi\n  line'\ny: \"multi\n\n  line\"\n",
    "z: plain\n  continued\n  # comment\n",
    "- |\n  block in seq\n- >+\n  kept\n\n",
    "{a: [1, 2], b: {c: d}}\n",
    "[a, b, c]\n",
    "empty:\nnext: 1\n",
    "q: 'a #b'\nr: a#b\n",
    "unicode: é雪 ✓\n",
    "tabs:\t[a,\tb]\n",
    "win: dows\r\nline: ends\r\n",
];

#[test]
fn corpus_matches_reference_parser() {
    for input in CORPUS {
        let ours = normalize(input);
        match reference(input) {
            Some(expected) => assert_eq!(ours.unwrap(), expected, "input: {input:?}"),
            None => assert!(ours.is_err(), "input: {input:?}"),
        }
    }
}

#[test]
fn unsupported_features_are_reported() {
    for input in ["? a\n: b\n", "? \n", "[a: b]\n", "{: v}\n", ": v\n"] {
        assert!(
            reference(input).is_some(),
            "the reference rejects {input:?}"
        );
        assert_eq!(
            render(input).unwrap_err().kind,
            YamlErrorKind::Unsupported,
            "input: {input:?}"
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Scalar(String),
    Seq(Vec<Node>),
    Map(Vec<(String, Node)>),
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const FRAGMENTS: &[&str] = &[
    "a",
    "b c",
    "x:y",
    "it's",
    "#1",
    "a #b",
    "k: v",
    "- x",
    "-x",
    "[a]",
    "{b}",
    "a,b",
    "\"q\"",
    "tab\there",
    "line\nbreak",
    "two\n\nbreaks",
    "trail\n",
    "",
    " lead",
    "trail ",
    "null",
    "é",
    "雪",
    "back\\slash",
    "?x",
    "!x",
    "&x",
    "*x",
    "|",
    ">",
    "a  b",
    "text\nwith\nlines\n",
];

fn random_node(rng: &mut Rng, depth: usize) -> Node {
    match if depth == 0 { 0 } else { rng.below(4) } {
        0 | 1 => Node::Scalar(FRAGMENTS[rng.below(FRAGMENTS.len())].to_owned()),
        2 => Node::Seq(
            (0..rng.below(4))
                .map(|_| random_node(rng, depth - 1))
                .collect(),
        ),
        _ => Node::Map(
            (0..rng.below(4))
                .map(|index| (format!("k{index}"), random_node(rng, depth - 1)))
                .collect(),
        ),
    }
}

fn plain_safe(value: &str, flow: bool) -> bool {
    let first = value.chars().next();
    !value.is_empty()
        && !value.starts_with([' ', '\t'])
        && !value.ends_with([' ', '\t'])
        && !value.contains(['\n', '\t'])
        && !value.contains(": ")
        && !value.contains(" #")
        && !value.ends_with(':')
        && !(flow && value.contains([',', '[', ']', '{', '}']))
        && !matches!(
            first,
            Some(
                ',' | '['
                    | ']'
                    | '{'
                    | '}'
                    | '#'
                    | '&'
                    | '*'
                    | '!'
                    | '|'
                    | '>'
                    | '\''
                    | '"'
                    | '%'
                    | '@'
                    | '`'
            )
        )
        && !(matches!(first, Some('-' | '?' | ':'))
            && (value.len() == 1 || value[1..].starts_with([' ', '\t'])))
}

fn double_quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A scalar that fits on the current line.
fn inline_scalar(rng: &mut Rng, value: &str, flow: bool) -> String {
    match rng.below(3) {
        0 if plain_safe(value, flow) => value.to_owned(),
        1 if !value.contains(['\n', '\t']) => format!("'{}'", value.replace('\'', "''")),
        _ => double_quoted(value),
    }
}

fn literal_safe(value: &str) -> bool {
    value.lines().any(|line| !line.is_empty())
        && !value.starts_with('\n')
        && value.lines().all(|line| !line.starts_with([' ', '\t']))
        && !value.contains('\t')
}

/// Write `value` after `key:` or `- ` (the cursor sits after the indicator)
/// for a node whose parent collection is at `indent`.
fn write_value(rng: &mut Rng, node: &Node, indent: usize, out: &mut String) {
    match node {
        Node::Scalar(value) if literal_safe(value) && rng.below(3) == 0 => {
            let chomp = if !value.ends_with('\n') {
                "-"
            } else if value.ends_with("\n\n") {
                "+"
            } else {
                ""
            };
            out.push_str(&format!(" |{chomp}\n"));
            for line in value.strip_suffix('\n').unwrap_or(value).split('\n') {
                if !line.is_empty() {
                    out.push_str(&" ".repeat(indent + 2));
                }
                out.push_str(line);
                out.push('\n');
            }
        }
        Node::Scalar(value) => {
            out.push(' ');
            out.push_str(&inline_scalar(rng, value, false));
            out.push('\n');
        }
        _ if rng.below(3) == 0 => {
            out.push(' ');
            write_flow(rng, node, out);
            out.push('\n');
        }
        Node::Seq(items) if items.is_empty() => out.push_str(" []\n"),
        Node::Map(entries) if entries.is_empty() => out.push_str(" {}\n"),
        _ => {
            out.push('\n');
            write_block(rng, node, indent + 2, out);
        }
    }
}

fn write_block(rng: &mut Rng, node: &Node, indent: usize, out: &mut String) {
    match node {
        Node::Seq(items) => {
            for item in items {
                out.push_str(&" ".repeat(indent));
                out.push('-');
                write_value(rng, item, indent, out);
            }
        }
        Node::Map(entries) => {
            for (key, value) in entries {
                out.push_str(&" ".repeat(indent));
                out.push_str(key);
                out.push(':');
                write_value(rng, value, indent, out);
            }
        }
        Node::Scalar(_) => unreachable!(),
    }
}

fn write_flow(rng: &mut Rng, node: &Node, out: &mut String) {
    match node {
        Node::Scalar(value) => out.push_str(&inline_scalar(rng, value, true)),
        Node::Seq(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_flow(rng, item, out);
            }
            out.push(']');
        }
        Node::Map(entries) => {
            out.push('{');
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    // Continuation lines must be indented past any enclosing block.
                    out.push_str(if rng.below(2) == 0 {
                        ", "
                    } else {
                        ",\n                              "
                    });
                }
                out.push_str(key);
                out.push_str(": ");
                write_flow(rng, value, out);
            }
            out.push('}');
        }
    }
}

fn write_document(rng: &mut Rng, node: &Node) -> String {
    let mut out = String::new();
    match node {
        Node::Seq(items) if !items.is_empty() => write_block(rng, node, 0, &mut out),
        Node::Map(entries) if !entries.is_empty() => write_block(rng, node, 0, &mut out),
        _ => {
            out.push_str("---");
            write_value(rng, node, 0, &mut out);
        }
    }
    out
}

/// Rebuild a tree from our events.
fn read_tree(input: &str) -> Result<Node, YamlError> {
    let mut parser = Parser::<YamlGrammar>::new(YamlConfig::default());
    let mut events = Vec::new();
    parser.parse_with(input.as_bytes(), |event| events.push(event))?;

    let mut scratch = String::new();
    let mut stack: Vec<Node> = Vec::new();
    let mut keys: Vec<Option<String>> = Vec::new();
    let mut root = None;
    for event in events {
        let node = match event {
            YamlEvent::SequenceStart { .. } => {
                stack.push(Node::Seq(Vec::new()));
                keys.push(None);
                continue;
            }
            YamlEvent::MappingStart { .. } => {
                stack.push(Node::Map(Vec::new()));
                keys.push(None);
                continue;
            }
            YamlEvent::SequenceEnd { .. } | YamlEvent::MappingEnd { .. } => {
                keys.pop();
                stack.pop().unwrap()
            }
            YamlEvent::Scalar(scalar) => {
                Node::Scalar(decode_yaml_scalar(&scalar, &mut scratch)?.to_owned())
            }
            _ => continue,
        };
        match stack.last_mut() {
            None => root = Some(node),
            Some(Node::Seq(items)) => items.push(node),
            Some(Node::Map(entries)) => match keys.last_mut().unwrap().take() {
                None => {
                    let Node::Scalar(key) = node else {
                        panic!("complex key")
                    };
                    *keys.last_mut().unwrap() = Some(key);
                }
                Some(key) => entries.push((key, node)),
            },
            Some(Node::Scalar(_)) => unreachable!(),
        }
    }
    Ok(root.expect("a document"))
}

#[test]
fn random_documents_round_trip_and_match_reference() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..1_500 {
        let depth = rng.below(4);
        let tree = random_node(&mut rng, depth);
        let document = write_document(&mut rng, &tree);
        assert_eq!(read_tree(&document).unwrap(), tree, "document:\n{document}");
        assert_matches_reference(&document);
    }
}

#[test]
fn every_backend_and_alignment_agrees() {
    let body = "k: [a, 'b', \"c\"]\nlist:\n  - x\n  - y: |\n      z\n";
    for prefix in 0..70 {
        let input = format!("#{}\n{body}", "p".repeat(prefix));
        let expected = render(&input).unwrap();
        for backend in [BackendKind::Scalar, BackendKind::Ssse3, BackendKind::Avx2] {
            for mode in [IndexMode::Hybrid, IndexMode::Masks] {
                let Ok(parser) =
                    Parser::<YamlGrammar>::with_backend(YamlConfig::default(), backend)
                else {
                    continue;
                };
                let mut parser = parser.with_index_mode(mode).unwrap();
                let mut events = Vec::new();
                parser
                    .parse_with(input.as_bytes(), |event| events.push(event))
                    .unwrap();
                let mut scratch = String::new();
                let actual: Vec<String> = events
                    .into_iter()
                    .map(|event| render_event(event, &mut scratch).unwrap())
                    .collect();
                assert_eq!(actual, expected, "prefix {prefix}, {backend:?}, {mode:?}");
            }
        }
    }
}

#[test]
fn table_is_exact_for_named_sets() {
    let members: [&[u8]; 3] = [b"\n", b" ", b"'\""];
    for byte in u8::MIN..=u8::MAX {
        let class = YamlGrammar::TABLES.classify(byte);
        for (index, set) in members.iter().enumerate() {
            let selected = class & YamlGrammar::CLASS_BITS[index] != 0;
            assert_eq!(
                selected,
                set.contains(&byte),
                "byte {byte:#x}, class {index}"
            );
        }
    }
}

#[test]
fn random_bytes_never_panic_and_backends_agree() {
    const ALPHABET: &[u8] = b"ab -:#?&*!|>'\"\\[]{},\n\n  \t\r%";
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut scalar =
        Parser::<YamlGrammar>::with_backend(YamlConfig::default(), BackendKind::Scalar).unwrap();
    let mut best = Parser::<YamlGrammar>::new(YamlConfig::default());
    let mut masks = Parser::<YamlGrammar>::new(YamlConfig::default())
        .with_index_mode(IndexMode::Masks)
        .unwrap();
    let mut scratch = String::new();

    for _ in 0..5_000 {
        let len = rng.below(120);
        let input: Vec<u8> = (0..len)
            .map(|_| ALPHABET[rng.below(ALPHABET.len())])
            .collect();
        let mut expected = Vec::new();
        let expected_result = scalar.parse_with(&input, |event| expected.push(event));
        let mut actual = Vec::new();
        let actual_result = best.parse_with(&input, |event| actual.push(event));
        assert_eq!(actual_result.err(), expected_result.err());
        assert_eq!(actual, expected);
        let mut from_masks = Vec::new();
        let masks_result = masks.parse_with(&input, |event| from_masks.push(event));
        assert_eq!(masks_result.err(), expected_result.err());
        assert_eq!(from_masks, expected);
        for event in expected {
            if let YamlEvent::Scalar(scalar) = event {
                let _ = decode_yaml_scalar(&scalar, &mut scratch);
            }
        }
    }
}

/// Equal, except for two known yaml-rust2 divergences:
///
/// - it appends a line break to block scalars that end at the end of input
///   (or are empty); YAML 1.2 `b-chomped-last` allows end of input there,
///   and libyaml and PyYAML add nothing;
/// - it lets a zero-indented block scalar run over a `---`/`...` line, which
///   `c-forbidden` makes a document boundary.
fn equivalent(ours: &[String], reference: &[String]) -> bool {
    let crosses_marker = |line: &String| {
        (line.contains(" literal ") || line.contains(" folded "))
            && (line.contains("---") || line.contains("..."))
    };
    if reference.iter().any(crosses_marker) {
        return true;
    }
    ours.len() == reference.len()
        && ours.iter().zip(reference).all(|(ours, reference)| {
            ours == reference
                || ((ours.contains(" literal ") || ours.contains(" folded "))
                    && *reference == format!("{ours}\\n"))
        })
}

/// Random YAML-ish inputs that both parsers accept must produce the same
/// events. `SIMDLEX_YAML_ROUNDS` raises the round count for longer runs.
#[test]
fn random_inputs_match_reference_parser() {
    const ALPHABETS: [&[u8]; 3] = [
        b"ab -:#&*!|>'\"\\[]{},\n\n  \t",
        b"ab:-\n\n    \n  '\"[]{},#&*!|>?%",
        b"k: \n- \n  \n    ab|>-+2'\"",
    ];
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let rounds = std::env::var("SIMDLEX_YAML_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(30_000);
    let mut failures = 0;
    let mut compared = 0;
    for round in 0..rounds {
        let alphabet = ALPHABETS[round % ALPHABETS.len()];
        let len = rng.below(48);
        let input: String = (0..len)
            .map(|_| alphabet[rng.below(alphabet.len())] as char)
            .collect();
        let (Ok(ours), Some(expected)) = (normalize(&input), reference(&input)) else {
            continue;
        };
        compared += 1;
        if !equivalent(&ours, &expected) {
            failures += 1;
            if failures <= 15 {
                println!("{input:?}\n  ours: {ours:?}\n  ref:  {expected:?}");
            }
        }
    }
    println!("compared {compared} of {rounds} inputs");
    assert!(compared > rounds / 10);
    assert_eq!(failures, 0);
}
