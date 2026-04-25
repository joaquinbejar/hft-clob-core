# Makefile for common tasks in the hft-clob-core workspace.
# Mirrors the OrderBook-rs Makefile layout so muscle memory transfers.

CURRENT_BRANCH := $(shell git rev-parse --abbrev-ref HEAD)
ZIP_NAME       = hft-clob-core.zip

# Default target
.PHONY: all
all: test fmt lint release

# Build the project
.PHONY: build
build:
	cargo build --workspace

.PHONY: release
release:
	cargo build --release --workspace

# Run tests via nextest (preferred by matching codebase over `cargo test`).
# `--no-tests=pass` tolerates empty crates during early bring-up;
# remove once every crate carries at least one test.
.PHONY: test
test: check-cargo-nextest
	RUST_LOG=warn cargo nextest run --workspace --no-tests=pass

# Plain `cargo test` fallback for machines without nextest installed.
.PHONY: test-std
test-std:
	RUST_LOG=warn cargo test --workspace

# Format the code
.PHONY: fmt
fmt:
	cargo fmt --all

# Check formatting (read-only — CI gate).
.PHONY: fmt-check
fmt-check:
	cargo fmt --all --check

# Run Clippy for linting. `-D warnings` is the project bar.
.PHONY: lint
lint:
	cargo clippy --all-targets -- -D warnings

.PHONY: lint-fix
lint-fix:
	cargo clippy --fix --all-targets --allow-dirty --allow-staged -- -D warnings

# Clean the project
.PHONY: clean
clean:
	cargo clean

# Pre-commit / CI gate: fmt-check + clippy + nextest + release.
.PHONY: check
check: fmt-check lint test release

# Run the engine binary (once crates/engine has a [[bin]] target — issue #12/#16).
.PHONY: run
run:
	cargo run --release --bin engine

# Run the replay binary (issue #16).
.PHONY: replay
replay:
	cargo run --release --bin replay -- fixtures/inbound.bin /tmp/outbound.bin --no-timestamps

.PHONY: fix
fix:
	cargo fix --workspace --allow-staged --allow-dirty

# Full pre-push: apply fixes, format, lint-fix, run tests, release build, doc check.
.PHONY: pre-push
pre-push: fix fmt lint-fix test release doc

# Lint for missing docs on public items (lightweight `cargo doc` proxy).
.PHONY: doc
doc:
	cargo clippy --all-targets -- -W missing-docs

.PHONY: doc-open
doc-open:
	cargo doc --workspace --no-deps --open

.PHONY: create-doc
create-doc:
	cargo doc --workspace --no-deps --document-private-items

# Coverage via cargo-tarpaulin. Excludes benches and generated files.
.PHONY: coverage
coverage: check-cargo-tarpaulin
	mkdir -p coverage
	RUST_LOG=warn cargo tarpaulin \
		--workspace --all-features --timeout 120 \
		--exclude-files 'crates/*/benches/**' \
		--exclude-files 'target/**' \
		--out Xml

.PHONY: coverage-html
coverage-html: check-cargo-tarpaulin
	mkdir -p coverage
	RUST_LOG=warn cargo tarpaulin \
		--workspace --all-features --timeout 120 \
		--exclude-files 'crates/*/benches/**' \
		--exclude-files 'target/**' \
		--verbose --out Html --output-dir coverage

.PHONY: coverage-json
coverage-json: check-cargo-tarpaulin
	mkdir -p coverage
	RUST_LOG=warn cargo tarpaulin \
		--workspace --all-features --timeout 120 \
		--exclude-files 'crates/*/benches/**' \
		--exclude-files 'target/**' \
		--verbose --out Json --output-dir coverage

.PHONY: open-coverage
open-coverage:
	@if [ -f coverage/tarpaulin-report.html ]; then \
		if command -v open > /dev/null; then open coverage/tarpaulin-report.html; \
		else echo "Open coverage/tarpaulin-report.html in your browser"; fi; \
	else echo "coverage/tarpaulin-report.html not found. Run 'make coverage' first."; fi

# Benchmarks via cargo-criterion. HDR histogram reporting lives inside
# each bench binary — detailed methodology documented in BENCH.md (issue #17).
.PHONY: bench
bench: check-cargo-criterion
	cargo criterion --workspace --output-format=quiet

.PHONY: bench-show
bench-show:
	@if [ -f target/criterion/reports/index.html ]; then \
		if command -v open > /dev/null; then open target/criterion/reports/index.html; \
		else echo "Open target/criterion/reports/index.html in your browser"; fi; \
	else echo "target/criterion/reports/index.html not found. Run 'make bench' first."; fi

.PHONY: bench-save
bench-save: check-cargo-criterion
	cargo criterion --workspace --output-format=quiet \
		--history-id v0.1.0 --history-description "First tagged run"

.PHONY: bench-compare
bench-compare: check-cargo-criterion
	cargo criterion --workspace --output-format=verbose

.PHONY: bench-json
bench-json: check-cargo-criterion
	cargo criterion --workspace --message-format=json

.PHONY: bench-clean
bench-clean:
	rm -rf target/criterion

# Git log for the current branch vs main.
.PHONY: git-log
git-log:
	@if [ "$(CURRENT_BRANCH)" = "HEAD" ]; then \
		echo "You are in a detached HEAD state. Please check out a branch."; \
		exit 1; \
	fi; \
	echo "Showing git log for branch $(CURRENT_BRANCH) against main:"; \
	git log main..$(CURRENT_BRANCH) --pretty=full

# Package repo as a zip for offline review.
.PHONY: zip
zip:
	@echo "Creating $(ZIP_NAME) without target/, Cargo.lock, and hidden files..."
	@find . -type f \
		! -path "*/target/*" \
		! -path "./.*" \
		! -name "Cargo.lock" \
		! -name ".*" \
		| zip -@ $(ZIP_NAME)
	@echo "$(ZIP_NAME) created successfully."

# Run GitHub Actions locally via `act`. Requires DOCKER_HOST to be set.
.PHONY: workflow-build
workflow-build:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job build \
		-P ubuntu-latest=catthehacker/ubuntu:latest

.PHONY: workflow-lint
workflow-lint:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job lint \
		-P ubuntu-latest=catthehacker/ubuntu:latest

.PHONY: workflow-test
workflow-test:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job test \
		-P ubuntu-latest=catthehacker/ubuntu:latest

.PHONY: workflow-coverage
workflow-coverage:
	DOCKER_HOST="$${DOCKER_HOST}" act push --job coverage \
		-P ubuntu-latest=catthehacker/ubuntu:latest \
		--privileged

.PHONY: workflow
workflow: workflow-build workflow-lint workflow-test workflow-coverage

# Tree view of the workspace, skipping generated / vendored content.
.PHONY: tree
tree:
	tree -I 'target|.idea|.run|.DS_Store|Cargo.lock|*.zip|*.html|*.xml|*.json|*.bin|coverage|fixtures' -a -L 3

# --- tool presence helpers ------------------------------------------------

.PHONY: check-cargo-nextest
check-cargo-nextest:
	@command -v cargo-nextest > /dev/null || (echo "Installing cargo-nextest..."; cargo install cargo-nextest --locked)

.PHONY: check-cargo-tarpaulin
check-cargo-tarpaulin:
	@command -v cargo-tarpaulin > /dev/null || (echo "Installing cargo-tarpaulin..."; cargo install cargo-tarpaulin --locked)

.PHONY: check-cargo-criterion
check-cargo-criterion:
	@command -v cargo-criterion > /dev/null || (echo "Installing cargo-criterion..."; cargo install cargo-criterion --locked)
