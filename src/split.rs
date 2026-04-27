use crate::spans::{ClassSpan, ClassToken};

#[inline]
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

#[inline]
pub(crate) fn is_html_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\r' | b' ')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DefaultClassifier, extract_class_spans_with};

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
}
