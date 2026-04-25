# Makefile for common tasks in the hft-clob-core workspace.
# Mirrors the OrderBook-rs Makefile layout so muscle memory transfers.

CURRENT_BRANCH := $(shell git rev-parse --abbrev-ref HEAD)
ZIP_NAME       = hft-clob-core.zip

# Default target
.PHONY: all
all: test fmt lint release

# Print the most useful targets grouped by purpose.
.PHONY: help
help:
	@echo "hft-clob-core — Makefile targets"
	@echo ""
	@echo "Build / lint / format:"
	@echo "  build              cargo build --workspace (debug)"
	@echo "  release            cargo build --release --workspace"
	@echo "  fmt / fmt-check    cargo fmt --all  /  --check"
	@echo "  lint / lint-fix    cargo clippy -D warnings  /  --fix"
	@echo "  check              CI gate: fmt-check + lint + test + release"
	@echo "  smoke              check + replay golden + smoke bench (run before PR)"
	@echo ""
	@echo "Tests:"
	@echo "  test               cargo nextest run --workspace (221 tests)"
	@echo "  test-<crate>       per-crate runner (matching, risk, engine, gateway, ...)"
	@echo "  test-verbose       --no-capture (see println! / dbg!)"
	@echo "  test-failed        only print failures"
	@echo "  test-stress-stp    run flaky STP test 30x (regression for #10 fix)"
	@echo "  test-proptest      proptest + invariants only"
	@echo ""
	@echo "Run binaries:"
	@echo "  run                engine listener on 0.0.0.0:9000"
	@echo "  run-addr ADDR=…    engine listener on a custom address"
	@echo "  replay             replay fixtures/inbound.bin and diff golden"
	@echo "  replay-with-ts     replay without --no-timestamps"
	@echo "  gen-fixture        regenerate fixtures/inbound.bin + outbound.golden"
	@echo ""
	@echo "Benchmarks:"
	@echo "  smoke-bench        smoke_bench binary (10k ops, prints percentiles)"
	@echo "  bench-quick        Criterion 1s warmup + 3s measurement (~1 min)"
	@echo "  bench-full         Criterion 5s warmup + 10s measurement (~18 min)"
	@echo "  bench-show         open Criterion HTML report"
	@echo "  bench-clean        rm -rf target/criterion"
	@echo ""
	@echo "Docker (requires Docker daemon):"
	@echo "  docker-build       build the multi-stage runtime image"
	@echo "  docker-up          docker compose up engine (foreground)"
	@echo "  docker-bench       docker compose run --rm bench"
	@echo "  docker-replay      docker compose run --rm replay"
	@echo "  docker-down        docker compose down"
	@echo ""
	@echo "Coverage / docs / misc:"
	@echo "  coverage           tarpaulin XML"
	@echo "  coverage-html      tarpaulin HTML at coverage/"
	@echo "  open-coverage      open coverage report"
	@echo "  doc / doc-open     missing-docs lint  /  cargo doc --open"
	@echo "  tree               workspace tree -L 3"
	@echo "  zip                package repo for offline review"
	@echo "  clean              cargo clean"

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

# Per-crate test runners.
.PHONY: test-domain test-wire test-matching test-risk test-engine test-gateway test-marketdata
test-domain: check-cargo-nextest
	cargo nextest run -p domain
test-wire: check-cargo-nextest
	cargo nextest run -p wire
test-matching: check-cargo-nextest
	cargo nextest run -p matching
test-risk: check-cargo-nextest
	cargo nextest run -p risk
test-engine: check-cargo-nextest
	cargo nextest run -p engine
test-gateway: check-cargo-nextest
	cargo nextest run -p gateway
test-marketdata: check-cargo-nextest
	cargo nextest run -p marketdata

# Verbose run — prints println! / dbg! output from every test.
.PHONY: test-verbose
test-verbose: check-cargo-nextest
	cargo nextest run --workspace --no-capture

# Run failing-only output. Useful after a flaky run.
.PHONY: test-failed
test-failed: check-cargo-nextest
	cargo nextest run --workspace --status-level=fail --no-fail-fast

# Determinism stress: runs the previously-flaky STP test 30 times.
# Should be 30/30 pass — any FAIL means a regression on the
# snapshot_orders FIFO fix from #10.
.PHONY: test-stress-stp
test-stress-stp: check-cargo-nextest
	@echo "Running test_stp_after_partial_fill_against_other_account 30 times..."
	@for i in $$(seq 1 30); do \
	  cargo nextest run -p matching test_stp_after_partial_fill_against_other_account 2>&1 \
	    | grep -E "PASS|FAIL" | tail -1; \
	done | sort | uniq -c

# Property-based tests only (proptest cases under matching + replay).
.PHONY: test-proptest
test-proptest: check-cargo-nextest
	cargo nextest run --workspace -E "test(/proptest_/)" -E "test(/inv_/)"

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

# Run the engine binary — listens on 0.0.0.0:9000 by default.
.PHONY: run
run:
	cargo run --release --bin engine

# Run the engine binary on a custom address.
# Usage: make run-addr ADDR=127.0.0.1:7000
ADDR ?= 0.0.0.0:9000
.PHONY: run-addr
run-addr:
	cargo run --release --bin engine -- $(ADDR)

# Run the engine binary with the matching thread pinned to a core.
# Usage: make run-pinned CORE=4
CORE ?= 0
.PHONY: run-pinned
run-pinned:
	cargo run --release --features pinned-engine --bin engine -- $(ADDR) --engine-core $(CORE)

# Replay the committed inbound fixture and diff against the golden.
# Silent output = byte-identical match.
.PHONY: replay
replay:
	cargo run --release --bin replay -- fixtures/inbound.bin /tmp/outbound.bin --no-timestamps
	@diff /tmp/outbound.bin fixtures/outbound.golden && echo "GOLDEN MATCHES"

# Replay without --no-timestamps. Two consecutive runs should still
# match (StubClock is deterministic).
.PHONY: replay-with-ts
replay-with-ts:
	cargo run --release --bin replay -- fixtures/inbound.bin /tmp/outbound-with-ts.bin

# Regenerate the inbound fixture and the matching golden file.
# Developer-only — CI never runs this.
.PHONY: gen-fixture
gen-fixture:
	cargo run --release --bin gen_fixture -- fixtures/inbound.bin
	cargo run --release --bin replay -- fixtures/inbound.bin fixtures/outbound.golden --no-timestamps
	@echo "Regenerated fixtures/inbound.bin and fixtures/outbound.golden"

# Smoke bench — 10k ops, prints p50/p99/p99.9/p99.99/max + throughput.
.PHONY: smoke-bench
smoke-bench:
	cargo run --release --bin smoke_bench

# Allocation count proof on the hot path (mechanical via dhat-rs).
# Prints allocs/op and bytes/op after warmup. Documented in BENCH.md.
.PHONY: dhat
dhat:
	cargo run --release --features hotpath-dhat --bin dhat_bench

# Sustained burst throughput — 1M ops, prints ops/sec.
.PHONY: throughput
throughput:
	cargo run --release --bin throughput_bench

# Full bench (Criterion, default budget). Allow ~18 min wall-clock.
.PHONY: bench-full
bench-full:
	cargo bench --bench add_cancel_mix

# Quick sanity bench — 1s warmup + 3s measurement, 10 samples.
.PHONY: bench-quick
bench-quick:
	cargo bench --bench add_cancel_mix -- --warm-up-time 1 --measurement-time 3 --sample-size 10

# Docker — zero-host-setup. Requires Docker daemon running.
.PHONY: docker-build
docker-build:
	docker compose -f docker/docker-compose.yml build

.PHONY: docker-up
docker-up:
	docker compose -f docker/docker-compose.yml up engine

.PHONY: docker-bench
docker-bench:
	docker compose -f docker/docker-compose.yml run --rm bench

.PHONY: docker-replay
docker-replay:
	docker compose -f docker/docker-compose.yml run --rm replay

.PHONY: docker-down
docker-down:
	docker compose -f docker/docker-compose.yml down

# End-to-end smoke: full gate + replay golden + smoke bench.
# Use this before opening a PR.
.PHONY: smoke
smoke: fmt-check lint test release replay smoke-bench
	@echo ""
	@echo "smoke OK — fmt, clippy, nextest, release build, replay golden, smoke bench all green"

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
