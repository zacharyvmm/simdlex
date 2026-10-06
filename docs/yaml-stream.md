# YAML: mask-mode stage 1 and event stream

> Experimental. Events only (libyaml-style): no tree, no schema resolution
> (`null`, `true`, `1` are plain scalars), no tag-handle resolution.

## Why masks, not a tape

YAML's structure is carried by indentation, so the bytes stage 2 must see
(every space and line break, plus `:`, `#`, quotes, and flow indicators) are
dense. On the benchmark fixture, 35% of bytes are significant:

| stage 1 mode | index size (754 KB input) | throughput (AVX2) |
|---|---|---|
| `Masks` (default) | 754 KB (8 masks × 8 B per 64 B) | 2.94 GiB/s |
| `Tape` | 1.06 MB (4 B per structural) | 2.30 GiB/s |

Stage 2 needs the masks: it measures indentation with
`MaskView::run_length(SPACE, line)` and finds the next line feed, `:`,
`#`, quote, or flow indicator with `MaskView::next_in(classes, from)`, both
added to `simdlex-index`. Run in tape mode, stage 2 returns
`YamlErrorKind::RequiresMaskIndex`.

Classes (8 table bits, generated): line feed, space, `:`, `#`, `"`, `'`,
and `[]{},`. Quote and comment context are not resolved in stage 1: an
apostrophe inside a plain scalar (`don't`) is text, so prefix-XOR parity
would be wrong.

## Events

```rust
let mut parser = Parser::<YamlGrammar>::new(YamlConfig::default());
let mut scratch = String::new();
parser.parse_with(input, |event| match event {
    YamlEvent::Scalar(scalar) => {
        let value = decode_yaml_scalar(&scalar, &mut scratch).unwrap();
    }
    YamlEvent::MappingStart { style, properties, .. } => {}
    _ => {}
})?;
```

`DocumentStart`/`DocumentEnd` (with `explicit` for `---`/`...`),
`MappingStart`/`MappingEnd`, `SequenceStart`/`SequenceEnd` (block or flow
style), `Scalar` (raw span, style, anchor/tag), and `Alias`. An empty node,
such as the value in `key:`, is an empty plain scalar. Scalars are borrowed
raw spans; `decode_yaml_scalar` applies line folding, escapes, block
indentation, and chomping into caller-owned scratch. Parsing and decoding
allocate nothing with `Parser::with_capacity` and a warm scratch string.

## Supported subset

- Block mappings and sequences, including sequences at their parent key's
  indentation and compact nesting (`- k: v`, `- - x`).
- Plain (multi-line), single-quoted, and double-quoted (multi-line, all YAML
  1.2 escapes) scalars; literal and folded block scalars with chomping and
  explicit indentation indicators.
- Flow sequences and mappings, nested and multi-line, including JSON-style
  `{"a":1}`.
- Anchors, aliases, and tags (as written); comments; `---`/`...` and
  multiple documents; directives (skipped); a leading byte-order mark; CRLF.

Reported as `YamlErrorKind::Unsupported`: explicit keys (`? k`), empty keys
(`: v`), and single-pair mappings in flow sequences (`[a: b]`). Complex keys
(`[a]: b`) are rejected. Indentation of flow and quoted continuation lines is
not validated.

## Verification

- Event streams in yaml-test-suite notation for each feature, plus error
  kinds and offsets.
- Differential testing against `yaml-rust2` (dev-dependency): a corpus,
  1 500 random trees serialized in random styles (which must also round-trip
  to the original tree), and random YAML-ish byte strings. A 3M-input run
  compared 907 117 documents that both parsers accepted. Two yaml-rust2
  divergences are excluded, both checked against libyaml/PyYAML or the
  spec: it appends a line break to block scalars that end at end of input,
  and it lets zero-indented block scalars run over `---` lines.
- Every backend and 70 alignments produce identical events; random byte
  strings never panic and agree across backends.
- Fuzz target `yaml_differential` (backends, then yaml-rust2).

## Benchmarks (local, AVX2, 754 KB fixture)

| | throughput |
|---|---|
| events | 177 MiB/s |
| events + decode every scalar | 103 MiB/s |
| yaml-rust2 events | 38 MiB/s |

Stage 2 dominates; stage 1 runs at 2.9 GiB/s.
