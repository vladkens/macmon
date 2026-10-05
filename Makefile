.PHONY: prepare check test build update bench remote

CARGO_FLAGS := --release --locked

prepare:
	cargo fmt
	cargo clippy $(CARGO_FLAGS) --fix --all-targets --allow-dirty -- -D warnings
	cargo check $(CARGO_FLAGS)

check:
	cargo fmt --check
	cargo clippy $(CARGO_FLAGS) --all-targets -- -D warnings
	cargo check $(CARGO_FLAGS) --bin macmon
	cargo check $(CARGO_FLAGS) --lib --no-default-features

test:
	cargo test $(CARGO_FLAGS)

build:
	cargo build $(CARGO_FLAGS)
	ls -lh target/release/$(shell basename $(CURDIR))

update:
	cargo upgrade -i

bench: # compare startup time
	cargo build $(CARGO_FLAGS)
	hyperfine --warmup 3 --min-runs 60 \
		'macmon pipe -s 1 -i 100' \
		'./target/release/macmon pipe -s 1 -i 100'

remote:
	@test -n "$(host)" || (echo "Usage: make remote host=user@host" >&2; exit 1)
	@rsync -az Cargo.toml Cargo.lock Makefile src_app src_lib "$(host):macmon/"
	@ssh "$(host)" 'cd ~/macmon && cargo build $(CARGO_FLAGS) && ./target/release/macmon debug'
	@ssh "$(host)" 'cd ~/macmon && ./target/release/macmon pipe -s 1 -i 100 > /dev/null'
