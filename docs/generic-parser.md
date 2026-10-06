# Generic compile-time grammar parser

`simdlex` exposes a reusable `Parser<G: Grammar>`. A grammar owns its nibble
tables, lexical state, configuration, errors, events, and stage-2 state.
Scalar, SSSE3, and AVX2 backends (in `simdlex-index`) only perform
grammar-neutral lookup, exact comparison, mask assembly, and tape or mask
recording.

```rust
use simdlex::{HtmlConfig, HtmlEvent, HtmlGrammar, Parser};

let source = br#"<div class="px-4 font-bold"></div>"#;
let mut parser = Parser::<HtmlGrammar>::new(HtmlConfig::default());
parser.parse_with(source, |event| {
    if let HtmlEvent::ClassCandidate { raw, span } = event {
        println!("{span:?}: {}", String::from_utf8_lossy(raw));
    }
})?;
# Ok::<(), simdlex::HtmlError>(())
```

All source offsets and spans are `u32`; inputs must be shorter than
`u32::MAX`. `with_capacity` reserves a worst-case structural index and the
grammar's state stack. Calls within that bound allocate nothing after
construction. `new` grows for a larger first input and retains its buffers.
CPU detection happens once at construction, while `with_backend` supports
deterministic scalar/SSSE3/AVX2 comparison.

The `json-test-grammar` feature adds `JsonGrammar`. Its stage 1 discovers
operators and pseudo-structural scalar starts, carries quote and escape state
across 64-byte blocks, rejects controls in strings, and validates UTF-8.
Stage 2 validates the full JSON grammar and emits borrowed raw strings and
tokens without a DOM. `decode_json_string` performs optional decoding into
caller-owned reusable scratch storage.

## Table generation and indexing

Stage 1 lives in two workspace crates: `simdlex-tablegen` generates nibble
tables from named byte sets at compile time, and `simdlex-index` executes
them and records either an offset tape or per-block masks. See
[`stage1-index.md`](stage1-index.md).

## Verification and benchmarks

- `cargo test --workspace --all-features` runs grammar correctness, exhaustive table,
  backend equivalence, alignment/tail, differential JSON, and allocation tests.
- `cargo fuzz run json_stage1_backends`, `json_differential`, and
  `html_backends` exercise the corresponding long-running fuzz properties.
- `just bench-grammars` measures JSON stage 1, prebuilt-tape stage 2, validated
  event traversal, HTML candidates/tokens, and a cold first parse.
- `just bench-reference` optionally checks out simdjson commit
  `8e6bac94877f2d3d026000d36ce81e0aaf38d26f` below `target/`, builds its
  reference parser, and runs Haswell and Westmere stage-1 and On-Demand
  traversal comparisons on the pinned `twitter.json` and
  `citm_catalog.json`. It is never part of a normal Cargo build or offline
  test.
