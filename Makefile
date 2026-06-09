.PHONY: prepare check test build update bench

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

bench:
	cargo build --release --locked
	hyperfine --warmup 1 --runs 3 --command-name old --command-name new \
		'/opt/homebrew/bin/macmon pipe --samples 100 --interval 100' \
		'./target/release/macmon pipe --samples 100 --interval 100'
