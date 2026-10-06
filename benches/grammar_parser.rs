use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use simdlex::{
    HtmlConfig, HtmlEmit, HtmlEvent, HtmlGrammar, HtmlStreamEvent, HtmlStreamGrammar, IndexMode,
    JsonConfig, JsonEvent, JsonGrammar, Parser,
};

fn grammar_parser(c: &mut Criterion) {
    let json = json_fixture(8_192);
    let html = br#"<div class="px-4 py-2"><span className='font-bold'></span></div>"#.repeat(1_024);

    let mut group = c.benchmark_group("json");
    group.throughput(Throughput::Bytes(json.len() as u64));
    let mut json_parser = Parser::<JsonGrammar>::new(JsonConfig::default());
    group.bench_function("json/stage1", |bench| {
        bench.iter(|| json_parser.stage1(black_box(&json)))
    });
    json_parser.stage1(&json).unwrap();
    group.bench_function("stage2/prebuilt_tape", |bench| {
        bench.iter(|| {
            let mut checksum = 0u64;
            json_parser
                .stage2_with(black_box(&json), |event| {
                    checksum = checksum.wrapping_add(json_event_checksum(event));
                })
                .unwrap();
            black_box(checksum)
        })
    });
    group.bench_function("validated_event_traversal", |bench| {
        bench.iter(|| {
            let mut checksum = 0u64;
            json_parser
                .parse_with(black_box(&json), |event| {
                    checksum = checksum.wrapping_add(json_event_checksum(event));
                })
                .unwrap();
            black_box(checksum)
        })
    });
    group.bench_function("cold_first_parse", |bench| {
        bench.iter(|| {
            let mut parser = Parser::<JsonGrammar>::new(JsonConfig::default());
            parser
                .parse_with(black_box(&json), |event| {
                    black_box(event);
                })
                .unwrap()
        })
    });
    group.finish();

    let mut group = c.benchmark_group("html");
    group.throughput(Throughput::Bytes(html.len() as u64));
    let mut html_parser = Parser::<HtmlGrammar>::new(HtmlConfig::default());
    group.bench_function("candidates", |bench| {
        bench.iter(|| {
            let mut checksum = 0u64;
            html_parser
                .parse_with(black_box(&html), |event| {
                    checksum = checksum.wrapping_add(html_event_checksum(event));
                })
                .unwrap();
            black_box(checksum)
        })
    });
    let mut token_parser = Parser::<HtmlGrammar>::new(HtmlConfig::default().emit(HtmlEmit::Tokens));
    group.bench_function("tokens", |bench| {
        bench.iter(|| {
            let mut checksum = 0u64;
            token_parser
                .parse_with(black_box(&html), |event| {
                    checksum = checksum.wrapping_add(html_event_checksum(event));
                })
                .unwrap();
            black_box(checksum)
        })
    });
    for mode in [IndexMode::Tape, IndexMode::Masks] {
        let mut stream_parser = Parser::<HtmlStreamGrammar>::new(())
            .with_index_mode(mode)
            .unwrap();
        group.bench_function(format!("stream/{mode:?}"), |bench| {
            bench.iter(|| {
                let mut checksum = 0u64;
                stream_parser
                    .parse_with(black_box(&html), |event| {
                        checksum = checksum.wrapping_add(html_stream_checksum(event));
                    })
                    .unwrap();
                black_box(checksum)
            })
        });
    }
    group.finish();
}

fn html_stream_checksum(event: HtmlStreamEvent<'_>) -> u64 {
    match event {
        HtmlStreamEvent::StartElement(element) => {
            element
                .attributes()
                .fold(bytes_checksum(13, element.name), |checksum, attribute| {
                    let value = attribute.value.map_or(&[][..], |value| value.raw);
                    checksum ^ bytes_checksum(bytes_checksum(14, attribute.name), value)
                })
        }
        HtmlStreamEvent::EndElement { name, .. } => bytes_checksum(15, name),
        HtmlStreamEvent::Text { raw, .. }
        | HtmlStreamEvent::Comment { raw, .. }
        | HtmlStreamEvent::Doctype { raw, .. }
        | HtmlStreamEvent::BogusComment { raw, .. } => bytes_checksum(16, raw),
    }
}

fn json_fixture(items: usize) -> Vec<u8> {
    let item = br#"{"id":123456,"text":"hello\\nworld","ok":true,"value":-12.5e2}"#;
    let mut json = Vec::with_capacity(items * (item.len() + 1) + 2);
    json.push(b'[');
    for index in 0..items {
        if index != 0 {
            json.push(b',');
        }
        json.extend_from_slice(item);
    }
    json.push(b']');
    json
}

fn json_event_checksum(event: JsonEvent<'_>) -> u64 {
    match event {
        JsonEvent::StartObject { offset } => 1 ^ offset as u64,
        JsonEvent::ObjectKey { raw } => bytes_checksum(2, raw.raw),
        JsonEvent::EndObject { offset } => 3 ^ offset as u64,
        JsonEvent::StartArray { offset } => 4 ^ offset as u64,
        JsonEvent::EndArray { offset } => 5 ^ offset as u64,
        JsonEvent::String { raw } => bytes_checksum(6, raw.raw),
        JsonEvent::Number { raw, .. } => bytes_checksum(7, raw),
        JsonEvent::Boolean { value, .. } => 8 + u64::from(value),
        JsonEvent::Null { .. } => 10,
    }
}

fn html_event_checksum(event: HtmlEvent<'_>) -> u64 {
    match event {
        HtmlEvent::ClassCandidate { raw, .. } => bytes_checksum(11, raw),
        HtmlEvent::ClassToken { raw, .. } => bytes_checksum(12, raw),
    }
}

fn bytes_checksum(seed: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(seed ^ bytes.len() as u64, |checksum, &byte| {
            checksum.rotate_left(5) ^ byte as u64
        })
}

criterion_group!(benches, grammar_parser);
criterion_main!(benches);
