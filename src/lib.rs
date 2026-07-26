mod backend;

pub mod extract;
pub mod grammar;
mod parser;
pub mod spans;
pub mod split;

pub use backend::ClassifiedBlock;
pub use extract::{
    extract_candidates, extract_class_tokens, extract_tokens, html::extract_class_spans,
};
pub use grammar::{
    Grammar, HtmlConfig, HtmlEmit, HtmlError, HtmlErrorKind, HtmlEvent, HtmlGrammar,
    HtmlStage1State, NibbleTables, Utf8Policy,
};
#[cfg(any(feature = "json-test-grammar", test))]
pub use grammar::{
    JsonConfig, JsonError, JsonErrorKind, JsonEvent, JsonGrammar, RawJsonString, decode_json_string,
};
pub use parser::{BackendError, BackendKind, CapacityError, ParseStats, Parser, Stage1Stats};
pub use spans::{
    CandidateOrigin, CandidateSyntax, ClassCandidate, ClassSpan, ClassToken, Diagnostic,
    DiagnosticKind, Document, DocumentKind, ExtractOptions, ExtractResult, Span,
};
pub use split::{split_candidate_tokens, split_class_tokens};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_html_candidates() {
        let html = br#"<div class="px-4 font-bold"></div>"#;
        let result = extract_candidates(html, DocumentKind::Html, &ExtractOptions::default());

        assert!(result.diagnostics.is_empty());
        assert_eq!(
            result.items,
            vec![ClassCandidate {
                value: Span { start: 12, end: 26 },
                syntax: CandidateSyntax::StaticString,
                origin: CandidateOrigin::HtmlAttribute,
            }]
        );
    }

    #[test]
    fn extracts_routed_html_tokens() {
        let html = br#"<div class="px-4 font-bold"></div>"#;
        let result = extract_class_tokens(html, DocumentKind::Html, &ExtractOptions::default());

        assert!(result.diagnostics.is_empty());
        assert_eq!(
            result
                .items
                .iter()
                .map(|token| (token.text, token.span, token.origin))
                .collect::<Vec<_>>(),
            vec![
                (
                    "px-4",
                    Span { start: 12, end: 16 },
                    CandidateOrigin::HtmlAttribute
                ),
                (
                    "font-bold",
                    Span { start: 17, end: 26 },
                    CandidateOrigin::HtmlAttribute
                )
            ]
        );
    }

    #[test]
    fn extract_tokens_accepts_document() {
        let document = Document {
            source: br#"<div class="a b"></div>"#,
            kind: DocumentKind::Html,
        };
        let result = extract_tokens(document, &ExtractOptions::default());

        assert_eq!(
            result
                .items
                .iter()
                .map(|token| token.text)
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn size_limit_returns_diagnostic() {
        let options = ExtractOptions {
            max_file_size: Some(4),
            ..ExtractOptions::default()
        };
        let result = extract_candidates(b"<div></div>", DocumentKind::Html, &options);

        assert!(result.items.is_empty());
        assert_eq!(
            result.diagnostics[0].kind,
            DiagnosticKind::MaxFileSizeExceeded { max: 4, actual: 11 }
        );
    }
}
