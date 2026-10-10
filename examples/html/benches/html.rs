use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use simdlex::{Indexer, StructuralIndex, available_backends};
use simdlex_html::{HtmlParser, HtmlSpec, HtmlStreamEvent};

/// Attribute-dense markup: about one structural per 8 bytes.
fn fixture(repetitions: usize) -> Vec<u8> {
    let mut html = Vec::with_capacity(repetitions * 128);
    for i in 0..repetitions {
        html.extend_from_slice(br#"<div class="px-4 py-2 text-red-500 hover:bg-blue-500 "#);
        html.extend_from_slice(if i % 2 == 0 {
            b"font-bold"
        } else {
            b"font-medium"
        });
        html.extend_from_slice(
            br#"" data-state='open'><span class='inline-flex gap-2'></span></div>"#,
        );
    }
    html
}

fn checksum(event: HtmlStreamEvent<'_>) -> u64 {
    match event {
        HtmlStreamEvent::StartElement(element) => {
            element
                .attributes()
                .fold(element.name.len() as u64, |sum, attribute| {
                    sum + attribute.name.len() as u64
                        + attribute.value.map_or(0, |value| value.raw.len() as u64)
                })
        }
        HtmlStreamEvent::EndElement { name, .. } => name.len() as u64,
        HtmlStreamEvent::Text { raw, .. }
        | HtmlStreamEvent::Comment { raw, .. }
        | HtmlStreamEvent::Doctype { raw, .. }
        | HtmlStreamEvent::BogusComment { raw, .. } => raw.len() as u64,
    }
}

fn html(c: &mut Criterion) {
    let input = fixture(8_192);
    let mut group = c.benchmark_group("html");
    group.throughput(Throughput::Bytes(input.len() as u64));

    for backend in available_backends() {
        let indexer = Indexer::<HtmlSpec>::with_backend(backend).unwrap();
        let mut index = StructuralIndex::new();
        group.bench_function(format!("stage1/{backend:?}"), |bench| {
            bench.iter(|| indexer.index(black_box(&input), &mut (), &mut index))
        });
    }

    let mut parser = HtmlParser::new();
    group.bench_function("stream", |bench| {
        bench.iter(|| parser.events(black_box(&input)).map(checksum).sum::<u64>())
    });
    group.finish();
}

criterion_group!(benches, html);
criterion_main!(benches);
