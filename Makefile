.PHONY: all ci fmt fmt-check check check-all-features check-no-default lint test test-all test-no-default doc security build publish-dry clean

all: ci

ci: fmt-check check check-all-features check-no-default lint test test-all test-no-default doc security

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

check:
	cargo check --all-targets $(CARGO_FLAGS)

check-all-features:
	cargo check --all-targets --all-features $(CARGO_FLAGS)

check-no-default:
	cargo check --all-targets --no-default-features $(CARGO_FLAGS)

lint:
	cargo clippy --all-targets --all-features $(CARGO_FLAGS) -- -D warnings

test test-all test-no-default:
	cargo test $(patsubst test-%,--%-features,$(filter-out test,$@)) $(CARGO_FLAGS)

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features $(CARGO_FLAGS)

security:
	cargo deny check --hide-inclusion-graph advisories bans sources

build:
	cargo build $(CARGO_FLAGS)

publish-dry:
	cargo publish --dry-run --locked --allow-dirty

clean:
	cargo clean
