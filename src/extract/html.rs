use crate::spans::ClassSpan;
use crate::{HtmlConfig, HtmlEvent, HtmlGrammar, Parser};

pub fn extract_class_spans(input: &[u8]) -> Vec<ClassSpan> {
    let mut parser = Parser::<HtmlGrammar>::new(HtmlConfig::new(["class"]));
    let mut spans = Vec::new();
    let _ = parser.parse_with(input, |event| {
        if let HtmlEvent::ClassCandidate { span, .. } = event {
            spans.push(span.into());
        }
    });
    spans
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
            .map(|span| {
                std::str::from_utf8(&html[span.value_start as usize..span.value_end as usize])
                    .unwrap()
            })
            .collect()
    }
}
