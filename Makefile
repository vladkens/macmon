.PHONY: prepare check test build update

LINT := uv run --script https://raw.githubusercontent.com/vladkens/lint/main/rust.py
CARGO_FLAGS := --release --locked

prepare:
	cargo +nightly fmt
	$(LINT) fix
	cargo clippy $(CARGO_FLAGS) --fix --all-targets --allow-dirty -- -D warnings
	cargo check $(CARGO_FLAGS)

check:
	cargo +nightly fmt --check
	cargo clippy $(CARGO_FLAGS) --all-targets -- -D warnings
	$(LINT)
	cargo check $(CARGO_FLAGS) --bin macmon
	cargo check $(CARGO_FLAGS) --lib --no-default-features

test:
	cargo test $(CARGO_FLAGS)

build:
	cargo build $(CARGO_FLAGS)
	ls -lh target/release/macmon

update:
	cargo upgrade -i

.PHONY: loc bench remote

loc: # production lines, tests excluded; REF=origin/main compares with a git ref
	@python3 scripts/loc.py $(REF)

bench: # compare startup time
	cargo build $(CARGO_FLAGS)
	hyperfine --warmup 3 --min-runs 60 \
		'macmon pipe -s 1 -i 100' \
		'./target/release/macmon pipe -s 1 -i 100'

remote:
	@test -n "$(host)" || (echo "Usage: make remote host=user@host" >&2; exit 1)
	@rsync -az Cargo.toml Cargo.lock Makefile src "$(host):macmon/"
	@ssh "$(host)" 'cd ~/macmon && cargo build $(CARGO_FLAGS) && ./target/release/macmon debug'
	@ssh "$(host)" 'cd ~/macmon && ./target/release/macmon pipe -s 1 -i 100 > /dev/null'
