lint:
	cargo fmt --check
	cargo check --release --locked

build:
	cargo build --release
	ls -lh target/release/$(shell basename $(CURDIR))

update:
	@# cargo install cargo-edit
	cargo upgrade -i
