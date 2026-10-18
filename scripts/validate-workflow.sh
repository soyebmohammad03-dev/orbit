#!/usr/bin/env bash
# End-to-end validation of every ORBIT subsystem through its public
# interfaces (HTTP API and CLIs), on a fresh data directory.
#
#   scripts/validate-workflow.sh [DATA_DIR] [--keep]
#
# Builds orbit-api on its own (so no test-only feature is unified into it)
# and the orbit-reliability runner separately, starts the API with the
# runner enabled, and checks each step's actual response. With --keep the
# API is left running on 127.0.0.1:7878 for the Observatory (its PID is
# printed). Requires curl and jq. Nothing here is mocked or seeded with
# fabricated results: every record is produced by the system itself.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DATA=""
KEEP=0
for arg in "$@"; do
  case "$arg" in
    --keep) KEEP=1 ;;
    *) DATA="$arg" ;;
  esac
done
DATA=${DATA:-$(mktemp -d "${TMPDIR:-/tmp}/orbit-validate-XXXXXX")}
mkdir -p "$DATA"
API=http://127.0.0.1:7878/api/v1
API_BIN="$ROOT/target/debug/orbit-api"
LAB_BIN="$ROOT/target/debug/orbit-lab"
RUNNER="$ROOT/target/debug/orbit-reliability"
PASS=0
API_PID=""

step() { printf '\n== %s\n' "$*"; }
ok() { PASS=$((PASS + 1)); printf '   ok: %s\n' "$*"; }
fail() { printf '   FAILED: %s\n' "$*" >&2; exit 1; }
expect() { # expect DESCRIPTION ACTUAL EXPECTED
  if [ "$2" = "$3" ]; then ok "$1 = $2"; else fail "$1: expected $3, got $2"; fi
}
get() { curl -sf "$API$1"; }
post() { curl -sf -X POST "$API$1" -H 'content-type: application/json' -d "$2"; }

start_api() {
  ORBIT_DB_PATH="$DATA/orbit.db" ORBIT_LAB_DIR="$DATA/lab" ORBIT_BENCHMARK_DIR="" \
    ORBIT_RELIABILITY_RUNNER="$RUNNER" ORBIT_RELIABILITY_TIMEOUT_SECS=300 \
    "$API_BIN" >>"$DATA/api.log" 2>&1 &
  API_PID=$!
  for _ in $(seq 1 200); do
    curl -sf "$API/health" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  fail "the API did not start (see $DATA/api.log)"
}
stop_api() {
  [ -n "$API_PID" ] || return 0
  kill -INT "$API_PID" 2>/dev/null || true
  wait "$API_PID" 2>/dev/null || true
  API_PID=""
}
cleanup() { [ "$KEEP" = 1 ] || stop_api; }
trap cleanup EXIT

wait_finished() { # wait_finished EXPERIMENT_ID -> prints the final status
  for _ in $(seq 1 1200); do
    s=$(get "/lab/experiments/$1" | jq -r .experiment.status)
    case "$s" in completed | failed) echo "$s"; return 0 ;; esac
    sleep 0.25
  done
  fail "experiment $1 did not finish"
}

step "build (orbit-api alone; runner and CLI separately)"
(cd "$ROOT" && cargo build -q --locked -p orbit-api)
(cd "$ROOT" && cargo build -q --locked -p orbit-lab --features reliability --bins)
for m in "not-faulted" "child error: " "append_forever"; do
  if LC_ALL=C grep -a -q "$m" "$API_BIN"; then fail "orbit-api contains reliability child code ($m)"; fi
done
ok "orbit-api contains no reliability child code"

step "start the API on a fresh data directory: $DATA"
lsof -iTCP:7878 -sTCP:LISTEN >/dev/null 2>&1 && fail "port 7878 is already in use"
start_api
expect "runner enabled" "$(get /lab/reliability/scenarios | jq -r .runner.enabled)" true

step "streams, events, historical state, snapshots, integrity (scripts/seed-demo.sh)"
"$ROOT/scripts/seed-demo.sh" "$API" >/dev/null
expect "order-1042 head" "$(get /streams/order-1042 | jq -r .head_sequence)" 6
expect "status at sequence 3" "$(get '/streams/order-1042/state?at=3' | jq -r .state.status)" new
expect "status now" "$(get '/streams/order-1042/state' | jq -r .state.status)" shipped
snap=$(get /streams/order-1042/snapshots | jq -r '.[0].snapshot_id')
expect "snapshot validation" "$(get "/snapshots/$snap" | jq -r .validation.status)" usable
expect "snapshot-assisted start" "$(get '/streams/order-1042/state?snapshots=prefer' | jq -r .report.start.kind)" snapshot
expect "store verification" "$(post /verify '{}' | jq -r .valid)" true
expect "stream verification issues" "$(post /streams/counter-1/verify '{}' | jq -r .total_issues)" 0

step "differential replay: equivalence and divergence"
post /streams '{"stream_id":"counter-demo"}' >/dev/null
for e in 'incremented 2' 'decremented 5' 'incremented 1' 'incremented 4'; do
  set -- $e
  post /streams/counter-demo/events "{\"event_type\":\"counter.$1\",\"payload\":{\"by\":$2}}" >/dev/null
done
same=$(post /lab/experiments '{"kind":"differential_replay","spec":{"stream":"counter-demo","left":"orbit.counter","right":"orbit.counter"}}')
expect "self-comparison outcome" "$(echo "$same" | jq -r .outcome)" equivalent
diverged=$(post /lab/experiments '{"kind":"differential_replay","spec":{"stream":"counter-demo","left":"orbit.counter","right":"example.counter-floor-at-zero","reference":"left"}}')
DIFF_ID=$(echo "$diverged" | jq -r .id)
expect "divergent pair outcome" "$(echo "$diverged" | jq -r .outcome)" diverged
expect "first divergence boundary" "$(echo "$diverged" | jq -r .result.first_divergence.boundary)" 2
expect "artifact verification" "$(post "/lab/experiments/$DIFF_ID/verify" '{}' | jq -r .report.status)" intact
expect "reproduction" "$(post "/lab/experiments/$DIFF_ID/reproduce" '{}' | jq -r .status)" reproduced

step "reliability scenario launched through the API, run by the separate runner"
rel=$(post /lab/reliability/runs '{"scenario":"snapshot-write-interruption","parameters":{"events":3}}')
REL_ID=$(echo "$rel" | jq -r .id)
expect "accepted as" "$(echo "$rel" | jq -r .status)" created
expect "final status" "$(wait_finished "$REL_ID")" completed
expect "verdict" "$(get "/lab/experiments/$REL_ID" | jq -r .experiment.outcome)" invariants_preserved
expect "cases" "$(get "/lab/experiments/$REL_ID" | jq -r .experiment.result.summary.cases)" 6
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API/lab/reliability/runs" -H 'content-type: application/json' -d '{"scenario":"x","command":"id"}')
expect "a request outside the catalog" "$code" 400

step "performance: two compatible measurements and a comparison"
PERF=()
for _ in 1 2; do
  while [ "$(get /lab/performance/workloads | jq -r .measuring)" = true ]; do sleep 0.2; done
  p=$(post /lab/experiments '{"kind":"performance","spec":{"workload":"historical-reconstruction","input_size":500,"warmup_iterations":2,"measured_iterations":12,"seed":7}}')
  id=$(echo "$p" | jq -r .id)
  expect "measurement" "$(wait_finished "$id")" completed
  PERF+=("$id")
done
expect "evidence level of a debug build" "$(get "/lab/experiments/${PERF[0]}" | jq -r .experiment.result.evidence_level)" smoke
expect "measured by" "$(get "/lab/experiments/${PERF[0]}" | jq -r .experiment.result.environment.process)" orbit-api
cmp=$(post /lab/performance/compare "{\"candidates\":[\"${PERF[1]}\"]}")
expect "baseline found" "$(echo "$cmp" | jq -r .baseline.experiment_id)" "${PERF[0]}"
verdict=$(echo "$cmp" | jq -r '.comparisons[0].comparison.verdict')
case "$verdict" in incompatible | insufficient_samples) fail "unexpected verdict $verdict" ;; *) ok "verdict = $verdict (machine-dependent; never a gate)" ;; esac

step "evidence bundles: export, validate, import into a fresh lab, reproduce"
curl -sf "$API/lab/experiments/$DIFF_ID/bundle" -o "$DATA/bundle.json"
curl -sf "$API/lab/experiments/$DIFF_ID/bundle?history=false" -o "$DATA/bundle-no-history.json"
expect "bundle valid" "$(curl -sf -X POST "$API/lab/bundles/validate" --data-binary @"$DATA/bundle.json" | jq -r .valid)" true
expect "history" "$(curl -sf -X POST "$API/lab/bundles/validate" --data-binary @"$DATA/bundle.json" | jq -r .history)" included_and_verified
expect "authenticity of an unsigned bundle" "$(curl -sf -X POST "$API/lab/bundles/validate" --data-binary @"$DATA/bundle.json" | jq -r .authenticity.status)" unsigned
FRESH="$DATA/fresh-lab"
expect "import into a fresh lab" "$("$LAB_BIN" --lab "$FRESH" import "$DATA/bundle.json" | jq -r .newly_imported)" true
digest=$("$LAB_BIN" --lab "$FRESH" bundles | jq -r '.[0].bundle_digest')
expect "reproduction from the bundle alone" "$("$LAB_BIN" --lab "$FRESH" reproduce-bundle "$digest" | jq -r .status)" reproduced
"$LAB_BIN" --lab "$FRESH" import "$DATA/bundle-no-history.json" >/dev/null
digest2=$("$LAB_BIN" --lab "$FRESH" bundles | jq -r '.[0].bundle_digest')
expect "reproduction without history" "$("$LAB_BIN" --lab "$FRESH" reproduce-bundle "$digest2" | jq -r .status)" history_unavailable
"$LAB_BIN" keygen --key-id validate-demo --out "$DATA/signing-key.json" --public-out "$DATA/trusted-keys.json" >/dev/null
"$LAB_BIN" sign "$DATA/bundle.json" --key "$DATA/signing-key.json" --out "$DATA/bundle-signed.json" >/dev/null
expect "signed, without trust" "$("$LAB_BIN" validate "$DATA/bundle-signed.json" | jq -r .authenticity.status)" signed_untrusted
expect "signed, trusted key" "$("$LAB_BIN" validate "$DATA/bundle-signed.json" --trusted-keys "$DATA/trusted-keys.json" | jq -r .authenticity.status)" trusted
tampered=$(sed 's/"outcome":"diverged"/"outcome":"equivalent"/' "$DATA/bundle.json")
expect "tampered copy" "$(curl -s -X POST "$API/lab/bundles/validate" --data-binary "$tampered" | jq -r '.errors[0].code')" digest_mismatch
expect "import on the server" "$(curl -sf -X POST "$API/lab/bundles" --data-binary @"$DATA/bundle.json" | jq -r .local_experiment.exists)" true

step "restart: state persists"
stop_api
start_api
expect "experiment after restart" "$(get "/lab/experiments/$DIFF_ID" | jq -r .experiment.outcome)" diverged
expect "reliability result after restart" "$(get "/lab/experiments/$REL_ID" | jq -r .experiment.status)" completed
expect "imported bundles after restart" "$(get /lab/bundles | jq -r '.bundles | length')" 1
expect "events after restart" "$(get /streams/order-1042 | jq -r .head_sequence)" 6

step "no leaked processes or work files"
expect "runner processes" "$(pgrep -f "$RUNNER" | wc -l | tr -d ' ')" 0
expect "work directories" "$(find "$DATA/lab" -maxdepth 2 -path '*-work/*' | wc -l | tr -d ' ')" 0
expect "temporary artifact files" "$(find "$DATA/lab/artifacts/tmp" -type f | wc -l | tr -d ' ')" 0
expect "lock files of finished experiments" "$(find "$DATA/lab/locks" -type f | wc -l | tr -d ' ')" 0

printf '\nall %d checks passed; data in %s\n' "$PASS" "$DATA"
if [ "$KEEP" = 1 ]; then
  printf 'the API is still running (PID %s) on %s\n' "$API_PID" "$API"
  trap - EXIT
fi
