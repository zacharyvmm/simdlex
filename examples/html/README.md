# Example: HTML element stream

> Experimental. Flat events only: no tree construction, tag balancing, or
> implied tags.

`HtmlReader` is a pull parser over the stage-1 delimiter index (`<`, `>`,
`'`, `"`; see [`stage1-index.md`](../../docs/stage1-index.md)).
`HtmlParser` runs stage 1 and returns the reader.

```rust
let mut parser = HtmlParser::new();
for event in parser.events(input) {
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

`HtmlReader::new` accepts any ascending tape that includes those offsets, so
a tape of every offset is a brute-force reference. Tests and the
`html_stream` fuzz target require the real index (every backend) to produce
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

## Benchmarks

`cargo bench -p simdlex-html` measures stage 1 per backend and the full
stream (including a lazy walk of every attribute) on an attribute-dense
fixture.
