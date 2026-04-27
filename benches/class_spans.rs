use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use simdlex::{
    ByteClassifier, ScalarClassifier, build_structural_index_with, extract_class_spans,
    split_class_tokens,
};

#[cfg(target_arch = "x86_64")]
use simdlex::RawSimdClassifier;

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

fn bench_classify_blocks(c: &mut Criterion) {
    let input = fixture(8_192);
    let mut group = c.benchmark_group("classify_blocks");
    group.throughput(Throughput::Bytes(input.len() as u64));

    bench_classifier::<ScalarClassifier>(&mut group, "scalar", &input);

    #[cfg(target_arch = "x86_64")]
    bench_classifier::<RawSimdClassifier>(&mut group, "raw_simd", &input);

    group.finish();
}

fn bench_build_structural_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("stage1/build_structural_index");

    for size in [32, 512, 8_192] {
        let input = fixture(size);
        group.throughput(Throughput::Bytes(input.len() as u64));

        group.bench_with_input(BenchmarkId::new("scalar", size), &input, |b, input| {
            b.iter(|| build_structural_index_with::<ScalarClassifier>(black_box(input)));
        });

        #[cfg(target_arch = "x86_64")]
        group.bench_with_input(BenchmarkId::new("raw_simd", size), &input, |b, input| {
            b.iter(|| build_structural_index_with::<RawSimdClassifier>(black_box(input)));
        });
    }

    group.finish();
}

fn bench_classifier<C: ByteClassifier>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    input: &[u8],
) {
    let len = input.len() - input.len() % C::LANES;

    group.bench_function(name, |b| {
        b.iter(|| {
            let mut structural = 0u64;
            let mut whitespace = 0u64;

            for offset in (0..len).step_by(C::LANES) {
                let classes = C::classify_block(black_box(input), offset);
                structural ^= classes.structural;
                whitespace ^= classes.whitespace;
            }

            black_box((structural, whitespace));
        });
    });
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
    bench_classify_blocks,
    bench_build_structural_index
);
criterion_main!(benches);
