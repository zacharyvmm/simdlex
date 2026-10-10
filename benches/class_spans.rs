use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use simdlex::{
    BackendKind, DocumentKind, ExtractOptions, HtmlConfig, HtmlGrammar, IndexMode, Parser,
    extract_candidates, extract_class_spans, split_class_tokens,
};

fn bench_extract_class_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("extract_class_spans");

    for size in [32, 512, 8_192] {
        let html = fixture(size);
        group.throughput(Throughput::Bytes(html.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &html, |b, html| {
            b.iter(|| extract_class_spans(black_box(html)));
        });
    }

    group.finish();
}

fn bench_split_class_tokens(c: &mut Criterion) {
    let html = fixture(512);
    let spans = extract_class_spans(&html);

    c.bench_function("split_class_tokens/all_spans", |b| {
        b.iter(|| {
            for span in black_box(&spans) {
                black_box(split_class_tokens(black_box(&html), *span));
            }
        });
    });
}

fn bench_html_stage1(c: &mut Criterion) {
    let input = fixture(8_192);
    let mut group = c.benchmark_group("stage1/html");
    group.throughput(Throughput::Bytes(input.len() as u64));

    let mut scalar =
        Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), BackendKind::Scalar).unwrap();
    group.bench_function("scalar", |b| {
        b.iter(|| scalar.stage1(black_box(&input)));
    });

    for mode in [IndexMode::Tape, IndexMode::Masks] {
        let mut automatic = Parser::<HtmlGrammar>::new(HtmlConfig::default())
            .with_index_mode(mode)
            .unwrap();
        group.bench_function(format!("auto/{mode:?}"), |b| {
            b.iter(|| automatic.stage1(black_box(&input)));
        });
        group.bench_function(format!("auto/{mode:?}/parse"), |b| {
            b.iter(|| {
                automatic.parse_with(black_box(&input), |event| {
                    black_box(event);
                })
            });
        });
    }

    group.finish();
}

fn bench_extract_candidates_html(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline/extract_candidates/html");
    let options = ExtractOptions::default();

    for size in [32, 512, 8_192] {
        let input = fixture(size);
        group.throughput(Throughput::Bytes(input.len() as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &input, |b, input| {
            b.iter(|| {
                extract_candidates(black_box(input), DocumentKind::Html, black_box(&options))
            });
        });
    }

    group.finish();
}

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

criterion_group!(
    benches,
    bench_extract_class_spans,
    bench_split_class_tokens,
    bench_html_stage1,
    bench_extract_candidates_html
);
criterion_main!(benches);
