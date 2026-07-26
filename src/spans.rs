#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassSpan {
    pub value_start: u32,
    pub value_end: u32,
}

impl From<ClassSpan> for Span {
    fn from(span: ClassSpan) -> Self {
        Self {
            start: span.value_start,
            end: span.value_end,
        }
    }
}

impl From<Span> for ClassSpan {
    fn from(span: Span) -> Self {
        Self {
            value_start: span.start,
            value_end: span.end,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSyntax {
    StaticString,
    TemplateStaticPart,
    DynamicExpression,
    UnsupportedDynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateOrigin {
    HtmlAttribute,
    JsxAttribute,
    FrameworkDirective,
    JavaScriptCall,
    EmbeddedRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassCandidate {
    pub value: Span,
    pub syntax: CandidateSyntax,
    pub origin: CandidateOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassToken<'a> {
    pub text: &'a str,
    pub span: Span,
    pub origin: CandidateOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub kind: DiagnosticKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticKind {
    MaxFileSizeExceeded { max: usize, actual: usize },
    UnsupportedDocumentKind(DocumentKind),
    UnsupportedDynamicClassExpression,
    InvalidUtf8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Html,
    Js,
    Ts,
    Jsx,
    Tsx,
    Vue,
    Svelte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Document<'a> {
    pub source: &'a [u8],
    pub kind: DocumentKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractOptions {
    pub class_attributes: Vec<String>,
    pub function_names: Vec<String>,
    pub tagged_template_names: Vec<String>,
    pub include_dynamic_diagnostics: bool,
    pub max_expression_depth: usize,
    pub max_file_size: Option<usize>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            class_attributes: vec!["class".to_string(), "className".to_string()],
            function_names: vec![
                "clsx".to_string(),
                "classnames".to_string(),
                "cva".to_string(),
                "cn".to_string(),
            ],
            tagged_template_names: vec!["tw".to_string()],
            include_dynamic_diagnostics: false,
            max_expression_depth: 8,
            max_file_size: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractResult<T> {
    pub items: Vec<T>,
    pub diagnostics: Vec<Diagnostic>,
}

impl<T> ExtractResult<T> {
    pub fn new(items: Vec<T>) -> Self {
        Self {
            items,
            diagnostics: Vec::new(),
        }
    }

    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}
