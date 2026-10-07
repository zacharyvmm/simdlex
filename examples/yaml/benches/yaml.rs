use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use simdlex_yaml::{YamlConfig, YamlEvent, YamlParser, decode_yaml_scalar};

fn yaml(c: &mut Criterion) {
    let yaml = yaml_fixture(4_096);
    let mut group = c.benchmark_group("yaml");
    group.throughput(Throughput::Bytes(yaml.len() as u64));

    let mut parser = YamlParser::new(YamlConfig::default());
    parser.stage1(&yaml).unwrap();
    eprintln!(
        "yaml stage 1: {} input bytes, {} quotes on the tape, {} index bytes",
        yaml.len(),
        parser.index().tape().len(),
        parser.index().bytes()
    );
    group.bench_function("stage1", |bench| {
        bench.iter(|| parser.stage1(black_box(&yaml)))
    });

    group.bench_function("events", |bench| {
        bench.iter(|| {
            let mut checksum = 0u64;
            parser
                .parse_with(black_box(&yaml), |event| {
                    if let YamlEvent::Scalar(scalar) = event {
                        checksum = checksum.wrapping_add(bytes_checksum(17, scalar.raw));
                    }
                })
                .unwrap();
            black_box(checksum)
        })
    });
    let mut scratch = String::new();
    group.bench_function("events_decoded", |bench| {
        bench.iter(|| {
            let mut checksum = 0u64;
            parser
                .parse_with(black_box(&yaml), |event| {
                    if let YamlEvent::Scalar(scalar) = event {
                        let value = decode_yaml_scalar(&scalar, &mut scratch).unwrap();
                        checksum = checksum.wrapping_add(bytes_checksum(17, value.as_bytes()));
                    }
                })
                .unwrap();
            black_box(checksum)
        })
    });

    struct Count(u64);
    impl yaml_rust2::parser::EventReceiver for Count {
        fn on_event(&mut self, event: yaml_rust2::parser::Event) {
            if let yaml_rust2::parser::Event::Scalar(value, ..) = event {
                self.0 = self.0.wrapping_add(bytes_checksum(17, value.as_bytes()));
            }
        }
    }
    let text = std::str::from_utf8(&yaml).unwrap();
    group.bench_function("yaml_rust2_events", |bench| {
        bench.iter(|| {
            let mut count = Count(0);
            yaml_rust2::parser::Parser::new_from_str(black_box(text))
                .load(&mut count, true)
                .unwrap();
            black_box(count.0)
        })
    });
    group.finish();
}
fn yaml_fixture(items: usize) -> Vec<u8> {
    let mut yaml = String::new();
    for index in 0..items {
        yaml.push_str(&format!(
            "- id: {index}\n  name: \"service-{index}\"\n  enabled: true\n  \
             tags: [web, api, 'v2']\n  description: >\n    A folded\n    description line.\n  \
             config:\n    retries: 3\n    url: http://example.com/{index}\n"
        ));
    }
    yaml.into_bytes()
}
fn bytes_checksum(seed: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(seed ^ bytes.len() as u64, |checksum, &byte| {
            checksum.rotate_left(5) ^ byte as u64
        })
}

criterion_group!(benches, yaml);
criterion_main!(benches);
