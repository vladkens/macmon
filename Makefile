.PHONY: prepare check test build update bench remote

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

bench: # compare startup time
	cargo build --release --locked
	hyperfine --warmup 3 --min-runs 60 \
		'macmon pipe -s 1 -i 100' \
		'./target/release/macmon pipe -s 1 -i 100'

remote:
	@test -n "$(host)" || (echo "Usage: make remote host=user@host" >&2; exit 1)
	@rsync -az Cargo.toml Cargo.lock Makefile src "$(host):macmon/"
	@ssh "$(host)" 'cd ~/macmon && cargo build --release --locked && ./target/release/macmon debug'
	@ssh "$(host)" 'cd ~/macmon && ./target/release/macmon pipe -s 1 -i 100 > /dev/null'
