.PHONY: deps tidy fmt lint test test-cover ui build e2e check install clean

deps:
	cargo fetch --locked
	cd ui && npm ci

tidy:
	cargo metadata --locked --format-version 1 >/dev/null

fmt:
	cargo fmt --all

lint:
	node scripts/check-workflows.mjs
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings
	cd ui && npm run typecheck

test:
	cargo test --workspace --locked
	cd ui && npm test

test-cover: test

# The binary embeds ui/dist, so the UI is built first.
ui:
	cd ui && npm run build

build: ui
	cargo build --release --locked -p lest

e2e: build
	scripts/e2e.sh

check: tidy lint test build

install: ui
	cargo install --path crates/lest --locked

clean:
	cargo clean
