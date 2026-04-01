.PHONY: fmt lint build update publish-check

lint:
	cargo fmt --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo check --release --locked

fmt:
	cargo fmt

build:
	cargo build --release
	ls -lh target/release/$(shell basename $(CURDIR))

update:
	@# cargo install cargo-edit
	cargo upgrade -i

publish-check:
	cargo package --list --allow-dirty
	cargo publish --dry-run --allow-dirty
