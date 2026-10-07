# Example: JSON in the simdjson style

The yardstick for simdlex: a JSON parser whose stage 1 does what
simdjson's does, so the two can be compared directly.

- **Stage 1** (`JsonSpec`): operators, opening quotes, and the first byte of
  every scalar outside strings, with quote and escape state carried across
  blocks (and across windows) and control characters inside strings
  rejected. This is a custom `IndexSpec::scan_block`.
- **Stage 2**: a validating recursive-descent parser that emits borrowed
  events without a DOM. `decode_json_string` unescapes into caller-owned
  scratch storage.

`JsonParser` validates UTF-8 with the standard library, indexes, and parses.
Tests check acceptance against `serde_json`, and every backend for identical
tapes, errors, and events.

## Comparing with simdjson

```sh
just bench-reference
```

checks out a pinned simdjson commit under `target/`, builds its stage-1
benchmark and an On-Demand traversal, and runs them next to
`cargo run --release -p simdlex-json --example reference_bench` on
`twitter.json` and `citm_catalog.json`. It is never part of a normal build.
