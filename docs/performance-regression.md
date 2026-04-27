# Performance Regression Report

## Summary

The slowdown observed during the candidate extraction refactor is real and is
not explained by the `a3b3706` docs-only commit itself. The main regression
comes from the stage1/stage2 implementation added after that point:

1. stage1 now classifies and materializes a much broader set of structural
   bytes than the old HTML extractor needed;
2. the SSSE3 path no longer uses the compact nibble-table structural mask that
   existed before the refactor;
3. HTML extraction is now split into two passes, so the pipeline pays for both
   structural indexing and stage2 parsing.

For the current HTML benchmark fixture, that means the new pipeline is doing
strictly more work than the old single-pass extractor.

## What Changed

Before the refactor, the HTML hot path was a combined scanner/parser:

- `extract_class_spans` scanned for `<`, skipped non-element tags, found the end
  of the tag, and parsed attributes in one loop.
- The SIMD classifier only needed to help with a narrow set of HTML-specific
  bytes.

After the refactor, the code is organized into:

- `src/stage1.rs`: a structural index builder that records positions for
  `<>/=\'"`` and many additional punctuation and whitespace bytes;
- `src/stage2/html.rs`: a parser that walks the structural index and then
  reparses tag contents to find class attributes;
- `src/classifier.rs`: a classifier that now computes a broader `structural`
  mask.

That architectural split is useful for future JS/JSX/Vue/Svelte support, but it
adds overhead on the HTML-only path unless stage1 stays very narrow.

## Measured Results

I compared the current tree against commit `a3b3706` using the existing
Criterion benchmark fixture.

Relevant numbers from local runs:

- `a3b3706 extract_class_spans/8192`: about `1.13 ms`
- current `stage1/build_structural_index/raw_simd/8192`: about `1.10 ms`
- current `stage2/html_candidates/from_index/8192`: about `1.12 ms`

That is the core problem:

- stage1 alone is already close to the full cost of the old extractor;
- stage2 adds another pass on top of that;
- the total HTML pipeline is therefore slower than the pre-refactor path.

The classifier benchmark shows the same trend:

- `a3b3706 classify_blocks/raw_simd`: about `305 us`
- current `classify_blocks/raw_simd`: about `455 us`

That is a large regression in the byte-classification hot loop itself.

## Root Cause

The main cost increase is in `src/classifier.rs`.

At `a3b3706`, the SSSE3 path used a compact nibble-table classifier to detect
HTML-relevant structural bytes. After the refactor, structural detection was
expanded to a large chain of equality masks:

- `<`
- `>`
- `/`
- `=`
- `'`
- `"`
- `` ` ``
- `(`
- `)`
- `{`
- `}`
- `[`
- `]`
- `.`
- `:`
- `,`
- `;`

That change is visible in the current SSSE3 implementation in
`src/classifier.rs`, where `structural_bitmask_ssse3` now ORs many separate
`eq_bitmask` calls together.

The same broader set is then consumed by `src/stage1.rs`, which materializes a
`Vec<Structural>` containing offsets and kinds for every matched byte. For an
HTML document, many of those bytes are irrelevant to the current extractor, but
they still incur classification and allocation cost.

## Secondary Contributors

The current benchmark setup makes the picture slightly harder to read:

- `stage2/html_candidates/from_index` builds the index outside the timed loop,
  so it hides stage1 cost.
- `pipeline/extract_candidates/html` still routes through the old
  `extract::html::extract_class_spans` path instead of the new stage1/stage2
  HTML pipeline.

That means the benchmark suite does not yet measure the end-to-end refactored
HTML path in one place. Even so, the component benchmarks already show the
regression clearly enough to explain the slowdown.

## What This Does Not Mean

This is not a reason to abandon the refactor. The architectural split is still
valuable if the goal is to support more document kinds.

What the measurements do show is narrower:

- the HTML fast path currently pays for generality before it gets any benefit
  from it;
- the generic structural index is too broad for HTML-only extraction at the
  moment;
- the SSSE3 classifier implementation regressed enough to matter on the hot
  path.

## Recommendation

The next performance step should be to preserve the refactor structure while
restoring a narrow HTML fast path:

1. keep HTML-specific indexing focused on the bytes needed for tag scanning;
2. avoid building a full generic structural index for HTML-only extraction until
   the broader formats need it;
3. restore a compact SIMD structural-mask implementation for the hot path;
4. update the benchmark so the timed HTML pipeline includes both stage1 and
   stage2, with one benchmark for each component and one for the full path.

That should make the regression visible, measurable, and actionable without
reverting the design work.
