# Example: YAML event stream

> Experimental. Events only (libyaml-style): no tree, no schema resolution
> (`null`, `true`, `1` are plain scalars), no tag-handle resolution.

## Stage 1: what YAML indexes

Following the rule in [stage1-index.md](../../docs/stage1-index.md#what-the-index-is-for),
the index records only the bounds stage 2 must search for:

| Class | Bound | Stored as |
|---|---|---|
| `\n` | end of every line | mask |
| space | indentation (`MaskView::run_length`) | mask |
| `'` `"` | end of a quoted scalar, which may span many lines | tape |

Everything else is read in place. `:`, `#`, `-` and `[ ] { } ,` all lie
inside a line that stage 2 is already walking between known bounds. Line
feeds and spaces are dense (spaces alone are 19–40% of the bytes in the
files below), so they are masks. Quotes are 0.1–0.6% of the bytes, so they
go on the tape (`YamlSpec` sets `MASK_CLASSES = 2`). It uses three
generated table bits. Quote context is not resolved in stage 1: an
apostrophe in a plain scalar (`don't`) is text, so prefix-XOR parity would
be wrong.

Index size for the 754 KB benchmark fixture:

| Layout | Index |
|---|---|
| masks for `\n` and space, quotes on the tape (this design) | 254 KB: 2 masks × 8 B per 64 B, plus 4 B per quote |
| all 7 earlier classes as masks | 754 KB |
| layout bytes on a tape | 1.06 MB: 35% of the bytes would be tape entries |

### How this design was chosen

All variants were benchmarked interleaved on a synthetic fixture and four
real files (GitHub's 9.9 MB OpenAPI description, a Prometheus Helm values
file, the rust-lang CI workflow, an ingress-nginx manifest):

| Variant | Stage 1 vs first design | Full parse vs first design |
|---|---|---|
| First design: masks `\n` space `:` `#` `"` `'` `[]{},` | – | – |
| Masks `\n` space `:` `-`; tape `#` quotes `[]{},` | +5 to +40% | −10 to −21% |
| …without `-` (never read) | +4 to +61% | −5 to −18% |
| Masks `\n` space `:` `#`; tape quotes `[]{},` | +21 to +41% | +1 to +15% |
| **Masks `\n` space; tape quotes (chosen)** | **+45 to +64%** | **+7 to +29%** |

## Events

```rust
let mut parser = YamlParser::new(YamlConfig::default());
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
allocate nothing with `YamlParser::with_capacity` and a warm scratch string.

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

## Benchmarks (local, AVX2)

Full parse is stage 1 plus stage 2. These numbers come from the interleaved
comparison above.

| Input | Full parse | Parse + decode | First design, full parse |
|---|---|---|---|
| synthetic, 754 KB | 233 MiB/s | 118 MiB/s | 180 MiB/s |
| GitHub OpenAPI, 9.9 MB | 360 MiB/s | 178 MiB/s | 297 MiB/s |
| Prometheus values, 219 KB | 651 MiB/s | 430 MiB/s | 609 MiB/s |
| rust-lang CI, 14 KB | 570 MiB/s | 275 MiB/s | 514 MiB/s |
| ingress-nginx, 16 KB | 262 MiB/s | 131 MiB/s | 229 MiB/s |

yaml-rust2 parses the synthetic fixture at 38 MiB/s on the same machine.
Stage 2 dominates the cost; stage 1 runs at 3.9–5.6 GiB/s.
