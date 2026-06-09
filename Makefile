.PHONY: prepare check test build update publish-check

prepare:
	cargo fmt
	cargo clippy --fix --all-targets --locked --allow-dirty -- -D warnings
	cargo check --release --locked

check:
	cargo fmt --check
	cargo clippy --all-targets --locked -- -D warnings
	cargo check --release --locked

test:
	cargo test --locked

build:
	cargo build --release
	ls -lh target/release/$(shell basename $(CURDIR))

update:
	cargo upgrade -i

publish-check:
	cargo package --list --allow-dirty
	cargo publish --dry-run --allow-dirty
