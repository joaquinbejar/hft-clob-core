#!/usr/bin/env bash
#
# scripts/smoke-test.sh — build the docker image, launch the engine,
# run every localhost example against `localhost:9000`, and tear
# everything down at exit. Renders a summary table and writes a
# JSON results file for CI consumption.
#
# Environment knobs:
#   EXAMPLE_TIMEOUT_SECS    Per-example wall-clock timeout (default 10).
#   SMOKE_RESULTS_FILE      JSON output path (default ./smoke-results.json).
#   SMOKE_USE_EXTERNAL_DEPS Skip docker compose up/down — assume an
#                           engine is already listening on
#                           CLOB_ENGINE_ADDR (default 127.0.0.1:9000).
#   CLOB_ENGINE_ADDR        Override engine listener address.

set -euo pipefail

EXAMPLE_TIMEOUT_SECS="${EXAMPLE_TIMEOUT_SECS:-10}"
SMOKE_RESULTS_FILE="${SMOKE_RESULTS_FILE:-./smoke-results.json}"
SMOKE_USE_EXTERNAL_DEPS="${SMOKE_USE_EXTERNAL_DEPS:-0}"
CLOB_ENGINE_ADDR="${CLOB_ENGINE_ADDR:-127.0.0.1:9000}"
COMPOSE_FILE="docker/docker-compose.yml"
LOG_DIR="$(mktemp -d -t smoke-XXXXXX)"
EXAMPLES=(
    localhost_new_order_gtc_rests
    localhost_new_order_gtc_crosses
    localhost_new_order_ioc_partial
    localhost_post_only_would_cross
    localhost_cancel_order
    localhost_cancel_replace_keeps_priority
    localhost_mass_cancel
    localhost_kill_switch
    localhost_snapshot_request
    localhost_market_in_empty_book
    localhost_engine_seq_monotonic
    localhost_duplicate_order_id
)

cleanup() {
    local rc="$?"
    set +e
    if [[ "$SMOKE_USE_EXTERNAL_DEPS" != "1" ]]; then
        echo "smoke: tearing down docker compose..."
        docker compose -f "$COMPOSE_FILE" down --remove-orphans >/dev/null 2>&1 || true
    fi
    rm -rf "$LOG_DIR"
    exit "$rc"
}
trap cleanup EXIT INT TERM

build_and_start() {
    echo "smoke: building docker image..."
    docker compose -f "$COMPOSE_FILE" build engine
    echo "smoke: starting engine container..."
    docker compose -f "$COMPOSE_FILE" up -d engine
    wait_for_listener "$CLOB_ENGINE_ADDR" 30
}

wait_for_listener() {
    local addr="$1"
    local timeout="$2"
    local host="${addr%:*}"
    local port="${addr##*:}"
    echo "smoke: waiting for engine on ${addr} (timeout ${timeout}s)..."
    local deadline=$(( $(date +%s) + timeout ))
    while (( $(date +%s) < deadline )); do
        if (echo > "/dev/tcp/${host}/${port}") 2>/dev/null; then
            echo "smoke: engine listening on ${addr}"
            return 0
        fi
        sleep 1
    done
    echo "smoke: ERROR — engine did not accept on ${addr} within ${timeout}s" >&2
    return 1
}

# Build all examples in release once so the per-example loop just
# invokes the binaries.
build_examples() {
    echo "smoke: building examples (release)..."
    cargo build --release --examples -p clob-client
}

restart_engine() {
    if [[ "$SMOKE_USE_EXTERNAL_DEPS" == "1" ]]; then
        # External engine — caller is responsible for state.
        return 0
    fi
    docker compose -f "$COMPOSE_FILE" restart engine >/dev/null 2>&1 || true
    wait_for_listener "$CLOB_ENGINE_ADDR" 10 >/dev/null
}

run_one() {
    local example="$1"
    local log_file="${LOG_DIR}/${example}.log"
    local start
    start=$(date +%s)
    local rc=0
    # Each example needs a clean engine — `state accumulates` between
    # runs against the same container (orders rest on the book, kill
    # switch state persists, etc.). Restart the engine container
    # before every example so each example starts from an empty book
    # with the kill switch off.
    restart_engine
    if timeout "${EXAMPLE_TIMEOUT_SECS}" cargo run --release --quiet --example "$example" -p clob-client \
        >"$log_file" 2>&1; then
        rc=0
    else
        rc=$?
    fi
    local end
    end=$(date +%s)
    local elapsed=$(( end - start ))
    echo "$rc $elapsed $log_file"
}

main() {
    if [[ "$SMOKE_USE_EXTERNAL_DEPS" != "1" ]]; then
        build_and_start
    else
        echo "smoke: SMOKE_USE_EXTERNAL_DEPS=1 — assuming engine on ${CLOB_ENGINE_ADDR}"
        wait_for_listener "$CLOB_ENGINE_ADDR" 5
    fi

    build_examples

    local pass=0 fail=0
    local results_json="["
    local first=1

    printf "\nsmoke: per-example results:\n"
    printf "  %-50s %-10s %-10s\n" "EXAMPLE" "RESULT" "WALL_S"
    for example in "${EXAMPLES[@]}"; do
        local out
        out=$(CLOB_ENGINE_ADDR="$CLOB_ENGINE_ADDR" run_one "$example")
        local rc elapsed log_file
        rc=$(echo "$out" | awk '{print $1}')
        elapsed=$(echo "$out" | awk '{print $2}')
        log_file=$(echo "$out" | awk '{print $3}')

        local status="PASS"
        if [[ "$rc" == "124" ]]; then
            status="TIMEOUT"
            fail=$(( fail + 1 ))
        elif [[ "$rc" != "0" ]]; then
            status="FAIL"
            fail=$(( fail + 1 ))
        else
            pass=$(( pass + 1 ))
        fi
        printf "  %-50s %-10s %-10s\n" "$example" "$status" "${elapsed}s"
        if [[ "$status" != "PASS" ]]; then
            echo "  --- log ($log_file) ---"
            sed 's/^/    /' "$log_file"
        fi

        if [[ $first -eq 0 ]]; then results_json+=","; fi
        first=0
        results_json+="{\"example\":\"${example}\",\"status\":\"${status}\",\"rc\":${rc},\"elapsed_secs\":${elapsed}}"
    done
    results_json+="]"

    echo "$results_json" >"$SMOKE_RESULTS_FILE"
    printf "\nsmoke: summary  PASS=%d  FAIL=%d  results=%s\n" "$pass" "$fail" "$SMOKE_RESULTS_FILE"

    if [[ $fail -gt 0 ]]; then
        exit 1
    fi
}

main "$@"
