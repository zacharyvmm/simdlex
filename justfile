set shell := ["bash", "-c"]

default:
    @just --list

format:
    cargo fmt --all

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

build:
    cargo build --release

bench:
    cargo bench

bench-grammars:
    cargo bench --features json-test-grammar --bench grammar_parser

bench-reference:
    tools/simdjson-reference.sh
