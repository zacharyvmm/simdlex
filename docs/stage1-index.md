# Stage 1: table generation and indexing

> Experimental. The crate boundaries and the `IndexSpec` hook are the point of
> this change; the stage-2 HTML state machine is out of scope and the existing
> tag walker is kept as the consumer.

Stage 1 is split into two dependency-free crates. `simdlex` glues them to
grammars and stage 2.

```text
crates/simdlex-tablegen   named byte sets ──► NibbleTables + class bits   (const fn, no_std)
crates/simdlex-index      NibbleTables + IndexSpec ──► tape | masks | hybrid (scalar/SSSE3/AVX2)
simdlex                   Grammar = IndexSpec + stage-2 parse(IndexView)
```

`simdlex-index` never sees the generator: tables are plain data, so
hand-written tables keep working. `simdlex-tablegen` never sees a backend.

## What the index is for

Stage 1 does not parse. It is an optimization that finds the **bounds** of
the grammar's regions: where a tag opens and closes, where a line ends and
how far it is indented, where a quoted value closes. With those bounds
known, the stage-2 state machine can be placed directly at the start of a
region and parse the content inside it.

The payoff is branch prediction. A byte-at-a-time state machine that must
also discover region boundaries takes a data-dependent branch on nearly
every byte ("is this `<`? a quote? a line feed?"), and on real text those
branches mispredict often. When the bounds are already known, the hot loop
inside a region runs over bytes whose role is settled: a tag name up to
whitespace, an attribute value up to its known closing quote, a YAML line up
to its known end. Its branches are regular and well predicted, so the loop
stays fast. The unpredictable decisions happen once per bound, not once per
byte.

### What to index

Index a byte only if stage 2 has to **search** for it to find a bound.
Bytes that stage 2 reads in place, because it is already standing on them or
is already walking the bounded region that contains them, are not bounds.
Indexing them costs stage-1 time and index space, and stage 2 only steps
over them. Every case measured so far follows this rule:

| Grammar | Bytes | How stage 2 meets them | Indexed | Measured |
|---|---|---|---|---|
| HTML | `<` `>` `'` `"` | bounds of tags and quoted values | yes | |
| HTML | `=` | read right after an attribute name | no | 13–21% of tape entries on real pages; dropping it made stage 1 8–12% faster, events identical |
| YAML | `\n`, space, `'` `"` | bounds of lines, indentation, and quoted scalars | yes | |
| YAML | `-` | read at a line's first non-space byte | no | its mask was never read; dropping it made stage 1 9–15% faster on 4 of 5 inputs (no change on the fifth) |
| YAML | `:` `#` `[` `]` `{` `}` `,` | read while walking a line already bounded by `\n` | no | full parse 7–29% faster than indexing them as masks |

A byte can also be indexed in the wrong place. Putting YAML's `#` on a tape
made the full parse 17–24% slower than keeping it as a mask, because every
`key: value` line paid two tape lookups to rule out a comment. Reading it
in place while walking the line is cheaper than either.

### How to store it

Once the bounds are chosen, their density picks the representation:

- **Tape**, for sparse bounds. Stage 2 jumps from entry to entry. The cost
  is 4 bytes per bound, so it grows with density.
- **Masks**, for dense bounds such as YAML's spaces and line feeds. The cost
  is a fixed 8 bytes per class per 64-byte block, even for a block with
  nothing in it. `MaskView::run_length` measures indentation and
  `MaskView::next_in` finds the next line feed in a word operation or two.
- **Hybrid**, when a grammar has both: dense classes stay masks and sparse
  ones go to a tape. YAML uses this.

Masks are not automatically cheaper to build. Classification is identical
in every mode; only the output differs. For sparse bounds a tape writes less
memory than a fixed set of masks per block. HTML's tape is 6.16 GiB/s in
stage 1 versus 4.75 GiB/s for masks.

## 1. Table generation (`simdlex-tablegen`)

A grammar lists its byte classes and generates tables in a `const` item:

```rust
const CLASSES: [ByteSet; 4] = [
    ByteSet::from_bytes(b"<"),
    ByteSet::from_bytes(b">"),
    ByteSet::from_bytes(b"'"),
    ByteSet::from_bytes(b"\""),
];
const GENERATED: Tables<4> = unwrap(generate(&CLASSES));
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
of three modes, chosen at compile time inside the block loop:

| `IndexMode` | Storage | Cost | Use when |
|---|---|---|---|
| `Tape` | global `Vec<u32>` of offsets | 4 B per structural | delimiters are sparse single bytes (JSON, HTML tags) |
| `Masks` | per block: structural + each class + each exact mask | `8 × (1 + classes + exacts)` B per 64 B, fixed | significant bytes are dense (YAML indentation) |
| `Hybrid` | structural lanes on a tape, plus the first `HYBRID_MASK_CLASSES` class masks per block | 4 B per structural + `8 × classes` B per 64 B | a grammar has dense and sparse bounds (YAML: line feeds and spaces as masks, quotes on the tape) |

A spec declares its preferred `MODE`. `Parser::with_index_mode` overrides it
for experiments. Stage 2 receives an `IndexView`:
`tape()` / `masks()` give the raw representations (both, in hybrid mode),
`positions()` yields structural offsets from any mode, and
`MaskView::class_positions(c)` walks a single class lazily.

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
- YAML tree construction on top of the [event stream](yaml-stream.md), which
  uses `Hybrid` by default.
- Smarter tablegen search (overlapping rectangles) if a grammar needs it.
