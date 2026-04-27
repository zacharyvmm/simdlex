I could access `zacharyvmm/simdlex`, but GitHub reports the repository size as `0` and code search is not indexed, so I could not inspect actual source files. Based on the SIMD table output you gave, here is a concrete architecture proposal inspired by `simdjson`, but adapted for HTML/class extraction.

# SIMD HTML lexer architecture proposal

## Goal

Build a two-stage HTML lexer inspired by `simdjson`:

1. **Stage 1: SIMD structural scan**

   * Scan the input in SIMD-width chunks.
   * Find important structural characters.
   * Track quote regions.
   * Handle backslashes for future JSX/TSX/string-literal support.
   * Produce a compact **structural index tape**.

2. **Stage 2: tape-driven lexer/parser**

   * Walk only the structural tape, not every byte.
   * Recognize tags, attributes, quoted values, comments, and raw text regions.
   * Extract `class` attribute values.
   * Later extend the same tape model to JSX, TSX, Vue, Svelte, etc.

This gives you a `simdjson`-style design: SIMD finds “interesting bytes”, then a scalar state machine interprets them.

---

# 1. Core idea

Instead of parsing HTML directly byte-by-byte, the scanner produces a tape like this:

```rust
pub struct StructuralTape {
    pub entries: Vec<StructuralEntry>,
}

#[derive(Debug, Clone, Copy)]
pub struct StructuralEntry {
    pub pos: u32,
    pub kind: StructuralKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralKind {
    Lt,          // <
    Gt,          // >
    Eq,          // =
    Quote,       // "
    Apostrophe,  // '
    Dash,        // -
    Whitespace,  // space, tab, newline, carriage return
    Backslash,   // \, mostly for JSX/TSX/script support later
}
```

For the input:

```html
<div class="foo bar" id="x">
```

Stage 1 produces something conceptually like:

```text
0:  Lt          '<'
4:  Whitespace  ' '
10: Eq          '='
11: Quote       '"'
19: Quote       '"'
23: Eq          '='
24: Quote       '"'
26: Quote       '"'
27: Gt          '>'
```

Then Stage 2 uses the original input plus the structural positions to interpret attributes.

The tape does **not** contain every byte. It only contains bytes that matter for parsing boundaries.

---

# 2. Why this fits your current SIMD table

Your generated masks are already close to a good Stage 1 scanner:

```text
Char   " (34) : Mask 1
Char   ' (39) : Mask 1
Char   < (60) : Mask 2
Char   > (62) : Mask 2
Char   - (45) : Mask 1
Char   = (61) : Mask 2
Char  \t (9) : Mask 4
Char  \n (10) : Mask 4
Char  \r (13) : Mask 4
Char ' ' (32) : Mask 8
```

I would split those semantic groups more clearly.

Current mask grouping:

```text
Mask 1: quote-ish / dash
Mask 2: structural HTML delimiters
Mask 4: tab/newline/carriage return
Mask 8: space
```

Better proposal:

```rust
pub const M_QUOTE:      u8 = 0b0000_0001; // " or '
pub const M_STRUCTURAL: u8 = 0b0000_0010; // < > =
pub const M_WS:         u8 = 0b0000_0100; // \t \n \r space
pub const M_DASH:       u8 = 0b0000_1000; // -
pub const M_BACKSLASH:  u8 = 0b0001_0000; // \
pub const M_SLASH:      u8 = 0b0010_0000; // /
pub const M_BANG:       u8 = 0b0100_0000; // !
pub const M_QUESTION:   u8 = 0b1000_0000; // ?
```

For HTML specifically, I would include at least:

```text
< > = " ' / ! ? - whitespace
```

Because those let you cheaply recognize:

```html
<tag>
</tag>
<tag attr="value">
<!-- comment -->
<!DOCTYPE html>
<?xml ...?>
```

For Tailwind/class scanning, you can keep the first version smaller:

```text
< > = " ' whitespace
```

But if you want a general HTML architecture, add `/`, `!`, `?`, and `-`.

---

# 3. Proposed module layout

```text
src/
  lib.rs

  tables/
    mod.rs
    backtracking_table_gen.rs
    html_tables.rs
    generic_tables.rs

  scan/
    mod.rs
    simd_stage1.rs
    scalar_fallback.rs
    masks.rs
    quote.rs
    tape.rs

  html/
    mod.rs
    lexer.rs
    attr.rs
    class_extractor.rs
    comments.rs
    raw_text.rs

  formats/
    mod.rs
    html.rs
    jsx.rs
    tsx.rs
    vue.rs
    svelte.rs
```

The important architectural split is:

```text
tables/     creates byte-classification lookup tables
scan/       produces the structural tape
html/       consumes the structural tape
formats/    future format-specific policies
```

---

# 4. Stage 1: SIMD structural scanner

## Input

```rust
pub struct ScanInput<'a> {
    pub bytes: &'a [u8],
}
```

## Output

```rust
pub struct Stage1Output {
    pub tape: StructuralTape,
    pub errors: Vec<ScanError>,
}
```

## Scanner API

```rust
pub trait StructuralScanner {
    fn scan(&self, input: &[u8]) -> Stage1Output;
}
```

Concrete implementations:

```rust
pub struct SimdScanner {
    tables: SimdTables,
}

pub struct ScalarScanner {
    tables: ScalarTables,
}
```

The scanner should use SIMD when available and fall back to scalar otherwise.

```rust
pub fn scan_structurals(input: &[u8], format: FormatKind) -> Stage1Output {
    #[cfg(any(target_arch = "x86_64", target_arch = "wasm32"))]
    {
        if simd_available() {
            return SimdScanner::for_format(format).scan(input);
        }
    }

    ScalarScanner::for_format(format).scan(input)
}
```

---

# 5. SIMD block scan

Each SIMD block should produce multiple bitmasks:

```rust
pub struct BlockMasks {
    pub quote: u64,
    pub apostrophe: u64,
    pub lt: u64,
    pub gt: u64,
    pub eq: u64,
    pub slash: u64,
    pub dash: u64,
    pub bang: u64,
    pub question: u64,
    pub whitespace: u64,
    pub backslash: u64,
}
```

If you scan 64 bytes at a time, each bit in the mask corresponds to one byte.

```text
bit 0  => input[base + 0]
bit 1  => input[base + 1]
...
bit 63 => input[base + 63]
```

Then combine them:

```rust
let structural =
    masks.lt
  | masks.gt
  | masks.eq
  | masks.quote
  | masks.apostrophe
  | masks.slash
  | masks.dash
  | masks.bang
  | masks.question
  | masks.whitespace
  | masks.backslash;
```

Then iterate set bits:

```rust
let mut bits = structural;

while bits != 0 {
    let bit = bits.trailing_zeros();
    let pos = base + bit as usize;

    let kind = classify_structural_byte(input[pos]);

    tape.entries.push(StructuralEntry {
        pos: pos as u32,
        kind,
    });

    bits &= bits - 1;
}
```

That is the `simdjson`-style design: SIMD identifies important positions, scalar code appends index entries.

---

# 6. Quote handling

HTML needs quote awareness, but less aggressively than JSON.

In JSON, quote handling is central because structural characters inside strings must be ignored.

For HTML, this matters inside attribute values:

```html
<div class="foo > bar">
```

The `>` inside the string must **not** close the tag.

So Stage 1 should either:

### Option A: Keep all structurals and let Stage 2 interpret quotes

This is simpler.

Tape includes:

```text
< class = " foo > bar " >
```

Stage 2 tracks whether it is inside a quoted attribute value and ignores `>` until the closing quote.

This is easier and probably best for your first architecture.

### Option B: Stage 1 masks structurals inside quotes

This is closer to `simdjson`.

Stage 1 computes quote masks and removes structurals that occur inside quote regions:

```rust
let inside_quotes = compute_quote_mask(masks.quote, masks.backslash);
let real_structural = structural & !inside_quotes;
```

But HTML has both `'` and `"`, and quotes are only quote delimiters inside tag/attribute context. In raw text, a quote is just text.

Because of that, I recommend **Option A for HTML**:

```text
Stage 1 is lexical/indexing only.
Stage 2 decides context.
```

For JSX/TSX later, you can add a context-aware Stage 2 that treats JS string/template literals differently.

---

# 7. Backslash handling

Backslashes are not very meaningful in normal HTML attributes:

```html
<div class="foo\"bar">
```

HTML does not use C-style escaping for attributes. The backslash is just a character.

But backslash matters a lot for future targets:

```jsx
<div className="foo \"bar\"" />
```

or:

```tsx
const x = "<div class=\"foo\">";
```

So I would include `\` in the tape, but not let the HTML parser interpret it as escape syntax.

```rust
pub enum EscapePolicy {
    NoneHtml,
    JsString,
    CssString,
}
```

For HTML:

```rust
EscapePolicy::NoneHtml
```

For JSX/TSX:

```rust
EscapePolicy::JsString
```

The structural scanner can still record backslashes so future format parsers can compute escaped quote masks.

---

# 8. Stage 2: tape-driven HTML lexer

Stage 2 walks the tape and emits higher-level events.

```rust
pub enum HtmlEvent<'a> {
    StartTag {
        name: &'a [u8],
        attrs: Vec<Attribute<'a>>,
        span: Span,
    },
    EndTag {
        name: &'a [u8],
        span: Span,
    },
    Comment {
        span: Span,
    },
    Doctype {
        span: Span,
    },
    Text {
        span: Span,
    },
}
```

But for Tailwind-like extraction, you do not need to fully materialize events. You can specialize:

```rust
pub struct ClassCandidate<'a> {
    pub value: &'a [u8],
    pub span: Span,
}
```

Then:

```rust
pub fn extract_html_classes<'a>(
    input: &'a [u8],
    tape: &'a StructuralTape,
) -> Vec<ClassCandidate<'a>>;
```

---

# 9. HTML tag parser over tape

The core parser is a state machine.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlState {
    Data,
    TagOpen,
    StartTag,
    EndTag,
    BeforeAttrName,
    AttrName,
    BeforeAttrValue,
    AttrValueDoubleQuoted,
    AttrValueSingleQuoted,
    AttrValueUnquoted,
    Comment,
    RawText,
}
```

The important logic:

```rust
match state {
    HtmlState::Data => {
        if entry.kind == StructuralKind::Lt {
            state = HtmlState::TagOpen;
        }
    }

    HtmlState::TagOpen => {
        // Look at bytes after '<'
        // '/', '!', '?' determine end tag, comment, doctype, processing instruction.
    }

    HtmlState::StartTag => {
        // Find whitespace or '>' to end tag name.
        // Then parse attributes.
    }

    HtmlState::BeforeAttrName => {
        // Skip whitespace.
        // Stop on '>'.
        // Otherwise start attr name.
    }

    HtmlState::AttrName => {
        // Attribute name extends until whitespace, '=', '/', or '>'.
    }

    HtmlState::BeforeAttrValue => {
        // Skip whitespace.
        // If quote, enter quoted value.
        // Otherwise parse unquoted until whitespace or '>'.
    }

    HtmlState::AttrValueDoubleQuoted => {
        // Value extends until next real '"'.
        // '>' is ignored here.
    }

    HtmlState::AttrValueSingleQuoted => {
        // Value extends until next real '\''.
        // '>' is ignored here.
    }

    HtmlState::AttrValueUnquoted => {
        // Value extends until whitespace or '>'.
    }

    _ => {}
}
```

For class extraction, when the parser sees:

```html
class = "..."
```

it emits the value span.

---

# 10. Class attribute matching

Do not allocate strings for attribute names. Compare slices case-insensitively.

```rust
fn is_class_attr(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"class")
}
```

For JSX later:

```rust
fn is_class_like_attr(name: &[u8], format: FormatKind) -> bool {
    match format {
        FormatKind::Html => name.eq_ignore_ascii_case(b"class"),
        FormatKind::Jsx | FormatKind::Tsx => {
            name == b"className" || name == b"class"
        }
        FormatKind::Vue | FormatKind::Svelte => {
            name == b"class" || name == b":class" || name == b"className"
        }
    }
}
```

---

# 11. Suggested tape representation

For performance, avoid storing a full enum per entry if you want a compact index tape.

A simdjson-like tape could use parallel arrays:

```rust
pub struct StructuralTape {
    pub positions: Vec<u32>,
    pub kinds: Vec<u8>,
}
```

Where kinds are bit flags:

```rust
pub mod kind {
    pub const QUOTE: u8      = 1;
    pub const APOSTROPHE: u8 = 2;
    pub const LT: u8         = 3;
    pub const GT: u8         = 4;
    pub const EQ: u8         = 5;
    pub const SLASH: u8      = 6;
    pub const DASH: u8       = 7;
    pub const BANG: u8       = 8;
    pub const QUESTION: u8   = 9;
    pub const WS: u8         = 10;
    pub const BACKSLASH: u8  = 11;
}
```

This is cache-friendly:

```rust
for i in 0..tape.positions.len() {
    let pos = tape.positions[i];
    let kind = tape.kinds[i];
}
```

A more ergonomic version:

```rust
pub struct StructuralTape {
    entries: Vec<StructuralEntry>,
}
```

I would start with `Vec<StructuralEntry>` for correctness, then switch to SoA later if profiling shows it matters.

---

# 12. Recommended architecture diagram

```text
                 ┌────────────────────┐
                 │     input bytes     │
                 └─────────┬──────────┘
                           │
                           ▼
                 ┌────────────────────┐
                 │ Stage 1 SIMD scan   │
                 │ - classify bytes    │
                 │ - find structurals  │
                 │ - record quotes     │
                 │ - record backslash  │
                 └─────────┬──────────┘
                           │
                           ▼
                 ┌────────────────────┐
                 │ structural tape     │
                 │ positions + kinds   │
                 └─────────┬──────────┘
                           │
                           ▼
                 ┌────────────────────┐
                 │ Stage 2 HTML parser │
                 │ - tag state machine │
                 │ - attr boundaries   │
                 │ - quote context     │
                 └─────────┬──────────┘
                           │
                           ▼
                 ┌────────────────────┐
                 │ class candidates    │
                 │ spans into input    │
                 └────────────────────┘
```

---

# 13. Concrete Rust sketch

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Html,
    Jsx,
    Tsx,
    Vue,
    Svelte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralKind {
    Quote,
    Apostrophe,
    Lt,
    Gt,
    Eq,
    Slash,
    Dash,
    Bang,
    Question,
    Whitespace,
    Backslash,
}

#[derive(Debug, Clone, Copy)]
pub struct StructuralEntry {
    pub pos: u32,
    pub kind: StructuralKind,
}

#[derive(Debug, Default)]
pub struct StructuralTape {
    pub entries: Vec<StructuralEntry>,
}

#[derive(Debug)]
pub struct Stage1Output {
    pub tape: StructuralTape,
}

#[derive(Debug)]
pub struct ClassCandidate<'a> {
    pub raw: &'a [u8],
    pub span: Span,
}

pub fn extract_classes<'a>(
    input: &'a [u8],
    format: FormatKind,
) -> Vec<ClassCandidate<'a>> {
    let stage1 = scan_structurals(input, format);

    match format {
        FormatKind::Html => extract_html_classes(input, &stage1.tape),
        FormatKind::Jsx | FormatKind::Tsx => {
            todo!("JSX/TSX parser over same structural tape")
        }
        FormatKind::Vue | FormatKind::Svelte => {
            todo!("Vue/Svelte parser over same structural tape")
        }
    }
}
```

---

# 14. HTML class extractor sketch

This is simplified, but shows the intended model:

```rust
pub fn extract_html_classes<'a>(
    input: &'a [u8],
    tape: &StructuralTape,
) -> Vec<ClassCandidate<'a>> {
    let mut out = Vec::new();

    let mut i = 0;

    while i < tape.entries.len() {
        let entry = tape.entries[i];

        if entry.kind != StructuralKind::Lt {
            i += 1;
            continue;
        }

        let tag_start = entry.pos as usize;

        // Skip comments, doctypes, closing tags.
        if input.get(tag_start + 1) == Some(&b'/')
            || input.get(tag_start + 1) == Some(&b'!')
            || input.get(tag_start + 1) == Some(&b'?')
        {
            i += 1;
            continue;
        }

        // Find matching '>' while respecting quoted attr values.
        let Some((tag_end_i, tag_end_pos)) = find_tag_end(input, tape, i + 1) else {
            break;
        };

        extract_classes_in_tag(
            input,
            tag_start,
            tag_end_pos as usize,
            &mut out,
        );

        i = tag_end_i + 1;
    }

    out
}
```

The simplest version of `extract_classes_in_tag` can be scalar. That is still fast because Stage 1 has already narrowed the parsing to tag regions.

Later, you can make `extract_classes_in_tag` use the tape too.

---

# 15. Tag end detection

This is the key correctness point.

```rust
fn find_tag_end(
    input: &[u8],
    tape: &StructuralTape,
    mut i: usize,
) -> Option<(usize, u32)> {
    let mut quote: Option<u8> = None;

    while i < tape.entries.len() {
        let e = tape.entries[i];
        let b = input[e.pos as usize];

        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }

            None => match b {
                b'"' | b'\'' => {
                    quote = Some(b);
                }
                b'>' => {
                    return Some((i, e.pos));
                }
                _ => {}
            },
        }

        i += 1;
    }

    None
}
```

This correctly handles:

```html
<div class="foo > bar">
```

The `>` inside the quotes is ignored.

---

# 16. Attribute parser strategy

Once you have a tag range:

```text
<tag attr=value class="foo bar">
 ^                       ^
 tag_start               tag_end
```

You can parse attributes with a tight scalar loop.

That is acceptable because:

1. You only parse inside tags.
2. Most HTML bytes are text, not tag syntax.
3. Class extraction is dominated by finding tags and attribute boundaries.

Pseudo-code:

```rust
fn extract_classes_in_tag<'a>(
    input: &'a [u8],
    tag_start: usize,
    tag_end: usize,
    out: &mut Vec<ClassCandidate<'a>>,
) {
    let mut p = tag_start + 1;

    // Skip tag name.
    while p < tag_end && !is_html_ws(input[p]) && input[p] != b'/' {
        p += 1;
    }

    while p < tag_end {
        skip_ws(input, &mut p, tag_end);

        if p >= tag_end || input[p] == b'/' {
            break;
        }

        let name_start = p;

        while p < tag_end
            && !is_html_ws(input[p])
            && input[p] != b'='
            && input[p] != b'/'
        {
            p += 1;
        }

        let name_end = p;
        skip_ws(input, &mut p, tag_end);

        if p >= tag_end || input[p] != b'=' {
            continue;
        }

        p += 1;
        skip_ws(input, &mut p, tag_end);

        let value_start;
        let value_end;

        if p < tag_end && (input[p] == b'"' || input[p] == b'\'') {
            let quote = input[p];
            p += 1;
            value_start = p;

            while p < tag_end && input[p] != quote {
                p += 1;
            }

            value_end = p;

            if p < tag_end {
                p += 1;
            }
        } else {
            value_start = p;

            while p < tag_end && !is_html_ws(input[p]) {
                p += 1;
            }

            value_end = p;
        }

        if input[name_start..name_end].eq_ignore_ascii_case(b"class") {
            out.push(ClassCandidate {
                raw: &input[value_start..value_end],
                span: Span {
                    start: value_start as u32,
                    end: value_end as u32,
                },
            });
        }
    }
}
```

This gives you fast class extraction without trying to fully implement the HTML spec in the first version.

---

# 17. Important design decision: index tape should preserve whitespace

For JSON, whitespace is usually skipped. For HTML attributes, whitespace is structurally meaningful:

```html
<div class="a" id="b">
```

Whitespace separates the tag name and attributes.

So unlike `simdjson`, your structural tape should probably include whitespace, at least inside tags.

You have two options:

## Option A: Always tape whitespace

Simple and good for now.

```text
Tape contains <, >, =, quotes, whitespace, etc.
```

## Option B: Only tape whitespace inside tags

Faster and smaller, but requires quote/tag context during Stage 1.

I recommend Option A first.

You can later optimize:

```rust
let maybe_useful_ws = whitespace_mask & inside_tag_mask;
```

But computing `inside_tag_mask` accurately with quotes is more complicated.

---

# 18. Generic format abstraction

Since your future goal includes HTML, JSX, TSX, Svelte, and Vue, make the scanner generic but the parser format-specific.

```rust
pub trait FormatSpec {
    fn structural_bytes() -> &'static [u8];
    fn whitespace_bytes() -> &'static [u8];
    fn escape_policy() -> EscapePolicy;
    fn class_attribute_names() -> &'static [&'static [u8]];
}
```

Example:

```rust
pub struct HtmlSpec;

impl FormatSpec for HtmlSpec {
    fn structural_bytes() -> &'static [u8] {
        b"<>=\"'/!?-\\"
    }

    fn whitespace_bytes() -> &'static [u8] {
        b" \t\n\r"
    }

    fn escape_policy() -> EscapePolicy {
        EscapePolicy::NoneHtml
    }

    fn class_attribute_names() -> &'static [&'static [u8]] {
        &[b"class"]
    }
}
```

JSX:

```rust
pub struct JsxSpec;

impl FormatSpec for JsxSpec {
    fn structural_bytes() -> &'static [u8] {
        b"<>={}()[]\"'`/!?-\\:"
    }

    fn whitespace_bytes() -> &'static [u8] {
        b" \t\n\r"
    }

    fn escape_policy() -> EscapePolicy {
        EscapePolicy::JsString
    }

    fn class_attribute_names() -> &'static [&'static [u8]] {
        &[b"class", b"className"]
    }
}
```

The scanner can reuse the same table generator. Only the format spec changes.

---

# 19. Proposed pipeline for Tailwind-like extraction

```rust
pub fn scan_file_for_classes(path: &Path, input: &[u8]) -> Vec<ClassCandidate<'_>> {
    let format = detect_format(path);

    let stage1 = scan_structurals(input, format);

    match format {
        FormatKind::Html => extract_html_classes(input, &stage1.tape),
        FormatKind::Jsx => extract_jsx_classes(input, &stage1.tape),
        FormatKind::Tsx => extract_tsx_classes(input, &stage1.tape),
        FormatKind::Vue => extract_vue_classes(input, &stage1.tape),
        FormatKind::Svelte => extract_svelte_classes(input, &stage1.tape),
    }
}
```

Then class tokenization is a separate phase:

```rust
pub fn split_class_tokens(value: &[u8]) -> impl Iterator<Item = &[u8]> {
    value
        .split(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .filter(|token| !token.is_empty())
}
```

Example:

```html
<div class="flex items-center text-red-500">
```

produces:

```text
flex
items-center
text-red-500
```

---

# 20. Suggested `README` architecture section

You could add something like this to the repo:

```md
# simdlex architecture

simdlex uses a two-stage lexer architecture inspired by simdjson.

## Stage 1: SIMD structural indexing

The input file is scanned in SIMD-width blocks. A generated nibble lookup table classifies bytes into structural groups such as:

- tag delimiters: `<`, `>`
- attribute assignment: `=`
- quotes: `"`, `'`
- whitespace: ` `, `\t`, `\n`, `\r`
- optional syntax markers: `/`, `!`, `?`, `-`, `\`

The output of this stage is a structural index tape: a compact list of byte offsets and structural kinds. This stage does not fully parse HTML. It only records the interesting byte positions needed by later parsers.

## Stage 2: Tape-driven parsing

A format-specific parser walks the structural tape and interprets the original input using a small state machine. For HTML, this parser recognizes tags, attributes, quoted values, comments, doctypes, and raw-text regions.

For Tailwind-like class extraction, the parser only needs to identify `class` attributes and return spans into the original input. No DOM is built.

## Why this design?

Traditional lexers inspect every byte with a scalar state machine. simdlex instead uses SIMD to quickly locate candidate syntax boundaries, then runs scalar logic only around those boundaries. This makes the scanner reusable across multiple file formats such as HTML, JSX, TSX, Vue, and Svelte.
```

---

# 21. Recommended implementation order

I would implement it in this order:

1. **Define `StructuralTape`**

   * `Vec<StructuralEntry>` first.
   * Optimize later.

2. **Implement scalar Stage 1**

   * Easier to test.
   * Same output format as SIMD version.

3. **Implement SIMD Stage 1**

   * Use your generated lookup tables.
   * Match scalar output exactly.

4. **Implement `find_tag_end`**

   * Correctly ignores `>` inside quotes.

5. **Implement HTML class extraction**

   * Start scalar within tag ranges.
   * Return spans into original input.

6. **Add tests**

   * Quoted class.
   * Single-quoted class.
   * Unquoted class.
   * `>` inside class value.
   * Comments.
   * Closing tags.
   * Multiple tags.
   * Malformed/incomplete tags.

7. **Add JSX later**

   * Reuse tape.
   * Add JS string/template handling.

---

# 22. Test cases you should include

```rust
#[test]
fn extracts_double_quoted_class() {
    let input = br#"<div class="foo bar"></div>"#;
    assert_classes(input, &["foo bar"]);
}

#[test]
fn extracts_single_quoted_class() {
    let input = br#"<div class='foo bar'></div>"#;
    assert_classes(input, &["foo bar"]);
}

#[test]
fn extracts_unquoted_class() {
    let input = br#"<div class=foo></div>"#;
    assert_classes(input, &["foo"]);
}

#[test]
fn ignores_gt_inside_class_value() {
    let input = br#"<div class="foo > bar"></div>"#;
    assert_classes(input, &["foo > bar"]);
}

#[test]
fn ignores_closing_tags() {
    let input = br#"</div>"#;
    assert_classes(input, &[]);
}

#[test]
fn ignores_comments() {
    let input = br#"<!-- <div class="hidden"> -->"#;
    assert_classes(input, &[]);
}

#[test]
fn handles_multiple_classes() {
    let input = br#"
        <div class="a b"></div>
        <span class='c d'></span>
    "#;

    assert_classes(input, &["a b", "c d"]);
}
```

---

# Main recommendation

Use this architecture:

```text
SIMD scanner:
    bytes -> masks -> structural tape

HTML parser:
    structural tape + original bytes -> tag/attribute spans

Class extractor:
    attribute spans -> class candidates -> class tokens
```

Do **not** try to fully parse HTML inside the SIMD scanner. Keep SIMD responsible for discovering important byte positions. Keep context-sensitive HTML rules in Stage 2.

That gives you a clean `simdjson`-inspired design while still respecting the fact that HTML is not JSON and quote handling depends on context.
