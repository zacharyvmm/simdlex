# HTML stage 2: StAX element stream

> Experimental. Flat events only: no tree construction, tag balancing, or
> implied tags.

`HtmlReader` is a pull parser over the stage-1 delimiter index (`<`, `>`,
`'`, `"`; see [`stage1-index.md`](stage1-index.md)). `HtmlStreamGrammar`
wraps it for `Parser::parse_with`.

```rust
let mut parser = Parser::<HtmlStreamGrammar>::new(());
parser.stage1(input)?;
for event in HtmlReader::new(input, parser.index()) {
    match event {
        HtmlStreamEvent::StartElement(element) => {
            let class = element.attribute(b"class");
            for attribute in element.attributes() { /* lazily parsed */ }
        }
        HtmlStreamEvent::EndElement { name, .. } => {}
        HtmlStreamEvent::Text { raw, .. } => {}
        HtmlStreamEvent::Comment { .. }
        | HtmlStreamEvent::Doctype { .. }
        | HtmlStreamEvent::BogusComment { .. } => {}
    }
}
```

## How the index is used

| State | Index use |
|---|---|
| data | jump to the next `<`; a `<` that does not open markup stays text |
| tag | names, whitespace, and `=` are read directly; a quoted value jumps to its closing quote |
| comment / DOCTYPE / bogus comment | jump between `>` candidates |
| raw text (`script`, `style`, `textarea`, `title`, …) | jump between `<` candidates to the matching `</name` |

`HtmlReader::from_positions` accepts any ascending superset of those offsets,
so `0..len` is a brute-force reference. Tests and the `html_stream` fuzz
target require the real index (every backend × `Tape`/`Masks`) to produce
exactly the reference stream.

## Semantics

Tokenization follows the WHATWG tokenizer for the data, tag, attribute,
comment, DOCTYPE, and bogus-comment states:

- unquoted values run to whitespace or `>`, so `<a href=/x/>` is not
  self-closing;
- the first byte of an attribute name is always part of it (`<a =b>`);
- `<!-->` and `<!--->` are empty comments, and `--!>` also closes a comment;
  a comment cut off by the end of input omits a trailing `-`, `--`, or `--!`;
- `</>` is ignored; `</ x>`, `<?…>`, and `<!…>` are bogus comments;
- a tag cut off by the end of input is dropped.

Simplifications: character references are not decoded, DOCTYPEs are not
split into name and identifiers, and raw-text elements end at the first
matching end tag without the script-data escape states.

## Benchmarks (local, AVX2)

`grammar_parser` HTML fixture (tag-dense), including a full walk of every
attribute:

| | throughput |
|---|---|
| class candidates (existing extractor) | 826 MiB/s |
| stream, tape | 407 MiB/s |
| stream, masks | 384 MiB/s |
