# syntax=docker/dockerfile:1.7
#
# Multi-stage build: rust:slim builder → debian:slim runtime.
# Build context is the workspace root (`docker compose --build` runs
# from the repo top-level).
#
# `pricelevel` and `ironsbe-*` are crates.io version pins, not local
# path dependencies, so the build context only needs the workspace
# itself — no sibling-repo COPY required.

ARG DEBIAN_VERSION=bookworm-slim

# ---------- builder stage --------------------------------------------------
# Use the latest stable rust slim image so the build does not depend on
# a specific minor version being mirrored. `rust-toolchain.toml` pins
# the channel to `stable`, so this image will always satisfy the
# workspace's MSRV.
FROM rust:slim-bookworm AS builder
WORKDIR /work

# System deps for the rust toolchain. `pkg-config` + `libssl-dev` cover
# the common-case TLS-backed transitive crates; the workspace itself
# does not pull them today, but pinning here keeps the image
# reproducible across dependency churn.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        pkg-config \
        libssl-dev \
        && rm -rf /var/lib/apt/lists/*

# Bring in the workspace. We deliberately copy the whole tree (not a
# Cargo.toml-only first stage) because the cost of a clean rebuild
# is tolerable for the smoke compose; cargo-chef-style caching is a
# follow-up if reviewers report build time pain.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fixtures ./fixtures

# Build only the binaries the runtime image needs. The bench / replay
# binaries land in the same artifact target dir.
RUN cargo build --release --workspace --bins

# ---------- runtime stage --------------------------------------------------
FROM debian:${DEBIAN_VERSION} AS runtime
WORKDIR /app

# Minimal runtime: ca-certificates only. The engine binary is statically
# linked against everything except glibc; debian:bookworm-slim ships
# glibc 2.36 which matches the rust:slim builder.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        && rm -rf /var/lib/apt/lists/* \
        && useradd --create-home --uid 1000 engine

USER engine

# Copy the produced binaries plus the replay fixtures.
COPY --from=builder --chown=engine:engine /work/target/release/engine /app/engine
COPY --from=builder --chown=engine:engine /work/target/release/smoke_bench /app/smoke_bench
COPY --from=builder --chown=engine:engine /work/target/release/throughput_bench /app/throughput_bench
COPY --from=builder --chown=engine:engine /work/target/release/co_bench /app/co_bench
COPY --from=builder --chown=engine:engine /work/target/release/replay /app/replay
COPY --from=builder --chown=engine:engine /work/target/release/gen_fixture /app/gen_fixture
COPY --from=builder --chown=engine:engine /work/fixtures /app/fixtures

EXPOSE 9000

# Default launches the engine listener. Override with
# `docker compose run bench` (see compose file).
ENTRYPOINT ["/app/engine"]
CMD ["0.0.0.0:9000"]
