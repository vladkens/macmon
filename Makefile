lint:
	cargo fmt --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cargo check --workspace --release --locked

build:
	cargo build --workspace --release
	ls -lh target/release/macmon
	ls -lh target/release/libmacmon.dylib

update:
	@# cargo install cargo-edit
	cargo upgrade -i
