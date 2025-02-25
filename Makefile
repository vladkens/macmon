lint:
	cargo fmt --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo check --release --locked

build:
	cargo build --release
	ls -lh target/release/$(shell basename $(CURDIR))

update:
	@# cargo install cargo-edit
	cargo upgrade -i
