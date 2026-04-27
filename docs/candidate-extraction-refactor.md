# Candidate Extraction Refactor Proposal

## Summary

This proposal refactors candidate extraction around the same architectural split
that makes simdjson effective: a fast, low-branching structural scan followed by
a semantic parser over the structural index.

The current implementation already contains the right ingredients. The
`ByteClassifier` abstraction can classify fixed-size byte blocks with scalar or
SIMD backends, and the HTML extractor uses those masks to find tags and class
attributes. However, structural scanning and semantic extraction are currently
interleaved in `extract::html::extract_class_spans_with`. That makes HTML fast
enough for the first prototype, but it limits reuse for JavaScript, JSX, Vue,
and Svelte, and it makes it hard to measure scanning separately from parsing.

The proposed refactor introduces two internal stages:

1. Stage 1 builds a compact index of structural byte positions.
2. Stage 2 walks that index with document-specific parsers and emits
   `ClassCandidate` values.

The public API can remain stable. Existing callers should continue using
`extract_candidates`, `extract_class_tokens`, and `extract_tokens`.

## Current Architecture

The existing extraction path is:

```text
source bytes
  -> extract_candidates(source, kind, options)
  -> extract::html::extract_class_spans(source)
  -> split_candidate_tokens(source, candidate)
  -> ClassToken
```

The main pieces are:

- `classifier.rs`: classifies blocks of bytes and exposes masks for structural
  characters such as `<`, `>`, `=`, and quotes.
- `extract/html.rs`: scans HTML, identifies tag boundaries, parses attributes,
  and returns class value spans.
- `extract/mod.rs`: routes by `DocumentKind`, converts HTML spans to
  `ClassCandidate`, and expands static candidates into tokens.
- `split.rs`: splits candidate spans into whitespace-separated Tailwind tokens.
- `spans.rs`: defines the shared extraction model: `Span`,
  `ClassCandidate`, `CandidateSyntax`, `CandidateOrigin`, diagnostics, document
  kinds, and options.

The important limitation is that `extract/html.rs` performs three distinct jobs
in one control loop:

1. It scans the whole input for interesting bytes.
2. It computes tag ranges while respecting quotes.
3. It parses attributes and emits class value spans.

That is reasonable for a small HTML-only extractor, but it is the wrong
long-term boundary for Tailwind Oxide. We need to extract candidates from
several syntactic contexts, and all of them benefit from the same low-level
structural scan.

## Goals

- Make the fast path simple: one pass over bytes to find structural positions.
- Reuse the same stage 1 output across HTML, JSX, JS, TS, Vue, and Svelte.
- Keep document-specific semantics out of stage 1.
- Keep allocation predictable and proportional to structural density, not file
  size alone.
- Make benchmarking more informative by measuring structural scanning,
  candidate parsing, and token splitting separately.
- Improve extensibility for Tailwind-specific extraction rules, including
  configured class attributes, helper functions, tagged templates, and
  framework directives.

## Non-Goals

- This refactor should not build a complete HTML, JavaScript, or TypeScript
  parser.
- It should not evaluate dynamic expressions.
- It should not expand Tailwind variants or validate whether a token exists in
  a Tailwind configuration.
- It should not require callers to understand the internal two-stage pipeline.
- It should not introduce heavyweight dependencies before proving that the
  structural-index approach is insufficient.

## Proposed Architecture

The internal pipeline becomes:

```text
source bytes
  -> stage1::build_structural_index(source)
  -> stage2 parser selected by DocumentKind
  -> Vec<ClassCandidate>
  -> split_candidate_tokens
  -> Vec<ClassToken>
```

### Stage 1: Structural Index

Stage 1 scans the input and records offsets of bytes that are useful to later
parsers. It does not know about Tailwind, HTML attributes, JSX props, or helper
function names.

Candidate structural bytes:

```text
< > / = ' " `
( ) { } [ ]
. : , ;
whitespace runs or selected whitespace positions
```

A first version can keep the index simple:

```rust
pub struct StructuralIndex {
    pub items: Vec<Structural>,
}

pub struct Structural {
    pub offset: u32,
    pub kind: StructuralKind,
}

pub enum StructuralKind {
    Lt,
    Gt,
    Slash,
    Eq,
    QuoteSingle,
    QuoteDouble,
    Backtick,
    OpenParen,
    CloseParen,
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Dot,
    Colon,
    Comma,
    Semicolon,
    Whitespace,
}
```

For larger files or tighter memory targets, the representation can evolve to
parallel arrays:

```rust
pub struct StructuralIndex {
    pub offsets: Vec<u32>,
    pub kinds: Vec<StructuralKind>,
}
```

or bit-packed kind tags. That optimization should wait until benchmarks show
the simple representation is too costly.

Stage 1 should expose backend-specific entry points the same way the current
classifier does:

```rust
pub fn build_structural_index(input: &[u8]) -> StructuralIndex;

pub fn build_structural_index_with<C: ByteClassifier>(
    input: &[u8],
) -> StructuralIndex;
```

On x86-64, runtime dispatch can select the SSSE3 implementation. Other targets
fall back to scalar classification.

### Stage 2: Candidate Parsers

Stage 2 receives the original source and the structural index. It performs
semantic recognition and emits `ClassCandidate` values.

```rust
pub trait CandidateParser {
    fn parse(
        source: &[u8],
        index: &StructuralIndex,
        options: &ExtractOptions,
    ) -> ExtractResult<ClassCandidate>;
}
```

Likely modules:

```text
src/stage2/mod.rs
src/stage2/html.rs
src/stage2/js.rs
src/stage2/jsx.rs
src/stage2/vue.rs
src/stage2/svelte.rs
```

The stage 2 parsers are allowed to inspect source slices between structural
positions. They should avoid rescanning the entire file byte by byte. Small
local scans are acceptable when they are bounded by structural ranges such as a
tag, string literal, template literal segment, argument list, or directive
value.

## HTML Parser Refactor

The current HTML extractor should become the first stage 2 parser.

Instead of repeatedly calling `find_next_lt`, `skip_until_gt`, and
`find_tag_end`, the HTML parser should walk the structural index:

1. Find `<`.
2. Skip comments, closing tags, declarations, and processing instructions.
3. Find the matching `>` while respecting `'` and `"` quotes.
4. Parse only the bytes inside that tag range.
5. Emit candidates for configured class attributes.

This allows the HTML parser to preserve existing behavior while removing
full-buffer scanning from the semantic parser.

The current `ClassSpan` type can remain for compatibility, but internally HTML
should emit `ClassCandidate` directly:

```rust
ClassCandidate {
    value: Span { start: value_start, end: value_end },
    syntax: CandidateSyntax::StaticString,
    origin: CandidateOrigin::HtmlAttribute,
}
```

`extract_class_spans` can be retained as a convenience wrapper over the new
candidate parser.

## JavaScript and TypeScript Parser

The JavaScript parser should be deliberately shallow. It should recognize
candidate-bearing surfaces without trying to typecheck or execute code.

Primary extraction forms:

```text
"px-4 text-sm"
'px-4 text-sm'
`px-4 ${dynamic} text-sm`
clsx("px-4", condition && "text-sm")
classnames(...)
cn(...)
cva(...)
tw`px-4 text-sm`
```

The parser can use the structural index to identify:

- string literal boundaries,
- template literal boundaries and `${...}` holes,
- call expressions whose callee matches `ExtractOptions::function_names`,
- tagged templates whose tag matches `ExtractOptions::tagged_template_names`,
- nested expression depth for diagnostics and safety limits.

Static string contents become `CandidateSyntax::StaticString`.
Static template segments become `CandidateSyntax::TemplateStaticPart`.
Dynamic expression ranges become `CandidateSyntax::DynamicExpression` or
`CandidateSyntax::UnsupportedDynamic` depending on whether the parser can make a
bounded, useful extraction.

The parser should not attempt to understand arbitrary concatenation in the first
iteration. For example:

```js
"px-" + size
```

can be represented as unsupported dynamic if diagnostics are enabled.

## JSX Parser

JSX can share much of the JavaScript parser but needs explicit attribute
handling.

Important forms:

```text
<div className="px-4 text-sm" />
<div className={'px-4 text-sm'} />
<div className={`px-4 ${active ? "block" : "hidden"}`} />
<div class="px-4" />
```

Configured `class_attributes` should apply here. The parser should emit
`CandidateOrigin::JsxAttribute` for JSX attributes and reuse the JS parser for
expressions inside `{...}`.

JSX should not be bolted onto the HTML parser. HTML and JSX look similar around
`<`, but JSX attributes can contain JavaScript expressions and nested JSX. It is
better to share lower-level helpers for quote and bracket matching than to
share a semantic parser.

## Vue and Svelte Parsers

Vue and Svelte should be treated as embedded-language containers.

The parser can identify regions:

```text
<template>...</template>
<script>...</script>
<style>...</style>
```

Then route:

- template regions through an HTML-like parser with framework directive support,
- script regions through the JS parser,
- style regions ignored for candidate extraction unless a later feature needs
  them.

Framework-specific candidate surfaces include:

```text
Vue:
  class="px-4"
  :class="{ active: isActive }"
  v-bind:class="..."

Svelte:
  class="px-4"
  class:active={isActive}
```

Static values can produce candidates immediately. Dynamic directive expressions
should route through the JS parser where possible or emit dynamic diagnostics
when requested.

## Data Model Adjustments

The existing `ClassCandidate` model is broadly sufficient. The main adjustment
is to make it the internal unit earlier in the pipeline.

Potential additions:

```rust
pub enum CandidateOrigin {
    HtmlAttribute,
    JsxAttribute,
    FrameworkDirective,
    JavaScriptCall,
    TaggedTemplate,
    StringLiteral,
    EmbeddedRegion,
}
```

The current `CandidateOrigin` already covers most of this, but separating
`TaggedTemplate` and `StringLiteral` may improve diagnostics and benchmarking.
This can be deferred if API stability matters more.

Diagnostics should grow carefully:

```rust
pub enum DiagnosticKind {
    MaxFileSizeExceeded { max: usize, actual: usize },
    UnsupportedDocumentKind(DocumentKind),
    UnsupportedDynamicClassExpression,
    InvalidUtf8,
    MaxExpressionDepthExceeded { max: usize },
    UnterminatedString,
    UnterminatedTemplate,
    UnterminatedTag,
}
```

These diagnostics should remain optional where they may be noisy.

## Allocation Strategy

Stage 1 introduces a new allocation, so it must earn its keep.

Recommended strategy:

- Preallocate structural index capacity based on file length and a conservative
  density estimate.
- Store offsets as `u32` and reject or chunk files larger than `u32::MAX`.
- Keep stage 2 outputs as spans into the original source.
- Avoid allocating strings during extraction.
- Keep token text borrowed as `&str` only after UTF-8 validation at token split
  time.

For very large files, a chunked structural index may eventually be useful, but
the first implementation should favor a straightforward contiguous vector.

## Error Handling and Safety

The extractor should be permissive. Candidate extraction is a best-effort scan
over user-authored source files, not a validating compiler front end.

Rules:

- Unterminated tags, strings, or templates should not panic.
- Malformed syntax should limit extraction to known-safe spans.
- Invalid UTF-8 should not break structural scanning, because stage 1 works on
  bytes.
- Token emission should continue to skip invalid UTF-8 token slices unless the
  caller requests diagnostics.
- Nesting depth limits should apply to JavaScript and framework expressions.

## Benchmarking Plan

The existing Criterion benchmarks should be split into pipeline stages:

```text
stage1/build_structural_index/html_small
stage1/build_structural_index/html_large
stage1/build_structural_index/jsx_large

stage2/html_candidates/from_index
stage2/js_candidates/from_index
stage2/jsx_candidates/from_index

pipeline/extract_candidates/html
pipeline/extract_class_tokens/html
pipeline/extract_class_tokens/jsx
```

Important metrics:

- bytes per second for stage 1,
- structural density per fixture,
- candidates per second for stage 2,
- allocations per extraction path,
- comparison against current `extract_class_spans`.

The first success criterion is not that every path becomes faster immediately.
The success criterion is a cleaner architecture where the hot scan is reusable
and measurable. After that, backend-specific SIMD work has a clear target.

## Migration Plan

### Phase 1: Introduce Stage 1

- Add `src/stage1.rs`.
- Move generalized byte classification into structural-index construction.
- Keep existing HTML extraction unchanged.
- Add tests comparing scalar and SIMD structural indexes.
- Add Criterion benchmarks for stage 1.

### Phase 2: Move HTML to Stage 2

- Add `src/stage2/html.rs`.
- Reimplement HTML candidate extraction over `StructuralIndex`.
- Keep `extract_class_spans` and `extract_class_spans_with` as compatibility
  wrappers.
- Compare output with the current extractor on existing tests and fixtures.
- Benchmark old HTML extraction against new stage1 plus stage2.

### Phase 3: Route Through the New Pipeline

- Update `extract_candidates` to build a structural index once.
- Route `DocumentKind::Html` through `stage2::html`.
- Keep unsupported diagnostics for other document kinds until their parsers
  land.

### Phase 4: Add JavaScript and JSX

- Add shallow JS parser support for strings, templates, calls, and tagged
  templates.
- Add JSX attribute parsing.
- Support configured `function_names`, `tagged_template_names`, and
  `class_attributes`.
- Add diagnostics for unsupported dynamic expressions.

### Phase 5: Add Vue and Svelte Containers

- Identify embedded template and script regions.
- Route each region to the appropriate existing parser.
- Add framework directive extraction incrementally.

## Testing Strategy

Tests should be organized by stage and by full pipeline behavior.

Stage 1 tests:

- scalar and SIMD produce identical structural indexes,
- structural positions are correct for representative HTML, JSX, and JS,
- tails shorter than the SIMD lane width are handled correctly.

Stage 2 HTML tests:

- quoted and unquoted class attributes,
- uppercase and configured class attributes,
- `>` inside quoted attributes,
- comments, closing tags, declarations, and malformed tags,
- offset preservation.

Stage 2 JS/JSX tests:

- simple strings,
- template static segments,
- dynamic template holes,
- configured helper calls,
- nested calls and depth limits,
- JSX string attributes,
- JSX expression attributes.

Pipeline tests:

- `extract_candidates` preserves candidate spans and origins,
- `extract_class_tokens` preserves borrowed token text and offsets,
- unsupported document kinds still produce diagnostics until implemented,
- max file size still short-circuits before stage 1 allocation.

## Risks

The main risk is over-indexing. If stage 1 records too many positions, memory
traffic can erase the benefits of SIMD classification. This should be managed
with benchmarks and by keeping the first structural set focused on bytes that
multiple parsers actually need.

Another risk is parser ambiguity in JSX and framework templates. The mitigation
is to keep stage 2 shallow and conservative: emit candidates only from bounded,
well-understood regions and report unsupported dynamic expressions when
requested.

A third risk is API churn. The mitigation is to introduce the new pipeline
behind the existing public functions and treat any data model changes as
additive until there is evidence that a breaking change is worthwhile.

## Recommended First Implementation

Start with HTML because it gives a direct comparison against the existing
extractor.

The first pull request should:

1. Add `stage1::StructuralIndex`.
2. Add structural classification for HTML-relevant bytes.
3. Add stage 1 tests and benchmarks.
4. Leave public extraction behavior unchanged.

The second pull request should:

1. Add `stage2::html`.
2. Reimplement HTML class candidate extraction over the structural index.
3. Route `DocumentKind::Html` through the new pipeline.
4. Preserve `extract_class_spans` compatibility.
5. Prove identical output on existing tests.

This sequence keeps risk low. It creates the architectural boundary first, then
moves one parser across it, then expands the parser family.

## Design Principle

The guiding rule should be:

```text
Stage 1 finds structure, not meaning.
Stage 2 finds candidates, not bytes.
```

If a proposed change makes stage 1 care about Tailwind semantics, it belongs in
stage 2. If a proposed stage 2 parser starts scanning the whole file from left
to right looking for individual bytes, the structural index is missing something
or the parser is crossing the wrong boundary.
