set shell := ["bash", "-c"]

default:
    @just --list

format:
    cargo fmt --all

lint:
    cargo clippy --all-targets --all-features -- -D warnings

build:
    cargo build --release