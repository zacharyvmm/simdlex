# Stage 1: table generation and indexing

simdlex is stage 1 of a simdjson-style parser, packaged as a primitive. It
is two dependency-free crates, re-exported together by the `simdlex`
facade:

```text
crates/simdlex-tablegen   named byte sets ──► NibbleTables + class bits     (const fn, no_std)
crates/simdlex-index      NibbleTables + IndexSpec ──► tape (+ class masks)  (scalar/SSSE3/AVX2/NEON)
```

`simdlex-index` never sees the generator: tables are plain data, so
hand-written tables keep working. `simdlex-tablegen` never sees a backend.
Stage 2 is the user's: the grammars under `examples/` show three.

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

Once the bounds are chosen, their density picks the representation, per
class:

- **Tape**, for sparse bounds. Stage 2 jumps from entry to entry. The cost
  is 4 bytes per bound, so it grows with density.
- **Masks**, for dense bounds such as YAML's spaces and line feeds. The cost
  is a fixed 8 bytes per class per 64-byte block, even for a block with
  nothing in it. `MaskView::run_length` measures indentation and
  `MaskView::next_in` finds the next line feed in a word operation or two.

A grammar with both, like YAML, keeps its dense classes as masks and puts
the sparse ones on the tape (`IndexSpec::MASK_CLASSES`).

Masks are not automatically cheaper to build. Classification is identical
either way; only the output differs. For sparse bounds a tape writes less
memory than a fixed set of masks per block: when HTML's bounds were stored
both ways, the tape built at 6.16 GiB/s and the masks at 4.75 GiB/s. That
is why there is no masks-only mode.

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

An `IndexSpec` supplies tables, class selectors, how many leading classes to
keep as masks, and optionally a per-block hook:

```rust
fn scan_block(input: &[u8; 64], base: u32, valid: u64,
              masks: &mut ClassifiedBlock, state: &mut Self::State)
    -> Result<u64, Self::Error>;
```

The backend classifies a block, the hook refines the masks and returns the
structural lanes, which are flattened onto the tape. The default hook makes
every class after the mask classes structural; JSON overrides it to drop
bytes inside strings and to find scalar starts, carrying quote and escape
state across blocks.

The index is always a tape of `u32` offsets (4 B per structural), plus one
mask per block for each of the first `MASK_CLASSES` classes
(`8 × MASK_CLASSES` B per 64 B). A grammar with only sparse bounds (JSON,
HTML) keeps no masks; YAML keeps line feeds and spaces as masks and puts
quotes on the tape. Stage 2 walks them with:

- `TapeCursor`: `find(from, predicate)` returns the next structural at or
  after `from` whose byte matches and stays on it; `seek(offset)` gallops
  forward, then binary-searches, so a stage 2 that skips ahead without the
  tape stays cheap.
- `MaskView`: `next_in(classes, from)` finds the next lane in any of the
  chosen classes, `run_length(class, from)` measures a run (indentation),
  `class_positions(class)` iterates one class.

`Indexer::index` scans a whole input. `Indexer::index_window` scans one
window (aligned to 64 bytes) with state carried from the previous window, so
a parser that may stop early, such as one answering a `first` query, only
indexes as far as it reads. Windows produce exactly the tape of a whole scan.

Backends are chosen once per `Indexer`: AVX2 or SSSE3 by runtime detection on
x86-64, NEON on little-endian aarch64, scalar elsewhere (including
big-endian aarch64). Every backend is tested to produce identical masks for
all 256 byte values; the NEON backend is verified by running the core test
suite under Miri for `aarch64-unknown-linux-gnu`.

## HTML

HTML's bounds are `<`, `>`, `'`, and `"`. Whitespace, `/`, and `=` are not
structural: the tag walker reads names, whitespace, and the `=` that follows
an attribute name directly from the bytes between delimiters. Indexing `=`
was measured and rejected: it was 13–21% of structurals on real pages, stage
2 only ever skipped those entries, and dropping it made stage 1 8–12% faster
with identical events.

Unlike JSON strings, quotes cannot be masked in stage 1, because a quote
only opens a value inside a tag, and apostrophes in text would break
prefix-XOR parity. Quote pairing therefore stays in stage 2.

## Next steps

- Unrolled flattening (simdjson writes eight offsets at a time into padding).
- A UTF-8 validation primitive for grammars that need it; the JSON and YAML
  examples validate with the standard library before indexing.
- Smarter tablegen search (overlapping rectangles) if a grammar needs it.
