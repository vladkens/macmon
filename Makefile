.PHONY: prepare check test build update

CARGO_FLAGS := --release --locked

prepare:
	cargo +nightly fmt
	cargo clippy $(CARGO_FLAGS) --fix --all-targets --allow-dirty --features capi -- -D warnings
	cargo check $(CARGO_FLAGS)

check:
	cargo +nightly fmt --check
	cargo clippy $(CARGO_FLAGS) --all-targets --features capi -- -D warnings
	cargo check $(CARGO_FLAGS) --bin macmon
	cargo check $(CARGO_FLAGS) --lib --no-default-features
	cargo check $(CARGO_FLAGS) --lib --no-default-features --features capi

test:
	cargo test $(CARGO_FLAGS) --features capi

build:
	cargo build $(CARGO_FLAGS)
	ls -lh target/release/$(shell basename $(CURDIR))

update:
	cargo upgrade -i

.PHONY: loc bench remote capi capi-test xcframework

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
CAPI_TAG ?= v$(VERSION)
CAPI := out/capi
XCF_ZIP := out/CMacmon.xcframework-$(CAPI_TAG).zip

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

capi: # C library: libmacmon.dylib + include/ in out/capi; libmacmon-$(CAPI_TAG).tar.gz, libmacmon.a in out
	MACOSX_DEPLOYMENT_TARGET=11.0 cargo rustc --locked --lib --profile capi --no-default-features \
		--features capi --crate-type cdylib,staticlib \
		-- -C link-arg=-Wl,-install_name,@rpath/libmacmon.dylib
	rm -rf $(CAPI)
	mkdir -p $(CAPI)/lib
	cp -R include LICENSE $(CAPI)/
	cp target/capi/libmacmon.dylib $(CAPI)/lib/
	otool -D $(CAPI)/lib/libmacmon.dylib | grep -qx '@rpath/libmacmon.dylib'
	tar czf out/libmacmon-$(CAPI_TAG).tar.gz -C $(CAPI) include lib LICENSE
	strip -S -o out/libmacmon.a target/capi/libmacmon.a

# the static smoke test links only the modulemap's system libs, without -dead_strip, as Xcode may
capi-test: capi # MACMON_CAPI_ALLOW_NO_HW=1 accepts missing metric sources (CI VMs only)
	clang -Wall -Wextra -Werror -I$(CAPI)/include -L$(CAPI)/lib -lmacmon \
		-Wl,-rpath,@executable_path/capi/lib -o out/capi_smoke tests/capi_smoke.c
	out/capi_smoke
	clang -Wall -Wextra -Werror -I$(CAPI)/include -o out/capi_smoke_static tests/capi_smoke.c \
		out/libmacmon.a -framework CoreFoundation -framework IOKit -lIOReport -liconv
	out/capi_smoke_static

# static, so SwiftPM and Xcode link it without rpath or embedding; the modulemap links its system libs
xcframework: capi # needs Xcode: CMacmon.xcframework-$(CAPI_TAG).zip and its SPM checksum in out
	rm -rf out/CMacmon.xcframework
	xcodebuild -create-xcframework -library out/libmacmon.a -headers include \
		-output out/CMacmon.xcframework
	ditto -c -k --sequesterRsrc --keepParent out/CMacmon.xcframework $(XCF_ZIP)
	swift package --scratch-path out/.build compute-checksum $(XCF_ZIP) > $(XCF_ZIP).sha256
