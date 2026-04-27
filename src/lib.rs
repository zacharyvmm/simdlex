#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassSpan {
    pub value_start: usize,
    pub value_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses;

    fn lt_block_mask(input: &[u8], offset: usize) -> u64 {
        Self::classify_block(input, offset).lt
    }

    fn gt_block_mask(input: &[u8], offset: usize) -> u64 {
        Self::classify_block(input, offset).gt
    }

    fn tag_end_block_mask(input: &[u8], offset: usize) -> u64 {
        let classes = Self::classify_block(input, offset);
        classes.quote_double | classes.quote_single | classes.gt
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScalarClassifier;

impl ByteClassifier for ScalarClassifier {
    const LANES: usize = 16;

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses {
        debug_assert!(offset + Self::LANES <= input.len());

        let mut out = ByteClasses {
            structural: 0,
            whitespace: 0,
            quote_double: 0,
            quote_single: 0,
            lt: 0,
            gt: 0,
            eq: 0,
        };

        for lane in 0..Self::LANES {
            let bit = 1u64 << lane;

            match input[offset + lane] {
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

    fn lt_block_mask(input: &[u8], offset: usize) -> u64 {
        scalar_byte_mask(input, offset, Self::LANES, b'<')
    }

    fn gt_block_mask(input: &[u8], offset: usize) -> u64 {
        scalar_byte_mask(input, offset, Self::LANES, b'>')
    }

    fn tag_end_block_mask(input: &[u8], offset: usize) -> u64 {
        let mut mask = 0;

        for lane in 0..Self::LANES {
            if matches!(input[offset + lane], b'"' | b'\'' | b'>') {
                mask |= 1u64 << lane;
            }
        }

        mask
    }
}

fn scalar_byte_mask(input: &[u8], offset: usize, lanes: usize, byte: u8) -> u64 {
    let mut mask = 0;

    for lane in 0..lanes {
        if input[offset + lane] == byte {
            mask |= 1u64 << lane;
        }
    }

    mask
}

#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy)]
pub struct RawSimdClassifier;

#[cfg(target_arch = "x86_64")]
impl ByteClassifier for RawSimdClassifier {
    const LANES: usize = 16;

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses {
        debug_assert!(offset + Self::LANES <= input.len());

        if std::is_x86_feature_detected!("ssse3") {
            unsafe { classify_block_ssse3(input, offset) }
        } else {
            ScalarClassifier::classify_block(input, offset)
        }
    }

    fn lt_block_mask(input: &[u8], offset: usize) -> u64 {
        if std::is_x86_feature_detected!("ssse3") {
            unsafe { byte_eq_block_mask_ssse3(input, offset, b'<') }
        } else {
            ScalarClassifier::lt_block_mask(input, offset)
        }
    }

    fn gt_block_mask(input: &[u8], offset: usize) -> u64 {
        if std::is_x86_feature_detected!("ssse3") {
            unsafe { byte_eq_block_mask_ssse3(input, offset, b'>') }
        } else {
            ScalarClassifier::gt_block_mask(input, offset)
        }
    }

    fn tag_end_block_mask(input: &[u8], offset: usize) -> u64 {
        if std::is_x86_feature_detected!("ssse3") {
            unsafe { tag_end_block_mask_ssse3(input, offset) }
        } else {
            ScalarClassifier::tag_end_block_mask(input, offset)
        }
    }
}

#[cfg(target_arch = "x86_64")]
const TLO: [u8; 16] = [8, 0, 1, 0, 0, 0, 0, 1, 0, 4, 4, 0, 2, 6, 2, 0];

#[cfg(target_arch = "x86_64")]
const THI: [u8; 16] = [4, 0, 9, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

#[cfg(target_arch = "x86_64")]
const STRUCTURAL_BITS: u8 = 1 | 2;

#[cfg(target_arch = "x86_64")]
const WHITESPACE_BITS: u8 = 4 | 8;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn classify_block_ssse3(input: &[u8], offset: usize) -> ByteClasses {
    use std::arch::x86_64::*;

    let bytes = unsafe { _mm_loadu_si128(input.as_ptr().add(offset).cast::<__m128i>()) };

    let lo_mask = _mm_set1_epi8(0x0f);
    let lo = _mm_and_si128(bytes, lo_mask);
    let hi = _mm_and_si128(_mm_srli_epi16(bytes, 4), lo_mask);

    let tlo = unsafe { _mm_loadu_si128(TLO.as_ptr().cast::<__m128i>()) };
    let thi = unsafe { _mm_loadu_si128(THI.as_ptr().cast::<__m128i>()) };

    let lo_class = _mm_shuffle_epi8(tlo, lo);
    let hi_class = _mm_shuffle_epi8(thi, hi);
    let class = _mm_and_si128(lo_class, hi_class);

    let structural = nonzero_bitmask(_mm_and_si128(class, _mm_set1_epi8(STRUCTURAL_BITS as i8)));
    let whitespace = nonzero_bitmask(_mm_and_si128(class, _mm_set1_epi8(WHITESPACE_BITS as i8)));

    ByteClasses {
        structural,
        whitespace,
        quote_double: eq_bitmask(bytes, b'"'),
        quote_single: eq_bitmask(bytes, b'\''),
        lt: eq_bitmask(bytes, b'<'),
        gt: eq_bitmask(bytes, b'>'),
        eq: eq_bitmask(bytes, b'='),
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn byte_eq_block_mask_ssse3(input: &[u8], offset: usize, byte: u8) -> u64 {
    use std::arch::x86_64::*;

    let bytes = unsafe { _mm_loadu_si128(input.as_ptr().add(offset).cast::<__m128i>()) };
    eq_bitmask(bytes, byte)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn tag_end_block_mask_ssse3(input: &[u8], offset: usize) -> u64 {
    use std::arch::x86_64::*;

    let bytes = unsafe { _mm_loadu_si128(input.as_ptr().add(offset).cast::<__m128i>()) };
    eq_bitmask(bytes, b'"') | eq_bitmask(bytes, b'\'') | eq_bitmask(bytes, b'>')
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
fn eq_bitmask(bytes: std::arch::x86_64::__m128i, byte: u8) -> u64 {
    use std::arch::x86_64::*;

    let wanted = _mm_set1_epi8(byte as i8);
    let cmp = _mm_cmpeq_epi8(bytes, wanted);
    (_mm_movemask_epi8(cmp) as u32 & 0xffff) as u64
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
fn nonzero_bitmask(bytes: std::arch::x86_64::__m128i) -> u64 {
    use std::arch::x86_64::*;

    let zero = _mm_setzero_si128();
    let cmp_zero = _mm_cmpeq_epi8(bytes, zero);
    ((!_mm_movemask_epi8(cmp_zero) as u32) & 0xffff) as u64
}

#[cfg(target_arch = "x86_64")]
pub type DefaultClassifier = RawSimdClassifier;

#[cfg(not(target_arch = "x86_64"))]
pub type DefaultClassifier = ScalarClassifier;

pub fn extract_class_spans(input: &[u8]) -> Vec<ClassSpan> {
    extract_class_spans_with::<DefaultClassifier>(input)
}

pub fn extract_class_spans_with<C: ByteClassifier>(input: &[u8]) -> Vec<ClassSpan> {
    let mut spans = Vec::new();
    let mut pos = 0;

    while pos < input.len() {
        match find_next_lt::<C>(input, pos) {
            Some(tag_start) => {
                pos = tag_start + 1;

                if matches!(input.get(pos), Some(b'!' | b'/' | b'?')) {
                    pos = skip_until_gt::<C>(input, pos).saturating_add(1);
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

        if token_start < pos
            && let Ok(text) = std::str::from_utf8(&source[token_start..pos])
        {
            out.push(ClassToken {
                text,
                start: token_start,
                end: pos,
            });
        }
    }

    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassToken<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

fn find_next_lt<C: ByteClassifier>(input: &[u8], mut pos: usize) -> Option<usize> {
    while pos + C::LANES <= input.len() {
        let lt = C::lt_block_mask(input, pos);

        if lt != 0 {
            return Some(pos + lt.trailing_zeros() as usize);
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
        let gt = C::gt_block_mask(input, pos);

        if gt != 0 {
            return pos + gt.trailing_zeros() as usize;
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
    let mut quote = 0;

    while pos + C::LANES <= input.len() {
        let mut interesting = C::tag_end_block_mask(input, pos);

        while interesting != 0 {
            let lane = interesting.trailing_zeros() as usize;
            let idx = pos + lane;
            let b = input[idx];

            match quote {
                0 => match b {
                    b'"' | b'\'' => quote = b,
                    b'>' => return idx,
                    _ => {}
                },
                q if b == q => quote = 0,
                _ => {}
            }

            interesting &= interesting - 1;
        }

        pos += C::LANES;
    }

    find_tag_end_scalar_tail(input, pos, quote)
}

fn find_tag_end_scalar_tail(input: &[u8], mut pos: usize, mut quote: u8) -> usize {
    debug_assert!(quote == 0 || quote == b'"' || quote == b'\'');

    while pos < input.len() {
        let b = input[pos];

        match quote {
            q if q != 0 && b == q => quote = 0,
            q if q != 0 => {}
            _ => match b {
                b'"' | b'\'' => quote = b,
                b'>' => return pos,
                _ => {}
            },
        }

        pos += 1;
    }

    input.len()
}

fn parse_tag_for_class_attrs(
    input: &[u8],
    mut pos: usize,
    tag_end: usize,
    spans: &mut Vec<ClassSpan>,
) {
    pos = skip_name(input, pos, tag_end);

    while pos < tag_end {
        pos = skip_ws(input, pos, tag_end);

        if pos >= tag_end {
            break;
        }

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

        pos = skip_ws(input, attr_name_end, tag_end);

        if pos >= tag_end || input[pos] != b'=' {
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
            .all(|(&x, &y)| x.eq_ignore_ascii_case(&y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_classifier_finds_expected_masks() {
        assert_classifier::<ScalarClassifier>();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn raw_simd_classifier_matches_scalar() {
        assert_classifier::<RawSimdClassifier>();

        let input = b"a<'\"=> \t\n\rxyz---";
        let scalar = ScalarClassifier::classify_block(input, 0);
        let simd = RawSimdClassifier::classify_block(input, 0);

        assert_eq!(simd, scalar);
    }

    #[test]
    fn extracts_quoted_and_unquoted_class_spans() {
        let html = br#"
            <div class="text-red-500 font-bold">
                <span id='x' CLASS='hover:bg-blue-500 p-4'></span>
                <button class=rounded-lg disabled></button>
            </div>
        "#;

        let spans = extract_class_spans_with::<DefaultClassifier>(html);
        let values = span_values(html, &spans);

        assert_eq!(
            values,
            vec![
                "text-red-500 font-bold",
                "hover:bg-blue-500 p-4",
                "rounded-lg"
            ]
        );
    }

    #[test]
    fn ignores_comments_closing_tags_and_non_class_attrs() {
        let html = br#"<!-- <div class="ignored"> --><div data-class="no" class = 'yes'></div>"#;

        let spans = extract_class_spans_with::<DefaultClassifier>(html);
        let values = span_values(html, &spans);

        assert_eq!(values, vec!["yes"]);
    }

    #[test]
    fn quoted_gt_does_not_end_tag() {
        let html = br#"<div title="2 > 1" class="ok"></div>"#;

        let spans = extract_class_spans_with::<DefaultClassifier>(html);
        let values = span_values(html, &spans);

        assert_eq!(values, vec!["ok"]);
    }

    #[test]
    fn long_quoted_attr_uses_later_tag_end() {
        let html =
            br#"<div title="0123456789abcdef > still quoted" data-x="another > value" class=ok></div>"#;

        let spans = extract_class_spans_with::<DefaultClassifier>(html);
        let values = span_values(html, &spans);

        assert_eq!(values, vec!["ok"]);
    }

    #[test]
    fn splits_class_tokens_with_offsets() {
        let html = br#"<div class="  a  hover:bg-blue-500
            p-4 "></div>"#;
        let span = extract_class_spans_with::<DefaultClassifier>(html)[0];
        let tokens = split_class_tokens(html, span);

        assert_eq!(
            tokens.iter().map(|token| token.text).collect::<Vec<_>>(),
            vec!["a", "hover:bg-blue-500", "p-4"]
        );
        assert_eq!(&html[tokens[1].start..tokens[1].end], b"hover:bg-blue-500");
    }

    fn assert_classifier<C: ByteClassifier>() {
        let input = b"a<'\"=> \t\n\rxyz---";
        let classes = C::classify_block(input, 0);

        assert_eq!(classes.lt, 1 << 1);
        assert_eq!(classes.quote_single, 1 << 2);
        assert_eq!(classes.quote_double, 1 << 3);
        assert_eq!(classes.eq, 1 << 4);
        assert_eq!(classes.gt, 1 << 5);
        assert_eq!(
            classes.whitespace,
            (1 << 6) | (1 << 7) | (1 << 8) | (1 << 9)
        );
        assert_eq!(
            classes.structural,
            (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5)
        );
    }

    fn span_values<'a>(html: &'a [u8], spans: &[ClassSpan]) -> Vec<&'a str> {
        spans
            .iter()
            .map(|span| std::str::from_utf8(&html[span.value_start..span.value_end]).unwrap())
            .collect()
    }
}
