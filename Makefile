.PHONY: help build test fmt lint doc check smoke clean all

help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'

build:  ## Build the project
	cargo build --workspace

test:  ## Run tests
	cargo nextest run --all-features --workspace

fmt:  ## Format code
	cargo fmt --all

lint:  ## Run clippy
	cargo clippy --all-targets --all-features --workspace -- -D warnings

doc:  ## Build documentation
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features --workspace

check:  ## Fast compile check
	cargo check --all-targets --all-features --workspace

smoke:  ## Run the full gate (what CI runs)
	./scripts/smoke

clean:  ## Clean artifacts
	cargo clean

all: smoke  ## Run full CI
