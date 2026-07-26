use std::fs;
use std::hint::black_box;
use std::time::Instant;

use simdlex::{JsonConfig, JsonEvent, JsonGrammar, Parser, Span};

fn main() {
    let mut arguments = std::env::args().skip(1);
    let iterations: usize = arguments
        .next()
        .as_deref()
        .unwrap_or("200")
        .parse()
        .expect("iteration count must be an integer");
    let files: Vec<String> = arguments.collect();
    if files.is_empty() {
        eprintln!("usage: reference_bench [iterations] <json-file>...");
        std::process::exit(2);
    }

    for file in files {
        let input = fs::read(&file).expect("could not read JSON corpus");
        let mut parser = Parser::<JsonGrammar>::with_capacity(JsonConfig::default(), input.len())
            .expect("could not reserve parser capacity");

        let stage1_start = Instant::now();
        for _ in 0..iterations {
            black_box(
                parser
                    .stage1(black_box(&input))
                    .expect("invalid JSON stage 1"),
            );
        }
        let stage1_elapsed = stage1_start.elapsed();
        let structural_count = parser.structural_offsets().len();

        let traversal_start = Instant::now();
        let mut checksum = 0u64;
        for _ in 0..iterations {
            parser
                .parse_with(black_box(&input), |event| {
                    checksum = checksum.wrapping_add(event_checksum(&input, event));
                })
                .expect("invalid JSON traversal");
        }
        let traversal_elapsed = traversal_start.elapsed();
        black_box(checksum);

        let bytes = input.len() as f64 * iterations as f64;
        println!(
            "{}\tstage1={:.3} GB/s\ttraversal={:.3} GB/s\tdensity={:.4}\t\
             tape={} B\tcapacity={} B\tchecksum={checksum}",
            file,
            bytes / stage1_elapsed.as_secs_f64() / 1e9,
            bytes / traversal_elapsed.as_secs_f64() / 1e9,
            structural_count as f64 / input.len() as f64,
            std::mem::size_of_val(parser.structural_offsets()),
            parser.tape_capacity_bytes(),
        );
    }
}

fn event_checksum(input: &[u8], event: JsonEvent<'_>) -> u64 {
    match event {
        JsonEvent::StartObject { .. } => 1,
        JsonEvent::ObjectKey { raw } => bytes_checksum(2, raw.raw),
        JsonEvent::EndObject { .. } => 3,
        JsonEvent::StartArray { .. } => 4,
        JsonEvent::EndArray { .. } => 5,
        JsonEvent::String { raw } => bytes_checksum(6, raw.raw),
        JsonEvent::Number { raw, .. } => bytes_checksum(7, raw),
        JsonEvent::Boolean { value, span } => {
            bytes_checksum(8 + u64::from(value), slice(input, span))
        }
        JsonEvent::Null { span } => bytes_checksum(10, slice(input, span)),
    }
}

fn slice(input: &[u8], span: Span) -> &[u8] {
    &input[span.start as usize..span.end as usize]
}

fn bytes_checksum(seed: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(seed ^ bytes.len() as u64, |checksum, &byte| {
            checksum.rotate_left(5) ^ byte as u64
        })
}
