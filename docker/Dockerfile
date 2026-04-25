# syntax=docker/dockerfile:1.7
#
# Multi-stage build using cargo-chef for dependency caching, on
# rust:1.94-alpine3.23 (musl) for a small, statically-linked
# runtime. The runtime stage is plain alpine:3.23.
#
# Build context is the workspace root. The Dockerfile lives in
# `docker/Dockerfile`. Path deps (`pricelevel`, `ironsbe-*`) come
# from crates.io version pins, so the build context only needs the
# workspace itself — no sibling-repo COPY.
#
# Override the registry (e.g., to point at Docker Hub instead of
# AWS Public ECR) via build args:
#
#   docker build \
#     --build-arg RUST_IMAGE=rust:1.94-alpine3.23 \
#     --build-arg ALPINE_IMAGE=alpine:3.23 \
#     -f docker/Dockerfile .

ARG RUST_IMAGE=public.ecr.aws/docker/library/rust:1.95-alpine
ARG ALPINE_IMAGE=public.ecr.aws/docker/library/alpine:3.23

# ---------- chef: cargo-chef base ----------------------------------------
FROM ${RUST_IMAGE} AS chef
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconf git
RUN cargo install cargo-chef --locked
WORKDIR /work

# ---------- planner: capture the dependency graph ------------------------
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fixtures ./fixtures
RUN cargo chef prepare --recipe-path recipe.json

# ---------- builder: cook deps then build the bins -----------------------
FROM chef AS builder
COPY --from=planner /work/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY fixtures ./fixtures
RUN cargo build --release --workspace --bins

# ---------- runtime: alpine + binaries -----------------------------------
FROM ${ALPINE_IMAGE} AS runtime

# OCI image annotations — GHCR auto-links the package to the repo
# from `org.opencontainers.image.source` and surfaces title /
# description / licenses on the package page.
LABEL org.opencontainers.image.title="hft-clob-core"
LABEL org.opencontainers.image.description="Single-symbol CLOB matching engine in Rust — integer-only hot path, deterministic replay, microsecond-class tail latency."
LABEL org.opencontainers.image.source="https://github.com/joaquinbejar/hft-clob-core"
LABEL org.opencontainers.image.url="https://github.com/joaquinbejar/hft-clob-core"
LABEL org.opencontainers.image.licenses="MIT"
LABEL org.opencontainers.image.vendor="joaquinbejar"

RUN apk add --no-cache ca-certificates && \
    addgroup -g 1000 engine && \
    adduser -u 1000 -G engine -s /bin/sh -D engine

WORKDIR /app
USER engine

COPY --from=builder --chown=engine:engine /work/target/release/engine /app/engine
COPY --from=builder --chown=engine:engine /work/target/release/smoke_bench /app/smoke_bench
COPY --from=builder --chown=engine:engine /work/target/release/throughput_bench /app/throughput_bench
COPY --from=builder --chown=engine:engine /work/target/release/co_bench /app/co_bench
COPY --from=builder --chown=engine:engine /work/target/release/replay /app/replay
COPY --from=builder --chown=engine:engine /work/target/release/gen_fixture /app/gen_fixture
COPY --from=builder --chown=engine:engine /work/fixtures /app/fixtures

EXPOSE 9000

ENTRYPOINT ["/app/engine"]
CMD ["0.0.0.0:9000"]
