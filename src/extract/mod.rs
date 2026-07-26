use crate::spans::{
    CandidateOrigin, CandidateSyntax, ClassCandidate, ClassToken, Diagnostic, DiagnosticKind,
    Document, DocumentKind, ExtractOptions, ExtractResult, Span,
};
use crate::split::split_candidate_tokens;
use crate::{HtmlConfig, HtmlEmit, HtmlEvent, HtmlGrammar, Parser};

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
                    start: max.min(u32::MAX as usize) as u32,
                    end: source.len().min(u32::MAX as usize) as u32,
                },
                kind: DiagnosticKind::MaxFileSizeExceeded {
                    max,
                    actual: source.len(),
                },
            }],
        };
    }

    match kind {
        DocumentKind::Html => {
            let config = HtmlConfig::new(&options.class_attributes);
            let mut parser = Parser::<HtmlGrammar>::new(config);
            let mut items = Vec::new();
            let result = parser.parse_with(source, |event| {
                if let HtmlEvent::ClassCandidate { span, .. } = event {
                    items.push(ClassCandidate {
                        value: span,
                        syntax: CandidateSyntax::StaticString,
                        origin: CandidateOrigin::HtmlAttribute,
                    });
                }
            });
            if result.is_err() {
                ExtractResult {
                    items: Vec::new(),
                    diagnostics: vec![Diagnostic {
                        span: Span {
                            start: 0,
                            end: source.len().min(u32::MAX as usize) as u32,
                        },
                        kind: DiagnosticKind::MaxFileSizeExceeded {
                            max: (u32::MAX - 1) as usize,
                            actual: source.len(),
                        },
                    }],
                }
            } else {
                ExtractResult::new(items)
            }
        }
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
                    end: source.len().min(u32::MAX as usize) as u32,
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
    if kind == DocumentKind::Html {
        if let Some(max) = options.max_file_size
            && source.len() > max
        {
            return ExtractResult {
                items: Vec::new(),
                diagnostics: vec![Diagnostic {
                    span: Span {
                        start: max.min(u32::MAX as usize) as u32,
                        end: source.len().min(u32::MAX as usize) as u32,
                    },
                    kind: DiagnosticKind::MaxFileSizeExceeded {
                        max,
                        actual: source.len(),
                    },
                }],
            };
        }

        let config = HtmlConfig::new(&options.class_attributes).emit(HtmlEmit::Tokens);
        let mut parser = Parser::<HtmlGrammar>::new(config);
        let mut items = Vec::new();
        let result = parser.parse_with(source, |event| {
            if let HtmlEvent::ClassToken { raw, span } = event
                && let Ok(text) = std::str::from_utf8(raw)
            {
                items.push(ClassToken {
                    text,
                    span,
                    origin: CandidateOrigin::HtmlAttribute,
                });
            }
        });
        return if result.is_ok() {
            ExtractResult::new(items)
        } else {
            ExtractResult::empty()
        };
    }

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
