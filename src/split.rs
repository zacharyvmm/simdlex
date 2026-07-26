use crate::spans::{CandidateOrigin, ClassCandidate, ClassSpan, ClassToken, Span};

#[inline]
pub fn split_class_tokens<'a>(source: &'a [u8], span: ClassSpan) -> Vec<ClassToken<'a>> {
    split_span_tokens(source, span.into(), CandidateOrigin::HtmlAttribute)
}

#[inline]
pub fn split_candidate_tokens<'a>(
    source: &'a [u8],
    candidate: ClassCandidate,
) -> Vec<ClassToken<'a>> {
    split_span_tokens(source, candidate.value, candidate.origin)
}

#[inline]
fn split_span_tokens<'a>(
    source: &'a [u8],
    span: Span,
    origin: CandidateOrigin,
) -> Vec<ClassToken<'a>> {
    let mut out = Vec::new();
    let mut pos = span.start as usize;
    let end = span.end as usize;

    while pos < end {
        while pos < end && is_html_ws(source[pos]) {
            pos += 1;
        }

        let token_start = pos;

        while pos < end && !is_html_ws(source[pos]) {
            pos += 1;
        }

        if token_start < pos
            && let Ok(text) = std::str::from_utf8(&source[token_start..pos])
        {
            out.push(ClassToken {
                text,
                span: Span {
                    start: token_start as u32,
                    end: pos as u32,
                },
                origin,
            });
        }
    }

    out
}

#[inline]
pub(crate) fn is_html_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\r' | b' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_class_spans;

    #[test]
    fn splits_class_tokens_with_offsets() {
        let html = br#"<div class="  a  hover:bg-blue-500
            p-4 "></div>"#;
        let span = extract_class_spans(html)[0];
        let tokens = split_class_tokens(html, span);

        assert_eq!(
            tokens.iter().map(|token| token.text).collect::<Vec<_>>(),
            vec!["a", "hover:bg-blue-500", "p-4"]
        );
        assert_eq!(
            &html[tokens[1].span.start as usize..tokens[1].span.end as usize],
            b"hover:bg-blue-500"
        );
    }
}
