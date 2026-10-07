.PHONY: deps tidy fmt lint test test-cover build check install clean

deps:
	cargo fetch --locked

tidy:
	cargo metadata --locked --format-version 1 >/dev/null

fmt:
	cargo fmt --all

lint:
	node scripts/check-workflows.mjs
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

test:
	cargo test --workspace --locked

test-cover: test

build:
	cargo build --release --locked -p lest

check: tidy lint test build

install:
	cargo install --path crates/lest --locked

clean:
	cargo clean
