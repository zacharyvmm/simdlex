use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use simdlex_json::{JsonConfig, JsonEvent, JsonParser};

fn json(c: &mut Criterion) {
    let json = json_fixture(8_192);

    let mut group = c.benchmark_group("json");
    group.throughput(Throughput::Bytes(json.len() as u64));
    let mut json_parser = JsonParser::new(JsonConfig::default());
    group.bench_function("stage1", |bench| {
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
    group.finish();
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

fn bytes_checksum(seed: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(seed ^ bytes.len() as u64, |checksum, &byte| {
            checksum.rotate_left(5) ^ byte as u64
        })
}

criterion_group!(benches, json);
criterion_main!(benches);
