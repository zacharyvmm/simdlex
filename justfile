set shell := ["bash", "-c"]

default:
    @just --list

format:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

# Run the core suite (including the NEON backend) under Miri for aarch64.
test-neon:
    cargo +nightly miri test -p simdlex-index --target aarch64-unknown-linux-gnu

bench:
    cargo bench --workspace

# Compare the JSON example's stage 1 with simdjson's (needs cmake and a C++ compiler).
bench-reference:
    examples/json/tools/simdjson-reference.sh
