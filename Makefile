lint:
	cargo fmt --check
	cargo check --release --locked

update:
	@# cargo install cargo-edit
	cargo upgrade -i
