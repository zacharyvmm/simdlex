use crate::classifier::{ByteClassifier, ScalarClassifier};
use crate::spans::{
    CandidateOrigin, CandidateSyntax, ClassCandidate, ClassSpan, ExtractOptions, ExtractResult,
    Span,
};
use crate::split::is_html_ws;
use crate::stage1::{Structural, StructuralIndex, StructuralKind};
use crate::stage2::CandidateParser;

pub struct HtmlParser;

impl CandidateParser for HtmlParser {
    fn parse(
        source: &[u8],
        index: &StructuralIndex,
        options: &ExtractOptions,
    ) -> ExtractResult<ClassCandidate> {
        ExtractResult::new(extract_class_candidates_from_index(source, index, options))
    }
}

pub fn parse(
    source: &[u8],
    index: &StructuralIndex,
    options: &ExtractOptions,
) -> ExtractResult<ClassCandidate> {
    HtmlParser::parse(source, index, options)
}

pub fn build_html_structural_index(source: &[u8]) -> StructuralIndex {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("ssse3") {
            return build_html_structural_index_with::<crate::classifier::Ssse3Classifier>(source);
        }
    }

    build_html_structural_index_with::<ScalarClassifier>(source)
}

pub fn build_html_structural_index_with<C: ByteClassifier>(source: &[u8]) -> StructuralIndex {
    let mut items = Vec::with_capacity(source.len() / 8);
    let mut offset = 0;

    while offset + C::LANES <= source.len() {
        let classes = C::classify_block(source, offset);
        push_html_structurals(
            source,
            offset,
            classes.lt | classes.gt | classes.quote_double | classes.quote_single,
            &mut items,
        );
        offset += C::LANES;
    }

    push_html_tail_structurals(source, offset, &mut items);

    StructuralIndex { items }
}

pub fn extract_class_candidates_from_index(
    source: &[u8],
    index: &StructuralIndex,
    options: &ExtractOptions,
) -> Vec<ClassCandidate> {
    extract_value_spans(
        source,
        index,
        ClassAttributeMatcher::Configured(&options.class_attributes),
    )
    .into_iter()
    .map(|value| ClassCandidate {
        value,
        syntax: CandidateSyntax::StaticString,
        origin: CandidateOrigin::HtmlAttribute,
    })
    .collect()
}

pub fn extract_class_spans_from_index(source: &[u8], index: &StructuralIndex) -> Vec<ClassSpan> {
    extract_value_spans(source, index, ClassAttributeMatcher::ClassOnly)
        .into_iter()
        .map(Into::into)
        .collect()
}

fn extract_value_spans(
    source: &[u8],
    index: &StructuralIndex,
    matcher: ClassAttributeMatcher<'_>,
) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut item_idx = 0;

    while item_idx < index.items.len() {
        let item = index.items[item_idx];

        if item.kind != StructuralKind::Lt {
            item_idx += 1;
            continue;
        }

        let tag_start = item.offset as usize;
        let tag_content_start = tag_start.saturating_add(1);

        if let Some(skip_kind) = skip_tag_kind(source, tag_content_start) {
            item_idx = match skip_kind {
                SkipTagKind::Comment => find_comment_end_item(source, index, item_idx + 1),
                SkipTagKind::Other => find_next_gt_item(index, item_idx + 1),
            }
            .map(|end_idx| end_idx + 1)
            .unwrap_or(index.items.len());
            continue;
        }

        let (tag_end, next_item_idx) = find_tag_end(source, index, item_idx + 1);
        parse_tag_for_class_attrs(source, tag_content_start, tag_end, matcher, &mut spans);

        item_idx = next_item_idx
            .checked_add(1)
            .filter(|next| *next > item_idx)
            .unwrap_or(index.items.len());
    }

    spans
}

#[inline]
fn push_html_structurals(source: &[u8], offset: usize, mut mask: u64, items: &mut Vec<Structural>) {
    while mask != 0 {
        let lane = mask.trailing_zeros() as usize;
        let pos = offset + lane;

        if let Some(kind) = html_structural_kind(source[pos]) {
            items.push(Structural {
                offset: structural_offset(pos),
                kind,
            });
        }

        mask &= mask - 1;
    }
}

#[inline]
fn push_html_tail_structurals(source: &[u8], mut offset: usize, items: &mut Vec<Structural>) {
    while offset < source.len() {
        if let Some(kind) = html_structural_kind(source[offset]) {
            items.push(Structural {
                offset: structural_offset(offset),
                kind,
            });
        }

        offset += 1;
    }
}

#[inline]
fn html_structural_kind(byte: u8) -> Option<StructuralKind> {
    match byte {
        b'<' => Some(StructuralKind::Lt),
        b'>' => Some(StructuralKind::Gt),
        b'\'' => Some(StructuralKind::QuoteSingle),
        b'"' => Some(StructuralKind::QuoteDouble),
        _ => None,
    }
}

#[inline]
fn structural_offset(offset: usize) -> u32 {
    u32::try_from(offset).expect("structural index offsets require inputs shorter than 4 GiB")
}

#[inline]
fn skip_tag_kind(source: &[u8], tag_content_start: usize) -> Option<SkipTagKind> {
    if source
        .get(tag_content_start..tag_content_start.saturating_add(3))
        .is_some_and(|prefix| prefix == b"!--")
    {
        Some(SkipTagKind::Comment)
    } else if matches!(source.get(tag_content_start), Some(b'!' | b'/' | b'?')) {
        Some(SkipTagKind::Other)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipTagKind {
    Comment,
    Other,
}

#[inline]
fn find_next_gt_item(index: &StructuralIndex, mut item_idx: usize) -> Option<usize> {
    while item_idx < index.items.len() {
        if index.items[item_idx].kind == StructuralKind::Gt {
            return Some(item_idx);
        }

        item_idx += 1;
    }

    None
}

#[inline]
fn find_comment_end_item(
    source: &[u8],
    index: &StructuralIndex,
    mut item_idx: usize,
) -> Option<usize> {
    while item_idx < index.items.len() {
        let item = index.items[item_idx];

        if item.kind == StructuralKind::Gt {
            let offset = item.offset as usize;

            if offset >= 2
                && source
                    .get(offset - 2..offset)
                    .is_some_and(|prefix| prefix == b"--")
            {
                return Some(item_idx);
            }
        }

        item_idx += 1;
    }

    None
}

fn find_tag_end(source: &[u8], index: &StructuralIndex, mut item_idx: usize) -> (usize, usize) {
    let mut quote = None;

    while item_idx < index.items.len() {
        let item = index.items[item_idx];

        match quote {
            Some(active) if item.kind == active => quote = None,
            Some(_) => {}
            None => match item.kind {
                StructuralKind::QuoteDouble | StructuralKind::QuoteSingle => {
                    quote = Some(item.kind);
                }
                StructuralKind::Gt => return (item.offset as usize, item_idx),
                _ => {}
            },
        }

        item_idx += 1;
    }

    (source.len(), index.items.len())
}

fn parse_tag_for_class_attrs(
    source: &[u8],
    mut pos: usize,
    tag_end: usize,
    matcher: ClassAttributeMatcher<'_>,
    spans: &mut Vec<Span>,
) {
    pos = skip_name(source, pos, tag_end);

    while pos < tag_end {
        pos = skip_ws(source, pos, tag_end);

        if pos >= tag_end {
            break;
        }

        if source[pos] == b'/' {
            pos += 1;
            continue;
        }

        let attr_name_start = pos;
        let attr_name_end = skip_attr_name(source, pos, tag_end);

        if attr_name_start == attr_name_end {
            pos += 1;
            continue;
        }

        pos = skip_ws(source, attr_name_end, tag_end);

        if pos >= tag_end || source[pos] != b'=' {
            continue;
        }

        pos += 1;
        pos = skip_ws(source, pos, tag_end);

        if pos >= tag_end {
            break;
        }

        let is_class = matcher.matches(source, attr_name_start, attr_name_end);
        let (value_start, value_end, next_pos) = parse_attr_value(source, pos, tag_end);

        if is_class {
            spans.push(Span {
                start: value_start,
                end: value_end,
            });
        }

        pos = next_pos;
    }
}

#[inline]
fn parse_attr_value(source: &[u8], pos: usize, tag_end: usize) -> (usize, usize, usize) {
    match source[pos] {
        b'"' | b'\'' => {
            let quote = source[pos];
            let value_start = pos + 1;
            let mut i = value_start;

            while i < tag_end {
                if source[i] == quote {
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
                let b = source[i];

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
fn skip_name(source: &[u8], mut pos: usize, end: usize) -> usize {
    while pos < end {
        let b = source[pos];

        if is_html_ws(b) || b == b'/' || b == b'>' {
            break;
        }

        pos += 1;
    }

    pos
}

#[inline]
fn skip_attr_name(source: &[u8], mut pos: usize, end: usize) -> usize {
    while pos < end {
        let b = source[pos];

        if is_html_ws(b) || b == b'=' || b == b'/' || b == b'>' {
            break;
        }

        pos += 1;
    }

    pos
}

#[inline]
fn skip_ws(source: &[u8], mut pos: usize, end: usize) -> usize {
    while pos < end && is_html_ws(source[pos]) {
        pos += 1;
    }

    pos
}

#[derive(Clone, Copy)]
enum ClassAttributeMatcher<'a> {
    ClassOnly,
    Configured(&'a [String]),
}

impl ClassAttributeMatcher<'_> {
    #[inline]
    fn matches(self, source: &[u8], start: usize, end: usize) -> bool {
        match self {
            Self::ClassOnly => ascii_eq_ignore_case(&source[start..end], b"class"),
            Self::Configured(attributes) => attributes
                .iter()
                .any(|attribute| ascii_eq_ignore_case(&source[start..end], attribute.as_bytes())),
        }
    }
}

#[inline]
fn ascii_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScalarClassifier;
    use crate::stage1::build_structural_index_with;

    #[test]
    fn html_structural_index_records_only_tag_boundaries_and_quotes() {
        let html = br#"<div class="a b" data-x='1'></div>"#;
        let index = build_html_structural_index_with::<ScalarClassifier>(html);

        assert_eq!(
            index.items,
            vec![
                item(0, StructuralKind::Lt),
                item(11, StructuralKind::QuoteDouble),
                item(15, StructuralKind::QuoteDouble),
                item(24, StructuralKind::QuoteSingle),
                item(26, StructuralKind::QuoteSingle),
                item(27, StructuralKind::Gt),
                item(28, StructuralKind::Lt),
                item(33, StructuralKind::Gt),
            ]
        );
    }

    #[test]
    fn extracts_quoted_and_unquoted_class_candidates() {
        let html = br#"
            <div class="text-red-500 font-bold">
                <span id='x' CLASS='hover:bg-blue-500 p-4'></span>
                <button class=rounded-lg disabled></button>
            </div>
        "#;
        let index = build_structural_index_with::<ScalarClassifier>(html);
        let candidates =
            extract_class_candidates_from_index(html, &index, &ExtractOptions::default());

        assert_eq!(
            candidate_values(html, &candidates),
            vec![
                "text-red-500 font-bold",
                "hover:bg-blue-500 p-4",
                "rounded-lg"
            ]
        );
    }

    #[test]
    fn uses_configured_class_attributes() {
        let html = br#"<div data-class="yes" class="also" className="react"></div>"#;
        let index = build_structural_index_with::<ScalarClassifier>(html);
        let options = ExtractOptions {
            class_attributes: vec!["data-class".to_string(), "className".to_string()],
            ..ExtractOptions::default()
        };
        let candidates = extract_class_candidates_from_index(html, &index, &options);

        assert_eq!(candidate_values(html, &candidates), vec!["yes", "react"]);
    }

    #[test]
    fn quoted_gt_does_not_end_tag() {
        let html = br#"<div title="2 > 1" class="ok"></div>"#;
        let index = build_structural_index_with::<ScalarClassifier>(html);
        let spans = extract_class_spans_from_index(html, &index);

        assert_eq!(span_values(html, &spans), vec!["ok"]);
    }

    #[test]
    fn ignores_comments_closing_tags_and_declarations() {
        let html = br#"<!-- <div class="ignored"> <span class="also-ignored"> --></ignored><?pi class="no"?><div class = 'yes'></div>"#;
        let index = build_structural_index_with::<ScalarClassifier>(html);
        let spans = extract_class_spans_from_index(html, &index);

        assert_eq!(span_values(html, &spans), vec!["yes"]);
    }

    fn candidate_values<'a>(html: &'a [u8], candidates: &[ClassCandidate]) -> Vec<&'a str> {
        candidates
            .iter()
            .map(|candidate| {
                std::str::from_utf8(&html[candidate.value.start..candidate.value.end]).unwrap()
            })
            .collect()
    }

    fn span_values<'a>(html: &'a [u8], spans: &[ClassSpan]) -> Vec<&'a str> {
        spans
            .iter()
            .map(|span| std::str::from_utf8(&html[span.value_start..span.value_end]).unwrap())
            .collect()
    }

    fn item(offset: u32, kind: StructuralKind) -> Structural {
        Structural { offset, kind }
    }
}
