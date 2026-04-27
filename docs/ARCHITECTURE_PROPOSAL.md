# Architecture Proposal: Multi-Syntax Class Extraction

## Goal

Build `simdlex` into a small, fast extraction engine that can find class-like tokens across:

- HTML
- JSX and TSX
- JavaScript and TypeScript
- Vue single-file components
- Svelte components

The architecture should preserve the current strength of the project: cheap byte-level scanning for common markup paths. It should also make room for syntax-aware handling where byte scanning alone is ambiguous, especially in JSX/TSX, template literals, Vue directives, Svelte expressions, and JavaScript helper calls.

## Design Principles

1. Keep the SIMD byte classifier independent from language rules.
2. Parse only as much syntax as needed to find candidate class strings.
3. Return source offsets for every token so callers can perform diagnostics, transforms, or editor integrations.
4. Treat framework files as containers with embedded languages instead of as unrelated parsers.
5. Prefer deterministic extraction over evaluation. Dynamic expressions can produce structured candidates, not guessed final strings.

## Proposed Layers

```text
public API
   |
   v
document router
   |
   +-- html extractor
   +-- jsx/tsx extractor
   +-- js/ts extractor
   +-- vue extractor
   +-- svelte extractor
   |
   v
candidate normalizer
   |
   v
token splitter
   |
   v
diagnostics + spans

shared below all extractors:
   byte classifier
   source cursor
   span model
   string literal scanner
   embedded region model
```

## Core Data Model

The public API should expose offsets and provenance rather than only plain strings.

```rust
pub struct Document<'a> {
    pub source: &'a [u8],
    pub kind: DocumentKind,
}

pub enum DocumentKind {
    Html,
    Js,
    Ts,
    Jsx,
    Tsx,
    Vue,
    Svelte,
}

pub struct ClassCandidate {
    pub value: Span,
    pub syntax: CandidateSyntax,
    pub origin: CandidateOrigin,
}

pub struct ClassToken<'a> {
    pub text: &'a str,
    pub span: Span,
    pub origin: CandidateOrigin,
}

pub struct Span {
    pub start: usize,
    pub end: usize,
}
```

`CandidateSyntax` describes how safe it is to split the value:

- `StaticString`: directly splittable class text.
- `TemplateStaticPart`: static segment inside a template literal.
- `DynamicExpression`: a recognized dynamic container such as `className={expr}` or `:class="expr"`.
- `UnsupportedDynamic`: a candidate location was found, but deterministic token extraction is not possible.

`CandidateOrigin` records where the value came from:

- HTML attribute: `class="..."`
- JSX attribute: `className="..."`, `class="..."`
- Framework directive: `:class`, `v-bind:class`, `class:active`, `class={{ ... }}`
- JavaScript call: `clsx(...)`, `classnames(...)`, `cva(...)`, `tw(...)`
- Embedded region: Vue template, Vue script, Svelte markup, Svelte script

## Module Layout

```text
src/
  lib.rs
  classifier.rs
  cursor.rs
  spans.rs
  split.rs
  extract/
    mod.rs
    html.rs
    jsx.rs
    js.rs
    vue.rs
    svelte.rs
    embedded.rs
  syntax/
    strings.rs
    comments.rs
    brackets.rs
```

Recommended responsibility split:

- `classifier.rs`: current scalar/SIMD byte classification.
- `cursor.rs`: common byte cursor helpers over `&[u8]`.
- `spans.rs`: `Span`, `ClassCandidate`, `ClassToken`, diagnostics.
- `split.rs`: class token splitting from candidate spans.
- `extract/html.rs`: current HTML tag and attribute scanner.
- `extract/jsx.rs`: JSX element and attribute scanner.
- `extract/js.rs`: string/template/call-expression extraction for JS and TS.
- `extract/vue.rs`: SFC region splitter plus Vue template rules.
- `extract/svelte.rs`: Svelte region splitter plus Svelte markup rules.
- `extract/embedded.rs`: utilities for running one extractor over a source subrange while preserving global offsets.
- `syntax/*`: lightweight lexical helpers shared by JS-like languages.

## Extraction Flow

1. `extract_tokens(document)` dispatches by `DocumentKind`.
2. The language extractor emits `ClassCandidate` values.
3. The normalizer discards unsupported candidates unless the caller requested diagnostics.
4. Static candidate values are split by whitespace into `ClassToken` values.
5. Dynamic candidate values are optionally scanned for static strings depending on policy.

```rust
pub fn extract_tokens<'a>(document: Document<'a>, options: ExtractOptions) -> ExtractResult<'a> {
    let candidates = extract_candidates(document, options);
    normalize_and_split(document.source, candidates, options)
}
```

## Language Strategies

### HTML

Use the current scanner as the base implementation.

Supported first:

- `class="..."`
- `class='...'`
- `class=...`
- case-insensitive `class`

Add later:

- configurable attribute names, for example `data-class`
- ignoring raw text regions: `<script>`, `<style>`, `<textarea>`, `<title>`
- recovery diagnostics for malformed tags

### JSX and TSX

JSX needs a JSX-aware scanner because `<` and `>` also appear in type parameters and expressions.

Supported first:

- `className="..."`
- `className='...'`
- `class="..."`
- `className={`...`}`
- static strings inside `className={condition ? "a" : "b"}`

Do not try to evaluate JavaScript expressions. Instead, recursively scan string literals inside recognized class-bearing expressions.

Examples:
```tsx
<div className="px-4 font-bold" />
<div className={isActive ? "text-blue-600" : "text-gray-500"} />
<div className={`grid ${open ? "block" : "hidden"}`} />
```

The extractor should emit static string parts and mark expression containers as dynamic in diagnostics.

### JavaScript and TypeScript

JS/TS extraction should focus on known class composition APIs and tagged templates. This should be configurable because projects use different helpers.

Supported first:

- `clsx("a", condition && "b")`
- `classnames("a", { b: condition })`
- `cva("base", { variants: ... })`
- configured identifiers such as `cn`, `tw`, `twMerge`
- tagged templates such as `` tw`px-4 text-sm` ``

Implementation should use a lightweight lexer, not a full TypeScript type checker. It only needs comments, strings, template literals, balanced brackets, identifiers, and call boundaries.

### Vue

Vue should be treated as an SFC container:

- `<template>`: run Vue template extraction.
- `<script>` and `<script setup>`: run JS/TS extraction.
- `<style>`: ignored by default.

Vue template support:

- `class="..."`
- `:class="'static tokens'"`
- `v-bind:class="'static tokens'"`
- `:class="{ active: isActive, hidden: !open }"` should emit object keys that are static class names.
- `:class="['a', condition && 'b']"` should emit static strings inside the expression.

### Svelte

Svelte should also be treated as a component container:

- markup outside `<script>` and `<style>`: run Svelte markup extraction.
- `<script>`: run JS/TS extraction.
- `<style>`: ignored by default.

Svelte support:

- `class="..."`
- `class:name={expr}` should emit `name`.
- `class:name` should emit `name`.
- expression attributes such as `class={expr}` should scan static string literals inside the expression.

## Parser Depth

This project should not start with full AST parsing. A clean staged approach is:

1. Byte scanner for HTML-like tag structure.
2. Lightweight JS-family lexer for comments, strings, templates, identifiers, and brackets.
3. Embedded region splitter for Vue and Svelte.
4. Optional feature-gated AST backend later if a consumer needs exact framework semantics.

This keeps the default dependency surface small and preserves the ability to benchmark the hot paths.

## Public API Shape

```rust
pub fn extract_class_tokens<'a>(
    source: &'a [u8],
    kind: DocumentKind,
    options: &ExtractOptions,
) -> ExtractResult<'a>;

pub fn extract_class_candidates(
    source: &[u8],
    kind: DocumentKind,
    options: &ExtractOptions,
) -> ExtractResult<ClassCandidate>;
```

`ExtractOptions` should include:

- `class_attributes`: default `["class", "className"]`
- `function_names`: default `["clsx", "classnames", "cva", "cn"]`
- `tagged_template_names`: default `["tw"]`
- `include_dynamic_diagnostics`
- `max_expression_depth`
- `max_file_size`

## Error Handling

Extraction should be best-effort. Malformed source should return partial results plus diagnostics.

```rust
pub struct ExtractResult<T> {
    pub items: Vec<T>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct Diagnostic {
    pub span: Span,
    pub kind: DiagnosticKind,
}
```

Diagnostics should be non-fatal for:

- unterminated string
- unterminated tag
- unbalanced expression
- unsupported dynamic class expression
- invalid UTF-8 inside a candidate value

## Implementation Milestones

### Milestone 1: Refactor Current HTML Extractor

- Move byte classification into `classifier.rs`.
- Move span and token types into `spans.rs`.
- Move token splitting into `split.rs`.
- Move current HTML scanner into `extract/html.rs`.
- Preserve existing tests and benchmarks.

### Milestone 2: Add Document Router

- Add `DocumentKind`.
- Add `extract_candidates` and `extract_tokens`.
- Keep `extract_class_spans` as a compatibility wrapper around HTML extraction.

### Milestone 3: Add JS-Family Lexical Helpers

- Implement comment skipping.
- Implement quoted string scanning with escapes.
- Implement template literal scanning with static segment spans.
- Implement balanced `{}`, `[]`, and `()` scanning.

### Milestone 4: Add JSX/TSX Extraction

- Find JSX tags and attributes.
- Support static `className` and `class` attributes.
- Scan static strings inside expression attributes.

### Milestone 5: Add JS/TS Helper Extraction

- Detect configured function calls.
- Extract static string arguments, object keys, and template static parts.
- Add fixtures for `clsx`, `classnames`, `cva`, and `cn`.

### Milestone 6: Add Vue and Svelte Containers

- Split SFC/component regions.
- Run embedded extractors with global offset preservation.
- Add directive-specific extraction rules.

### Milestone 7: Performance and Compatibility

- Benchmark each syntax kind.
- Add corpus tests for malformed input.
- Add snapshot-style tests for offsets.
- Feature-gate optional heavier parser integrations if needed.

## Key Tradeoffs

- A lightweight lexer will miss some complex semantics, but it keeps extraction fast and predictable.
- Full AST parsing would improve precision for JS/TS, but it adds dependencies, language-version churn, and slower startup.
- Dynamic class expressions should be reported explicitly rather than guessed. This gives consumers useful visibility without pretending static extraction can solve runtime logic.

## Recommended First Change

The first concrete change should be a mechanical module split with no behavior changes. That creates stable boundaries before adding new syntaxes and lets the existing tests protect the current HTML behavior.
