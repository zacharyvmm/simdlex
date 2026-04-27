use crate::spans::{
    CandidateOrigin, CandidateSyntax, ClassCandidate, ClassToken, Diagnostic, DiagnosticKind,
    Document, DocumentKind, ExtractOptions, ExtractResult, Span,
};
use crate::split::split_candidate_tokens;

pub mod html;

pub fn extract_candidates(
    source: &[u8],
    kind: DocumentKind,
    options: &ExtractOptions,
) -> ExtractResult<ClassCandidate> {
    if let Some(max) = options.max_file_size
        && source.len() > max
    {
        return ExtractResult {
            items: Vec::new(),
            diagnostics: vec![Diagnostic {
                span: Span {
                    start: max,
                    end: source.len(),
                },
                kind: DiagnosticKind::MaxFileSizeExceeded {
                    max,
                    actual: source.len(),
                },
            }],
        };
    }

    match kind {
        DocumentKind::Html => ExtractResult::new(
            html::extract_class_spans(source)
                .into_iter()
                .map(|span| ClassCandidate {
                    value: span.into(),
                    syntax: CandidateSyntax::StaticString,
                    origin: CandidateOrigin::HtmlAttribute,
                })
                .collect(),
        ),
        DocumentKind::Js
        | DocumentKind::Ts
        | DocumentKind::Jsx
        | DocumentKind::Tsx
        | DocumentKind::Vue
        | DocumentKind::Svelte => ExtractResult {
            items: Vec::new(),
            diagnostics: vec![Diagnostic {
                span: Span {
                    start: 0,
                    end: source.len(),
                },
                kind: DiagnosticKind::UnsupportedDocumentKind(kind),
            }],
        },
    }
}

pub fn extract_tokens<'a>(
    document: Document<'a>,
    options: &ExtractOptions,
) -> ExtractResult<ClassToken<'a>> {
    extract_class_tokens(document.source, document.kind, options)
}

pub fn extract_class_tokens<'a>(
    source: &'a [u8],
    kind: DocumentKind,
    options: &ExtractOptions,
) -> ExtractResult<ClassToken<'a>> {
    let candidates = extract_candidates(source, kind, options);
    let mut diagnostics = candidates.diagnostics;
    let mut items = Vec::new();

    for candidate in candidates.items {
        match candidate.syntax {
            CandidateSyntax::StaticString | CandidateSyntax::TemplateStaticPart => {
                items.extend(split_candidate_tokens(source, candidate));
            }
            CandidateSyntax::DynamicExpression | CandidateSyntax::UnsupportedDynamic => {
                if options.include_dynamic_diagnostics {
                    diagnostics.push(Diagnostic {
                        span: candidate.value,
                        kind: DiagnosticKind::UnsupportedDynamicClassExpression,
                    });
                }
            }
        }
    }

    ExtractResult { items, diagnostics }
}
