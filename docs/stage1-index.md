# Stage 1: table generation and indexing

> Experimental. The crate boundaries and the `IndexSpec` hook are the point of
> this change; the stage-2 HTML state machine is out of scope and the existing
> tag walker is kept as the consumer.

Stage 1 is split into two dependency-free crates. `simdlex` glues them to
grammars and stage 2.

```text
crates/simdlex-tablegen   named byte sets ──► NibbleTables + class bits   (const fn, no_std)
crates/simdlex-index      NibbleTables + IndexSpec ──► tape | block masks (scalar/SSSE3/AVX2)
simdlex                   Grammar = IndexSpec + stage-2 parse(IndexView)
```

`simdlex-index` never sees the generator: tables are plain data, so
hand-written tables keep working. `simdlex-tablegen` never sees a backend.

## 1. Table generation (`simdlex-tablegen`)

A grammar lists its byte classes and generates tables in a `const` item:

```rust
const CLASSES: [ByteSet; 5] = [
    ByteSet::from_bytes(b"<"),
    ByteSet::from_bytes(b">"),
    ByteSet::from_bytes(b"="),
    ByteSet::from_bytes(b"'"),
    ByteSet::from_bytes(b"\""),
];
const GENERATED: Tables<5> = unwrap(generate(&CLASSES));
```

Each class is partitioned into `low-nibble-set × high-nibble-set` rectangles.
One table bit recognises exactly one rectangle, and identical rectangles from
different classes share a bit. That sharing is how JSON's overlapping
whitespace and control classes fit in eight bits. The cheapest per-class
partition is tried first, then every combination if it does not fit. The
result is checked against all 256 byte values before it is returned.
Unsatisfiable requests (more than eight bits) fail at compile time. Minimum
biclique cover is NP-hard, so this is a heuristic; `verify` lets hand-written
tables get the same exhaustive check.

## 2. Execution (`simdlex-index`)

An `IndexSpec` supplies tables, class selectors, and one hook:

```rust
fn scan_block(input: &[u8; 64], base: u32, valid: u64,
              masks: &mut ClassifiedBlock, state: &mut Self::State)
    -> Result<u64, Self::Error>;
```

The backend classifies a block, the hook refines the masks (JSON removes
in-string bytes and finds scalar starts; HTML just unions its four classes)
and returns the structural lanes. The indexer then records the block in one
of two modes, chosen at compile time inside the block loop:

| `IndexMode` | Storage | Cost | Use when |
|---|---|---|---|
| `Tape` | global `Vec<u32>` of offsets | 4 B per structural | delimiters are sparse single bytes (JSON, HTML tags) |
| `Masks` | per block: structural + each class + each exact mask | `8 × (1 + classes + exacts)` B per 64 B, fixed | significant bytes are dense (YAML indentation) |

A spec declares its preferred `MODE`. `Parser::with_index_mode` overrides it
for experiments. Stage 2 receives an `IndexView`:
`tape()` / `masks()` give the raw representation, `positions()` yields
structural offsets from either one, and `MaskView::class_positions(c)` walks a
single class lazily.

## HTML

HTML stage 1 indexes exactly `<`, `>`, `'`, and `"`, one class each.
Whitespace, `/`, and `=` are not structural: the tag walker reads names,
whitespace, and the `=` that follows an attribute name directly from the
bytes between delimiters. Indexing `=` was measured and rejected: it was
13–21% of structurals on real pages, stage 2 only ever skipped those
entries, and dropping it made stage 1 8–12% faster with identical events.

Unlike JSON strings, quotes cannot be masked in stage 1, because a quote
only opens a value inside a tag, and apostrophes in text would break
prefix-XOR parity. Quote pairing therefore stays in stage 2. The default
mode is `Tape`.

Measured on the `stage1/html` fixture (AVX2, 8 192 repetitions, ~1 MiB):

| | main | this change |
|---|---|---|
| stage 1, tape | 4.70 GiB/s | 6.16 GiB/s |
| stage 1, masks | – | 4.75 GiB/s |
| `extract_candidates` 8192 | 0.90 GiB/s | 1.06 GiB/s |

JSON stage 1 on the same machine went from 1.76 GiB/s to 1.94 GiB/s.

## Out of scope / next steps

- HTML tree construction on top of the [element stream](html-stream.md).
- A dense-whitespace grammar (YAML-like) to exercise `Masks` as a default.
- Smarter tablegen search (overlapping rectangles) if a grammar needs it.
