# HTML Class Extractor

This document describes the implementation added in `src/lib.rs`. The extractor follows the
design in `.docs/spec.md`: SIMD is used as a byte classifier to find HTML-relevant bytes quickly,
while the actual HTML-ish parsing stays scalar and stateful.

## Goal

The extractor finds values for normal HTML `class` attributes:

```html
<div class="text-red-500 font-bold"></div>
<span class='hover:bg-blue-500 p-4'></span>
<button class=rounded-lg></button>
```

It returns byte spans into the original input rather than allocating class strings. A separate
helper can split those spans into whitespace-delimited class tokens.

## Public API

### `ClassSpan`

```rust
pub struct ClassSpan {
    pub value_start: usize,
    pub value_end: usize,
}
```

`ClassSpan` is a half-open byte range, `[value_start, value_end)`, pointing at the contents of the
class attribute value. Quotes are excluded from the span.

For example:

```html
<div class="a b"></div>
```

returns a span covering only `a b`, not the surrounding `"`.

### `extract_class_spans`

```rust
pub fn extract_class_spans(input: &[u8]) -> Vec<ClassSpan>
```

This is the default entry point. On `x86_64`, it uses `RawSimdClassifier` when SSSE3 is available
at runtime and otherwise falls back to the scalar classifier. On other targets, the default
classifier is scalar.

### `extract_class_spans_with`

```rust
pub fn extract_class_spans_with<C: ByteClassifier>(input: &[u8]) -> Vec<ClassSpan>
```

This is the generic version used by tests and by callers that want to force a specific classifier.
It is useful for comparing SIMD and scalar behavior.

### `ClassToken` and `split_class_tokens`

```rust
pub struct ClassToken<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

pub fn split_class_tokens<'a>(source: &'a [u8], span: ClassSpan) -> Vec<ClassToken<'a>>
```

`split_class_tokens` splits one class value span by HTML ASCII whitespace: tab, newline, carriage
return, and space. Tokens keep their absolute byte offsets in the original input. Invalid UTF-8
tokens are skipped, because `ClassToken::text` is a `&str`.

## Classifier Contract

The parser depends on this trait:

```rust
pub trait ByteClassifier {
    const LANES: usize;

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses;

    fn lt_block_mask(input: &[u8], offset: usize) -> u64;
    fn gt_block_mask(input: &[u8], offset: usize) -> u64;
    fn tag_end_block_mask(input: &[u8], offset: usize) -> u64;
}
```

`classify_block` classifies exactly `LANES` bytes starting at `offset`. Callers ensure
`offset + LANES <= input.len()`. Returned masks use bit `i` for byte `input[offset + i]`.

The narrower mask helpers are optimization hooks:

- `lt_block_mask` is used by the top-level scan for `<`.
- `gt_block_mask` is used when skipping comments, doctypes, closing tags, and processing
  instructions.
- `tag_end_block_mask` returns only `"`, `'`, and `>` positions for tag-end parsing.

The default trait implementations can derive these from `classify_block`, but the x86_64 SIMD
backend overrides them to avoid building full byte-class results when the caller only needs a
single search mask.

The returned `ByteClasses` fields are:

```rust
pub struct ByteClasses {
    pub structural: u64,
    pub whitespace: u64,
    pub quote_double: u64,
    pub quote_single: u64,
    pub lt: u64,
    pub gt: u64,
    pub eq: u64,
}
```

The structural class is the union of:

- `"`
- `'`
- `<`
- `>`
- `=`

The whitespace class is the union of:

- `\t`
- `\n`
- `\r`
- space

The individual masks identify exact bytes.

## SIMD Tables

The raw SIMD classifier uses the nibble table technique from `.docs/backtracking_table_gen.rs`.
Each byte is split into low and high nibbles:

```rust
class_byte = TLO[byte & 0x0f] & THI[byte >> 4]
```

For this extractor, the generated target set is:

```rust
structural = b"\"'<>="
whitespace = b"\t\n\r "
```

The tables embedded in `src/lib.rs` are:

```rust
const TLO: [u8; 16] = [8, 0, 1, 0, 0, 0, 0, 1, 0, 4, 4, 0, 2, 6, 2, 0];
const THI: [u8; 16] = [4, 0, 9, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
```

The assigned bit domains are:

```rust
const STRUCTURAL_BITS: u8 = 1 | 2;
const WHITESPACE_BITS: u8 = 4 | 8;
```

One practical detail differs from the pseudocode in `.docs/spec.md`: several bytes intentionally
share generated table bits. For example, `"` and `'` are both structural quote-like bytes, and
`<`, `>`, and `=` are structural delimiter-like bytes. Because the parser needs exact masks for
`lt`, `gt`, `eq`, and each quote type, the SIMD backend uses the nibble table to classify broad
domains and uses SIMD equality comparisons for exact per-byte masks.

That keeps the table compact while preserving the precise `ByteClasses` API.

## Raw x86_64 SIMD Backend

`RawSimdClassifier` is compiled only on `x86_64`.

At runtime, it checks:

```rust
std::is_x86_feature_detected!("ssse3")
```

If SSSE3 is available, it calls `classify_block_ssse3`; otherwise it delegates to
`ScalarClassifier`.

The full SSSE3 classifier processes 16 bytes per block:

1. `_mm_loadu_si128` loads the input block.
2. `_mm_and_si128` and `_mm_srli_epi16` derive low and high nibbles.
3. `_mm_shuffle_epi8` looks up low-nibble and high-nibble classes from `TLO` and `THI`.
4. `_mm_and_si128` combines those classes into a broad class byte per lane.
5. `_mm_movemask_epi8` converts vector comparisons into bitmasks.

Broad `structural` and `whitespace` masks are produced by checking whether the relevant class bits
are non-zero. Exact masks for `"`, `'`, `<`, `>`, and `=` are produced by comparing the original
input vector against each target byte with `_mm_cmpeq_epi8`.

For narrower scans, the backend bypasses the nibble table:

- `<` scanning uses one `_mm_cmpeq_epi8` plus `_mm_movemask_epi8`.
- `>` skipping uses one `_mm_cmpeq_epi8` plus `_mm_movemask_epi8`.
- tag-end scanning uses three equality masks for `"`, `'`, and `>`.

This avoids the full table lookup and five exact-byte comparisons on paths that only need a subset
of the class information.

The unsafe surface is intentionally small:

- the public `ByteClassifier` implementation checks lane bounds with `debug_assert`
- SSSE3 execution is gated by runtime feature detection
- unaligned loads are used, so no input alignment is required

## Scalar Backend

`ScalarClassifier` also uses 16 lanes. It exists for:

- fallback on machines without SSSE3
- non-`x86_64` targets
- tests that compare scalar and SIMD output

It directly matches each byte in a block and sets the same `ByteClasses` masks as the SIMD backend.

## Parser Flow

The parser is intentionally scalar after byte classification.

### Top-level scan

`extract_class_spans_with` repeatedly calls `find_next_lt` to find the next `<`.

If the next byte after `<` is one of:

- `!`
- `/`
- `?`

the parser skips until the next `>` and does not parse the contents as a normal start tag. This
skips comments, doctypes, closing tags, and processing instructions.

Otherwise it finds the end of the tag with `find_tag_end`, parses attributes in the tag body, and
continues after the tag.

### Finding `<` and `>`

`find_next_lt` and `skip_until_gt` use the active classifier's narrow mask helpers in full 16-byte
blocks and fall back to scalar byte search for the tail of the input. On x86_64 with SSSE3, these
paths are single-byte SIMD equality scans rather than full byte classification.

### Finding tag end

`find_tag_end` uses the active classifier to process full 16-byte blocks. For each block it ORs
the double-quote, single-quote, and `>` masks, then walks only those interesting positions in byte
order. That keeps quote state correct while skipping over ordinary attribute text in whole SIMD
blocks.

Quote state is still scalar because `>` only ends a tag when the parser is not inside a
single-quoted or double-quoted attribute value.

This means:

```html
<div title="2 > 1" class="ok"></div>
```

correctly keeps scanning until the `>` after `"ok"`.

After the last full SIMD block, the remaining tail is scanned scalar with the current quote state.

### Attribute parsing

`parse_tag_for_class_attrs`:

1. skips the tag name
2. skips whitespace
3. reads an attribute name
4. skips whitespace
5. requires `=`
6. skips whitespace
7. parses the value
8. records the value span if the attribute name equals `class` ignoring ASCII case

Boolean attributes are ignored. Self-closing `/` is skipped.

Attribute values can be:

- double quoted: `class="a b"`
- single quoted: `class='a b'`
- unquoted: `class=a`

Unquoted values end at HTML ASCII whitespace or `>`.

If a quoted value is unterminated before the tag end, the value span extends to the computed tag
end.

## Example Binary

`src/main.rs` demonstrates the default API:

```rust
for span in simdlex::extract_class_spans(html) {
    let value = std::str::from_utf8(&html[span.value_start..span.value_end]).unwrap();
    println!("{value}");
}
```

For the current sample input, `cargo run` prints:

```text
text-red-500 font-bold
hover:bg-blue-500 p-4
rounded-lg
```

## Tests Added

The library tests cover:

- scalar classifier masks
- SIMD classifier masks matching scalar masks on `x86_64`
- double-quoted, single-quoted, and unquoted class extraction
- ASCII case-insensitive `CLASS`
- ignoring comments, closing tags, and non-`class` attributes
- `>` inside quoted attributes
- long quoted attributes whose `>` characters cross SIMD block boundaries
- class token splitting with absolute offsets

## Verification Commands

The implementation was verified with:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo run
```

## Current Limitations

This is an HTML attribute extractor, not a full HTML parser.

Notable limitations:

- It only extracts literal `class` attributes.
- It does not extract JSX `className`.
- It does not evaluate template syntax, JavaScript expressions, Vue `:class`, Svelte bindings, or
  other framework-specific forms.
- It does not decode HTML entities inside class values.
- It skips invalid UTF-8 tokens in `split_class_tokens`.
- Comment and doctype skipping is a simple skip-to-`>` strategy.

These are deliberate boundaries for the current implementation. The main extension point is the
`ByteClassifier` trait: additional SIMD backends can be added without changing the parser API.
