# simdlex

simdjson-style structural indexing as a primitive, for parsers of any
grammar whose bounds are single bytes.

A two-stage parser first finds the bounds of its grammar's regions with SIMD
(stage 1), then runs a state machine that jumps between those bounds
(stage 2). Inside known bounds the state machine's hot loop branches
regularly and predicts well, so it stays fast. simdlex is stage 1; stage 2
is yours.

```rust
use simdlex::tablegen::{ByteSet, Tables, generate, unwrap};
use simdlex::{IndexSpec, Indexer, NibbleTables, StructuralIndex};

const TABLES: Tables<2> = unwrap(generate(&[
    ByteSet::from_bytes(b"<>"),
    ByteSet::from_bytes(b"'\""),
]));

struct Html;

impl IndexSpec for Html {
    type State = ();
    type Error = std::convert::Infallible;
    const TABLES: NibbleTables = NibbleTables { low: TABLES.low, high: TABLES.high };
    const CLASS_BITS: [u8; 8] = TABLES.padded_class_bits();
    const CLASS_COUNT: usize = 2;
}

let input = br#"<a href="x">hi</a>"#;
let mut index = StructuralIndex::new();
Indexer::<Html>::new().index(input, &mut (), &mut index).unwrap();
assert_eq!(index.tape(), [0, 8, 10, 11, 14, 17]);
```

## What it provides

| | |
|---|---|
| `tablegen` | Named byte sets → nibble lookup tables, generated at compile time and checked against all 256 bytes. Requests that cannot fit fail the build. |
| `Indexer` | Classifies 64-byte blocks with a scalar, SSSE3, AVX2, or NEON backend (chosen once, by CPU) and flattens structural lanes onto a `u32` tape. Optional per-block masks for dense classes. Whole inputs or 64-byte-aligned windows with state carried across them. |
| `IndexSpec::scan_block` | One hook per block to refine masks with lexical state, such as dropping bytes inside JSON strings. |
| `TapeCursor` | "Next structural of this kind at or after here", with galloping `seek` for a stage 2 that skips ahead. |
| `MaskView` | `next_in` across chosen classes and `run_length` (indentation) over the masks. |

The library is two dependency-free crates, `simdlex-tablegen` and
`simdlex-index`, which the `simdlex` crate re-exports.

## What to index

Index a byte only if stage 2 has to search for it to find a bound. Bytes
stage 2 reads in place cost stage-1 time and are only skipped: HTML's `=`
was 13–21% of the tape on real pages, and dropping it made stage 1 8–12%
faster. See [`docs/stage1-index.md`](docs/stage1-index.md) for the rule, its
measurements, and the design.

## Examples

Complete grammars built on simdlex, each a workspace crate with tests,
benchmarks, and fuzz targets:

- [`examples/html`](examples/html): a StAX-style HTML element stream.
- [`examples/json`](examples/json): a simdjson-style JSON parser and a
  benchmark against simdjson itself.
- [`examples/yaml`](examples/yaml): a YAML event stream over line-feed and
  space masks plus a quote tape, differentially tested against yaml-rust2.

## Verification

- Every backend is checked against the scalar classifier for all 256 byte
  values, and every grammar's events are checked across backends.
- `just test-neon` runs the core suite, NEON backend included, under Miri
  for aarch64; CI also runs everything on a native arm64 runner.
- Windowed indexing must reproduce a whole-input tape, including state
  carried across windows.
- Warm, reserved indexes and parsers are tested to allocate nothing.
