use crate::classifier::{ByteClassifier, ScalarClassifier};
use crate::spans::ClassSpan;
use crate::split::is_html_ws;

pub fn extract_class_spans(input: &[u8]) -> Vec<ClassSpan> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("ssse3") {
            return extract_class_spans_with::<crate::classifier::Ssse3Classifier>(input);
        }
    }

    extract_class_spans_with::<ScalarClassifier>(input)
}

#[inline]
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

#[inline]
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

#[inline]
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

#[inline]
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

#[inline]
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

        let is_class = is_class_attr_name(input, attr_name_start, attr_name_end);
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

#[inline]
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

#[inline]
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

#[inline]
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

#[inline]
fn skip_ws(input: &[u8], mut pos: usize, end: usize) -> usize {
    while pos < end && is_html_ws(input[pos]) {
        pos += 1;
    }

    pos
}

#[inline]
fn is_class_attr_name(input: &[u8], start: usize, end: usize) -> bool {
    end - start == 5
        && input[start].eq_ignore_ascii_case(&b'c')
        && input[start + 1].eq_ignore_ascii_case(&b'l')
        && input[start + 2].eq_ignore_ascii_case(&b'a')
        && input[start + 3].eq_ignore_ascii_case(&b's')
        && input[start + 4].eq_ignore_ascii_case(&b's')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_quoted_and_unquoted_class_spans() {
        let html = br#"
            <div class="text-red-500 font-bold">
                <span id='x' CLASS='hover:bg-blue-500 p-4'></span>
                <button class=rounded-lg disabled></button>
            </div>
        "#;

        let spans = extract_class_spans(html);
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

        let spans = extract_class_spans(html);
        let values = span_values(html, &spans);

        assert_eq!(values, vec!["yes"]);
    }

    #[test]
    fn quoted_gt_does_not_end_tag() {
        let html = br#"<div title="2 > 1" class="ok"></div>"#;

        let spans = extract_class_spans(html);
        let values = span_values(html, &spans);

        assert_eq!(values, vec!["ok"]);
    }

    #[test]
    fn long_quoted_attr_uses_later_tag_end() {
        let html =
            br#"<div title="0123456789abcdef > still quoted" data-x="another > value" class=ok></div>"#;

        let spans = extract_class_spans(html);
        let values = span_values(html, &spans);

        assert_eq!(values, vec!["ok"]);
    }

    fn span_values<'a>(html: &'a [u8], spans: &[ClassSpan]) -> Vec<&'a str> {
        spans
            .iter()
            .map(|span| std::str::from_utf8(&html[span.value_start..span.value_end]).unwrap())
            .collect()
    }
}
