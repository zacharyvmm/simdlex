I cannot literally be Daniel Lemire, but I would frame the refactor in the simdjson style: scan first,
understand second.

The current code already has part of this philosophy: src/classifier.rs:12 classifies bytes with scalar/
SIMD backends, while src/extract/html.rs:17 scans tags and immediately parses class attributes. The
refactor should make that boundary explicit.

Proposal

Refactor candidate extraction into two stages:

Stage 1: Structural Index

Add a stage1 module that performs one fast pass over bytes and emits only offsets of interesting bytes.

Example output:

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
    Colon,
    Comma,
    Dot,
    Whitespace,
}

Stage 1 should not know what class, clsx, JSX, Vue, or Svelte mean. It only answers: “where are the
structurally relevant bytes?” This matches simdjson stage 1: find the structural characters, record their
positions, and avoid semantic branching during the hot scan.

The existing ByteClassifier is the right foundation, but it should grow from HTML-specific masks like
lt_block_mask / tag_end_block_mask into a more general structural classifier.

Stage 2: Candidate Parser

Stage 2 walks the StructuralIndex plus the original source and emits ClassCandidate.

The parser becomes document-aware:

pub trait CandidateParser {
    fn parse(
        source: &[u8],
        index: &StructuralIndex,
        options: &ExtractOptions,
    ) -> ExtractResult<ClassCandidate>;
}

Implementations:

stage2::html
stage2::jsx
stage2::js
stage2::vue
stage2::svelte

For HTML, stage 2 replaces the current combined scanner/parser in src/extract/html.rs:17. It would walk
from < to matching > using quote-aware structural positions, then inspect attributes and emit candidates
for configured class_attributes.

For JS/TS/JSX, stage 2 can recognize:

"class string"
`template static ${dynamic} part`
clsx(...)
classnames(...)
cn(...)
cva(...)
tw`...`
className="..."
className={...}

and map them to the existing syntax model in src/spans.rs:31:

CandidateSyntax::StaticString
CandidateSyntax::TemplateStaticPart
CandidateSyntax::DynamicExpression
CandidateSyntax::UnsupportedDynamic

Pipeline

The public API can remain stable:

extract_class_tokens(source, kind, options)

Internally:

source bytes
  -> stage1 structural index
  -> stage2 document parser
  -> candidate spans
  -> split_candidate_tokens
  -> ClassToken

split.rs can remain a final lightweight consumer for now, since it already operates on spans and avoids
unnecessary copies.

Why This Matches simdjson

The important property is that stage 1 is fast, broad, and dumb. It scans every byte once and builds a
compact index. Stage 2 is slower and semantic, but it walks only structural positions and relevant slices.

That gives you:

SIMD-friendly hot path
one structural scan reusable across HTML/JSX/Vue/Svelte
fewer full-buffer rescans
clear parser boundaries
better benchmarking of scan cost vs parse cost

Concrete Refactor Shape

I would introduce:

src/stage1.rs              // structural index builder
src/stage2/mod.rs          // parser trait and routing
src/stage2/html.rs         // HTML attribute candidate parser
src/stage2/js.rs           // JS/TS string/call/template parser
src/stage2/jsx.rs          // JSX attribute parser
src/stage2/embedded.rs     // Vue/Svelte region routing

Then gradually move logic:

1. Move byte scanning from extract/html.rs into stage1.
2. Rewrite HTML extraction as a stage 2 parser over StructuralIndex.
3. Keep existing public functions and tests green.
4. Add JS/JSX candidate extraction without changing the scanner.
5. Benchmark stage 1 independently, like simdjson benchmarks structural indexing separately from parsing.

The key design rule: stage 1 never emits candidates; stage 2 never scans the whole file byte-by-byte
unless it is inside a small slice selected by the structural index.