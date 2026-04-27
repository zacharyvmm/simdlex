Below is a practical design for a **SIMD-assisted HTML attribute lexer** that extracts `class="..."`, `class='...'`, and unquoted `class=foo` values. It uses your nibble-table idea as the SIMD byte classifier, but keeps the parsing state machine scalar.

That is the right split:

```text
SIMD layer:
  scan 16/32/64 bytes at a time
  classify bytes: < > = " ' whitespace
  return bitmasks of interesting byte positions

Lexer layer:
  walk only interesting positions
  maintain HTML-ish state
  when inside a tag, parse attributes
  when attr name == class, extract value
```

Rust’s standard portable SIMD API is still nightly-only under `portable_simd`, so for a production crate today I would hide SIMD behind a backend trait and provide scalar, x86 SSSE3, wasm SIMD, and AArch64 NEON backends later. `std::simd` is documented as nightly-only/experimental, while SIMD itself maps naturally to lane-wise operations over vectors. ([Rust Documentation][1])

---

## 1. Core API

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassSpan {
    pub value_start: usize,
    pub value_end: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ByteClasses {
    pub structural: u64,
    pub whitespace: u64,

    pub quote_double: u64,
    pub quote_single: u64,
    pub lt: u64,
    pub gt: u64,
    pub eq: u64,
}

pub trait ByteClassifier {
    const LANES: usize;

    /// Classifies `input[offset..offset + LANES]`.
    ///
    /// The returned masks use bit `i` for byte `offset + i`.
    fn classify_block(input: &[u8], offset: usize) -> ByteClasses;
}
```

This lets the parser stay independent from the actual SIMD implementation.

---

## 2. Use your generated tables

For extracting `class`, the structural bytes you need are:

```rust
pub const STRUCTURAL: &[u8] = b"\"'<>=";
pub const WHITESPACE: &[u8] = b"\t\n\r ";
```

You do **not** need `-` for normal HTML class extraction. You would add `-`, `{`, `}`, `[`, `]`, `` ` ``, etc. later for JSX/TSX/Svelte/Vue frontends.

For example:

```rust
#[derive(Debug, Clone)]
pub struct SimdMapping {
    pub tlo: [u8; 16],
    pub thi: [u8; 16],
    pub masks: Vec<(u8, u8)>,
}

#[derive(Debug, Clone, Copy)]
pub struct HtmlMasks {
    pub double_quote: u8,
    pub single_quote: u8,
    pub lt: u8,
    pub gt: u8,
    pub eq: u8,
    pub whitespace: u8,
}

impl HtmlMasks {
    pub fn from_mapping(mapping: &SimdMapping) -> Self {
        let mut out = HtmlMasks {
            double_quote: 0,
            single_quote: 0,
            lt: 0,
            gt: 0,
            eq: 0,
            whitespace: 0,
        };

        for &(c, mask) in &mapping.masks {
            match c {
                b'"' => out.double_quote = mask,
                b'\'' => out.single_quote = mask,
                b'<' => out.lt = mask,
                b'>' => out.gt = mask,
                b'=' => out.eq = mask,
                b'\t' | b'\n' | b'\r' | b' ' => out.whitespace |= mask,
                _ => {}
            }
        }

        out
    }
}
```

The lookup formula is:

```rust
class_byte = tlo[byte & 0x0f] & thi[byte >> 4]
```

If the byte is not one of your target bytes, the generated tables ensure the result is `0`.

---

## 3. Scalar backend first

This is useful because your lexer can be fully tested before writing architecture-specific SIMD.

```rust
pub struct ScalarClassifier;

impl ByteClassifier for ScalarClassifier {
    const LANES: usize = 64;

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses {
        let mut out = ByteClasses {
            structural: 0,
            whitespace: 0,
            quote_double: 0,
            quote_single: 0,
            lt: 0,
            gt: 0,
            eq: 0,
        };

        let end = usize::min(offset + Self::LANES, input.len());

        for i in offset..end {
            let bit = 1u64 << (i - offset);

            match input[i] {
                b'"' => {
                    out.structural |= bit;
                    out.quote_double |= bit;
                }
                b'\'' => {
                    out.structural |= bit;
                    out.quote_single |= bit;
                }
                b'<' => {
                    out.structural |= bit;
                    out.lt |= bit;
                }
                b'>' => {
                    out.structural |= bit;
                    out.gt |= bit;
                }
                b'=' => {
                    out.structural |= bit;
                    out.eq |= bit;
                }
                b'\t' | b'\n' | b'\r' | b' ' => {
                    out.whitespace |= bit;
                }
                _ => {}
            }
        }

        out
    }
}
```

---

## 4. SIMD backend shape

For SSSE3/x86, the real classifier looks like this conceptually:

```rust
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod x86_ssse3 {
    use super::ByteClasses;

    #[cfg(target_arch = "x86")]
    use core::arch::x86::*;

    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::*;

    pub struct Ssse3Classifier;

    impl Ssse3Classifier {
        pub const LANES: usize = 16;

        #[target_feature(enable = "ssse3")]
        pub unsafe fn classify_block_with_tables(
            input: &[u8],
            offset: usize,
            tlo: [u8; 16],
            thi: [u8; 16],
            mask_double_quote: u8,
            mask_single_quote: u8,
            mask_lt: u8,
            mask_gt: u8,
            mask_eq: u8,
            mask_whitespace: u8,
        ) -> ByteClasses {
            debug_assert!(offset + 16 <= input.len());

            let bytes = _mm_loadu_si128(input.as_ptr().add(offset) as *const __m128i);

            let lo_mask = _mm_set1_epi8(0x0f);
            let lo = _mm_and_si128(bytes, lo_mask);
            let hi = _mm_and_si128(_mm_srli_epi16(bytes, 4), lo_mask);

            let tlo_v = _mm_loadu_si128(tlo.as_ptr() as *const __m128i);
            let thi_v = _mm_loadu_si128(thi.as_ptr() as *const __m128i);

            let lo_class = _mm_shuffle_epi8(tlo_v, lo);
            let hi_class = _mm_shuffle_epi8(thi_v, hi);
            let class = _mm_and_si128(lo_class, hi_class);

            unsafe fn bitmask_for(class: __m128i, bit: u8) -> u64 {
                let wanted = _mm_set1_epi8(bit as i8);
                let anded = _mm_and_si128(class, wanted);
                let cmp = _mm_cmpeq_epi8(anded, wanted);
                _mm_movemask_epi8(cmp) as u64
            }

            let quote_double = bitmask_for(class, mask_double_quote);
            let quote_single = bitmask_for(class, mask_single_quote);
            let lt = bitmask_for(class, mask_lt);
            let gt = bitmask_for(class, mask_gt);
            let eq = bitmask_for(class, mask_eq);
            let whitespace = bitmask_for(class, mask_whitespace);

            ByteClasses {
                structural: quote_double | quote_single | lt | gt | eq,
                whitespace,
                quote_double,
                quote_single,
                lt,
                gt,
                eq,
            }
        }
    }
}
```

For Wasm SIMD, the same idea maps to `u8x16.swizzle`. For AArch64 NEON, it maps to table lookup instructions. The important part is that your parser only depends on `ByteClassifier`, not on `_mm_shuffle_epi8`.

---

## 5. HTML `class` lexer

This version uses the classifier to quickly skip to `<` and `>` positions, then parses attributes inside each tag.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassSpan {
    pub value_start: usize,
    pub value_end: usize,
}

pub fn extract_class_spans<C: ByteClassifier>(input: &[u8]) -> Vec<ClassSpan> {
    let mut spans = Vec::new();
    let mut pos = 0;

    while pos < input.len() {
        match find_next_lt::<C>(input, pos) {
            Some(tag_start) => {
                pos = tag_start + 1;

                // Skip comments, doctypes, closing tags, processing instructions.
                if matches!(input.get(pos), Some(b'!' | b'/' | b'?')) {
                    pos = skip_until_gt::<C>(input, pos);
                    continue;
                }

                let tag_end = find_tag_end::<C>(input, pos);
                parse_tag_for_class_attrs(input, pos, tag_end, &mut spans);
                pos = tag_end.saturating_add(1);
            }
            None => break,
        }
    }

    spans
}

fn find_next_lt<C: ByteClassifier>(input: &[u8], mut pos: usize) -> Option<usize> {
    while pos + C::LANES <= input.len() {
        let classes = C::classify_block(input, pos);

        if classes.lt != 0 {
            return Some(pos + classes.lt.trailing_zeros() as usize);
        }

        pos += C::LANES;
    }

    input[pos..]
        .iter()
        .position(|&b| b == b'<')
        .map(|i| pos + i)
}

fn skip_until_gt<C: ByteClassifier>(input: &[u8], mut pos: usize) -> usize {
    while pos + C::LANES <= input.len() {
        let classes = C::classify_block(input, pos);

        if classes.gt != 0 {
            return pos + classes.gt.trailing_zeros() as usize;
        }

        pos += C::LANES;
    }

    input[pos..]
        .iter()
        .position(|&b| b == b'>')
        .map(|i| pos + i)
        .unwrap_or(input.len())
}

fn find_tag_end<C: ByteClassifier>(input: &[u8], mut pos: usize) -> usize {
    let mut quote: Option<u8> = None;

    while pos < input.len() {
        let b = input[pos];

        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return pos,
                _ => {}
            },
        }

        pos += 1;
    }

    input.len()
}
```

`find_tag_end` is scalar here because quotes matter. You can optimize it later using quote/`>` masks, but this version is correct enough to build on.

---

## 6. Attribute parser for one tag

```rust
fn parse_tag_for_class_attrs(
    input: &[u8],
    mut pos: usize,
    tag_end: usize,
    spans: &mut Vec<ClassSpan>,
) {
    // Skip tag name.
    pos = skip_name(input, pos, tag_end);

    while pos < tag_end {
        pos = skip_ws(input, pos, tag_end);

        if pos >= tag_end {
            break;
        }

        // Self-closing slash.
        if input[pos] == b'/' {
            pos += 1;
            continue;
        }

        let attr_name_start = pos;
        let attr_name_end = skip_attr_name(input, pos, tag_end);

        if attr_name_start == attr_name_end {
            pos += 1;
            continue;
        }

        pos = attr_name_end;
        pos = skip_ws(input, pos, tag_end);

        if pos >= tag_end || input[pos] != b'=' {
            // Boolean attribute.
            continue;
        }

        pos += 1;
        pos = skip_ws(input, pos, tag_end);

        if pos >= tag_end {
            break;
        }

        let is_class = ascii_eq_ignore_case(&input[attr_name_start..attr_name_end], b"class");

        let (value_start, value_end, next_pos) = parse_attr_value(input, pos, tag_end);

        if is_class {
            spans.push(ClassSpan {
                value_start,
                value_end,
            });
        }

        pos = next_pos;
    }
}

fn parse_attr_value(input: &[u8], pos: usize, tag_end: usize) -> (usize, usize, usize) {
    match input[pos] {
        b'"' | b'\'' => {
            let quote = input[pos];
            let value_start = pos + 1;
            let mut i = value_start;

            while i < tag_end {
                if input[i] == quote {
                    return (value_start, i, i + 1);
                }

                i += 1;
            }

            // Unterminated quoted value.
            (value_start, tag_end, tag_end)
        }
        _ => {
            let value_start = pos;
            let mut i = pos;

            while i < tag_end {
                let b = input[i];

                if is_html_ws(b) || b == b'>' {
                    break;
                }

                i += 1;
            }

            (value_start, i, i)
        }
    }
}

fn skip_name(input: &[u8], mut pos: usize, end: usize) -> usize {
    while pos < end {
        let b = input[pos];

        if is_html_ws(b) || b == b'/' || b == b'>' {
            break;
        }

        pos += 1;
    }

    pos
}

fn skip_attr_name(input: &[u8], mut pos: usize, end: usize) -> usize {
    while pos < end {
        let b = input[pos];

        if is_html_ws(b) || b == b'=' || b == b'/' || b == b'>' {
            break;
        }

        pos += 1;
    }

    pos
}

fn skip_ws(input: &[u8], mut pos: usize, end: usize) -> usize {
    while pos < end && is_html_ws(input[pos]) {
        pos += 1;
    }

    pos
}

#[inline]
fn is_html_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\r' | b' ')
}

fn ascii_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(&x, &y)| x.to_ascii_lowercase() == y)
}
```

---

## 7. Example usage

```rust
fn main() {
    let html = br#"
        <div class="text-red-500 font-bold">
            <span id='x' class='hover:bg-blue-500 p-4'></span>
            <button class=rounded-lg></button>
        </div>
    "#;

    let spans = extract_class_spans::<ScalarClassifier>(html);

    for span in spans {
        let value = std::str::from_utf8(&html[span.value_start..span.value_end]).unwrap();
        println!("{value}");
    }
}
```

Output:

```text
text-red-500 font-bold
hover:bg-blue-500 p-4
rounded-lg
```

---

## 8. Turning class values into Tailwind-like tokens

Once you have the class attribute value span, you can split it by ASCII whitespace:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassToken<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

pub fn split_class_tokens<'a>(source: &'a [u8], span: ClassSpan) -> Vec<ClassToken<'a>> {
    let mut out = Vec::new();
    let mut pos = span.value_start;

    while pos < span.value_end {
        while pos < span.value_end && is_html_ws(source[pos]) {
            pos += 1;
        }

        let token_start = pos;

        while pos < span.value_end && !is_html_ws(source[pos]) {
            pos += 1;
        }

        let token_end = pos;

        if token_start < token_end {
            if let Ok(text) = std::str::from_utf8(&source[token_start..token_end]) {
                out.push(ClassToken {
                    text,
                    start: token_start,
                    end: token_end,
                });
            }
        }
    }

    out
}
```

Usage:

```rust
let spans = extract_class_spans::<ScalarClassifier>(html);

for span in spans {
    for token in split_class_tokens(html, span) {
        println!("{:?}", token);
    }
}
```

---

## 9. Why this is better than a full SIMD DFA

For HTML, JSX, TSX, Vue, and Svelte, a full SIMD DFA gets messy because syntax is nested and context-sensitive enough that the lexer still needs state:

```text
HTML:
  <div class="...">

JSX:
  <div className="..." className={`...`} className={foo ? "a" : "b"} />

Vue:
  <div class="..." :class="{ active: ok }">

Svelte:
  <div class:active={ok} class="...">
```

A better architecture is:

```text
scanner-core/
  byte classifier
  delimiter finder
  quoted string finder
  whitespace splitter

frontends/
  html
  jsx
  tsx
  vue
  svelte
```

Each frontend reuses the same SIMD primitives but owns its syntax rules.

---

## 10. Recommended crate structure

```text
crates/
  class-scan-core/
    src/
      lib.rs
      classifier.rs
      scalar.rs
      tables.rs
      spans.rs

  class-scan-html/
    src/
      lib.rs
      lexer.rs

  class-scan-jsx/
    src/
      lib.rs

  class-scan-vue/
    src/
      lib.rs

  class-scan-cli/
    src/
      main.rs
```

Core trait:

```rust
pub trait ClassExtractor {
    fn extract<'a>(&self, source: &'a [u8]) -> Vec<ClassToken<'a>>;
}
```

HTML implementation:

```rust
pub struct HtmlExtractor;

impl HtmlExtractor {
    pub fn extract_with<C: ByteClassifier>(source: &[u8]) -> Vec<ClassSpan> {
        extract_class_spans::<C>(source)
    }
}
```

---

## 11. Important limitation

This is a **lexer**, not a complete HTML parser. That is fine for a Tailwind-like compiler.

Tailwind-style scanning does not need to build a DOM. It needs to find likely class tokens quickly. So the goal should be:

```text
Be fast.
Be conservative.
Extract class-like strings.
Avoid full DOM parsing.
Let false positives be filtered later.
```

For plain HTML, this lexer will be very fast. For JSX/TSX/Vue/Svelte, you should not try to make one universal parser. Instead, reuse the SIMD byte-scanning layer and write format-specific shallow lexers.

[1]: https://doc.rust-lang.org/std/simd/index.html?utm_source=chatgpt.com "std::simd - Rust"
