#!/usr/bin/env bash
set -euo pipefail

coordinator_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
factory_dir="$(cd "$coordinator_dir/.." && pwd)"
factoryd_bin="$factory_dir/target/debug/factoryd"
runner_bin="$factory_dir/target/debug/factory-runner-fixture"
container_name="factory-runner-acceptance-$$"
temp_dir="$(mktemp -d /tmp/factory-runner-acceptance.XXXXXX)"
server_pid=""
runner_pid=""
runner_pids=()
runner_log=""
runner_index=0
lease_seconds=4
lifecycle_pid=""

cleanup() {
  local exit_status=$?
  if [[ $exit_status -ne 0 ]]; then
    echo "factory runner acceptance failed with status $exit_status" >&2
    for log_file in "$temp_dir"/*.log; do
      [[ -f "$log_file" ]] || continue
      echo "acceptance log: $log_file" >&2
      cat "$log_file" >&2
    done
    docker logs "$container_name" >&2 2>/dev/null || true
  fi
  for pid in "${runner_pids[@]}"; do
    if kill -0 "$pid" 2>/dev/null; then
      kill -INT "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  if [[ -n "$lifecycle_pid" ]] && kill -0 "$lifecycle_pid" 2>/dev/null; then
    kill "$lifecycle_pid" 2>/dev/null || true
    wait "$lifecycle_pid" 2>/dev/null || true
  fi
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  docker stop "$container_name" >/dev/null 2>&1 || true
  case "$temp_dir" in
    /tmp/factory-runner-acceptance.*) rm -rf -- "$temp_dir" ;;
  esac
}
trap cleanup EXIT

for command in cargo curl docker git jq; do
  command -v "$command" >/dev/null || {
    echo "required command is missing: $command" >&2
    exit 1
  }
done

assert_equal() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "$label: expected '$expected', got '$actual'" >&2
    exit 1
  fi
}

assert_one_stage_completed_event() {
  local job_id="$1"
  local events
  events="$(request_json GET "$server_base/jobs/$job_id/events?after=0&limit=100")"
  assert_equal \
    "$(jq '[.events[] | select(.kind == "stage.completed")] | length' <<<"$events")" \
    "1" \
    "stage.completed event count for $job_id"
}

request_json() {
  local method="$1"
  local url="$2"
  if [[ $# -eq 3 ]]; then
    curl -fsS -X "$method" -H 'content-type: application/json' --data "$3" "$url"
  else
    curl -fsS -X "$method" "$url"
  fi
}

create_job() {
  local operation_kind="$1"
  local max_attempts="$2"
  local body
  body="$(jq -nc --arg kind "$operation_kind" --argjson attempts "$max_attempts" '{
    kind:"factory.runner.acceptance",
    input:{},
    operations:[{kind:$kind,input:{},maxAttempts:$attempts}]
  }')"
  request_json POST "$server_base/jobs" "$body" | jq -r '.job.jobId'
}

wait_job_state() {
  local job_id="$1"
  local expected="$2"
  for _ in $(seq 1 200); do
    local state
    state="$(curl -fsS "$server_base/jobs/$job_id" 2>/dev/null | jq -r '.job.state' || true)"
    if [[ "$state" == "$expected" ]]; then
      return
    fi
    if [[ -n "$runner_pid" ]] && ! kill -0 "$runner_pid" 2>/dev/null; then
      echo "runner exited while waiting for job $job_id to reach $expected" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "job $job_id did not reach $expected" >&2
  exit 1
}

wait_runner_event() {
  local log_file="$1"
  local job_id="$2"
  local event="$3"
  for _ in $(seq 1 200); do
    if [[ -s "$log_file" ]] && jq -e -R -s --arg job "$job_id" --arg event "$event" \
      'split("\n") | map(fromjson?) | any(.jobId == $job and .event == $event)' \
      "$log_file" >/dev/null 2>&1; then
      return
    fi
    if [[ -n "$runner_pid" ]] && ! kill -0 "$runner_pid" 2>/dev/null; then
      echo "runner exited while waiting for $event on $job_id" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "runner did not emit $event for $job_id" >&2
  exit 1
}

start_server() {
  local log_file="$temp_dir/factoryd.log"
  "$factoryd_bin" --database-url "$database_url" serve --bind 127.0.0.1:0 \
    >"$log_file" 2>&1 &
  server_pid=$!
  for _ in $(seq 1 100); do
    local listening
    listening="$(jq -r 'select(.listening != null) | .listening' "$log_file" 2>/dev/null | head -n 1)"
    if [[ -n "$listening" ]] && curl -fsS "http://$listening/healthz" >/dev/null 2>&1; then
      server_base="http://$listening"
      return
    fi
    sleep 0.1
  done
  echo "factoryd did not become ready" >&2
  exit 1
}

start_runner() {
  local worker_id="$1"
  local slots="${2:-1}"
  local drain_gate="${3:-}"
  local runner_lease_seconds="${4:-$lease_seconds}"
  runner_index=$((runner_index + 1))
  runner_log="$temp_dir/runner-$runner_index.log"
  local runner_args=(
    --database-url "$database_url"
    --worker-id "$worker_id"
    --lease-seconds "$runner_lease_seconds"
    --poll-milliseconds 100
    --slots "$slots"
  )
  if [[ -n "$drain_gate" ]]; then
    runner_args+=(--drain-gate "$drain_gate")
  fi
  "$runner_bin" "${runner_args[@]}" >"$runner_log" 2>&1 &
  runner_pid=$!
  runner_pids+=("$runner_pid")
  for _ in $(seq 1 100); do
    if [[ -s "$runner_log" ]] && jq -e -R -s \
      'split("\n") | map(fromjson?) | any(.event == "ready")' \
      "$runner_log" >/dev/null 2>&1; then
      return
    fi
    if ! kill -0 "$runner_pid" 2>/dev/null; then
      echo "runner $worker_id exited before readiness" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "runner $worker_id did not become ready" >&2
  exit 1
}

stop_runner() {
  kill -INT "$runner_pid"
  for _ in $(seq 1 100); do
    local process_state
    process_state="$(ps -o stat= -p "$runner_pid" 2>/dev/null | tr -d ' ' || true)"
    if [[ -z "$process_state" || "$process_state" == Z* ]]; then
      wait "$runner_pid"
      runner_pid=""
      return
    fi
    sleep 0.1
  done
  kill -KILL "$runner_pid" 2>/dev/null || true
  wait "$runner_pid" 2>/dev/null || true
  runner_pid=""
  echo "runner did not stop cooperatively" >&2
  exit 1
}

kill_runner() {
  kill -KILL "$runner_pid"
  wait "$runner_pid" 2>/dev/null || true
  runner_pid=""
}

cd "$factory_dir"
cargo build --locked -p factory-coordinator --bin factoryd \
  --bin factory-runner-fixture --features acceptance-fixtures >/dev/null

docker run --rm -d --name "$container_name" \
  -e POSTGRES_PASSWORD=factoryd_acceptance \
  -e POSTGRES_DB=factoryd_acceptance \
  -p 127.0.0.1::5432 postgres:16-alpine >/dev/null

for _ in $(seq 1 100); do
  if docker exec "$container_name" psql -U postgres -d factoryd_acceptance \
    -Atc 'SELECT 1' >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
docker exec "$container_name" psql -U postgres -d factoryd_acceptance \
  -Atc 'SELECT 1' >/dev/null
postgres_port="$(docker port "$container_name" 5432/tcp | sed 's/.*://')"
database_url="postgresql://postgres:factoryd_acceptance@127.0.0.1:$postgres_port/factoryd_acceptance"
start_server

success_job_id="$(create_job acceptance.runner.success 1)"
start_runner acceptance-success-worker
wait_job_state "$success_job_id" succeeded
stop_runner
success_attempts="$(request_json GET "$server_base/jobs/$success_job_id/attempts")"
success_checkpoints="$(request_json GET "$server_base/jobs/$success_job_id/stage-checkpoints")"
assert_equal "$(jq -r 'length' <<<"$success_attempts")" "1" "success attempt count"
assert_equal "$(jq -r '.[0].state' <<<"$success_attempts")" "succeeded" "success attempt state"
assert_equal "$(jq -r '.[0].recoveryCause' <<<"$success_attempts")" "newOperation" "success claim cause"
assert_equal "$(jq -r '.[0].checkpoint.payload.scenario' <<<"$success_checkpoints")" "success" "success checkpoint"
assert_one_stage_completed_event "$success_job_id"

retry_job_id="$(create_job acceptance.runner.retry 2)"
start_runner acceptance-retry-worker
wait_job_state "$retry_job_id" succeeded
stop_runner
retry_attempts="$(request_json GET "$server_base/jobs/$retry_job_id/attempts")"
assert_equal "$(jq -r 'length' <<<"$retry_attempts")" "2" "retry attempt count"
assert_equal "$(jq -r '.[0].state' <<<"$retry_attempts")" "failed" "stored first failure"
assert_equal "$(jq -r '.[0].failure.detail.reason' <<<"$retry_attempts")" "acceptance fail-first" "stored retry reason"
assert_equal "$(jq -r '.[1].state' <<<"$retry_attempts")" "succeeded" "retry completion"
assert_equal "$(jq -r '.[1].recoveryCause' <<<"$retry_attempts")" "retryScheduled" "stored retry claim"
[[ "$(jq -r '.[1].resumesCheckpointId' <<<"$retry_attempts")" != "null" ]]
assert_one_stage_completed_event "$retry_job_id"

plan_retry_job_id="$(create_job acceptance.runner.plan-validation-retry 2)"
start_runner acceptance-plan-validation-retry-worker
wait_job_state "$plan_retry_job_id" succeeded
stop_runner
plan_retry_attempts="$(request_json GET "$server_base/jobs/$plan_retry_job_id/attempts")"
assert_equal "$(jq -r 'length' <<<"$plan_retry_attempts")" "2" "Plan validation retry attempt count"
assert_equal "$(jq -r '.[0].state' <<<"$plan_retry_attempts")" "failed" "Plan validation first attempt"
assert_equal "$(jq -r '.[0].failure.detail.cause' <<<"$plan_retry_attempts")" "stageExecutionRetry" "Plan validation durable retry cause"
assert_equal "$(jq -r '.[1].state' <<<"$plan_retry_attempts")" "succeeded" "Plan validation replacement attempt"
assert_equal "$(jq -r '.[1].recoveryCause' <<<"$plan_retry_attempts")" "retryScheduled" "Plan validation recovery cause"
assert_one_stage_completed_event "$plan_retry_job_id"

review_panic_job_id="$(create_job acceptance.runner.review-panic-retry 2)"
start_runner acceptance-review-panic-retry-worker
wait_job_state "$review_panic_job_id" succeeded
stop_runner
review_panic_attempts="$(request_json GET "$server_base/jobs/$review_panic_job_id/attempts")"
assert_equal "$(jq -r 'length' <<<"$review_panic_attempts")" "2" "review panic retry attempt count"
assert_equal "$(jq -r '.[0].state' <<<"$review_panic_attempts")" "failed" "review panic first attempt"
assert_equal "$(jq -r '.[0].failure.detail.cause' <<<"$review_panic_attempts")" "executorPanicked" "review panic durable retry cause"
assert_equal "$(jq -r '.[1].state' <<<"$review_panic_attempts")" "succeeded" "review panic replacement attempt"
assert_equal "$(jq -r '.[1].recoveryCause' <<<"$review_panic_attempts")" "retryScheduled" "review panic recovery cause"
assert_one_stage_completed_event "$review_panic_job_id"

cancel_job_id="$(create_job acceptance.runner.cancel 1)"
start_runner acceptance-cancel-worker
wait_runner_event "$runner_log" "$cancel_job_id" started
sleep 5
cancel_running="$(request_json GET "$server_base/jobs/$cancel_job_id/attempts")"
assert_equal "$(jq -r 'length' <<<"$cancel_running")" "1" "heartbeat attempt count"
assert_equal "$(jq -r '.[0].state' <<<"$cancel_running")" "running" "heartbeat preserves lease"
cancel_started_ns="$(date +%s%N)"
cancel_request="$(request_json POST "$server_base/jobs/$cancel_job_id/cancel")"
assert_equal "$(jq -r '.job.state' <<<"$cancel_request")" "cancelling" "running cancellation request state"
wait_job_state "$cancel_job_id" cancelled
cancel_elapsed_ms="$(( ($(date +%s%N) - cancel_started_ns) / 1000000 ))"
if (( cancel_elapsed_ms > 2000 )); then
  echo "running cancellation took ${cancel_elapsed_ms}ms; expected at most 2000ms" >&2
  exit 1
fi
cancel_probe_id="$(create_job acceptance.runner.success 1)"
wait_job_state "$cancel_probe_id" succeeded
stop_runner
cancel_attempts="$(request_json GET "$server_base/jobs/$cancel_job_id/attempts")"
assert_equal "$(jq -r '.[0].state' <<<"$cancel_attempts")" "abandoned" "cancelled attempt state"
assert_equal "$(jq -r '.[0].failure.cause' <<<"$cancel_attempts")" "jobCancelled" "durable cancellation cause"

shutdown_job_id="$(create_job acceptance.runner.shutdown 2)"
start_runner acceptance-shutdown-worker-one 1 "" 900
wait_runner_event "$runner_log" "$shutdown_job_id" started
shutdown_log="$runner_log"
stop_runner
wait_runner_event "$shutdown_log" "$shutdown_job_id" cancellationObserved
shutdown_before="$(request_json GET "$server_base/jobs/$shutdown_job_id/attempts")"
assert_equal "$(jq -r '.[0].state' <<<"$shutdown_before")" "running" "shutdown leaves lease recoverable"
shutdown_restart_started_ns="$(date +%s%N)"
start_runner acceptance-shutdown-worker-two 1 "" 900
wait_job_state "$shutdown_job_id" succeeded
shutdown_restart_elapsed_ms="$(( ($(date +%s%N) - shutdown_restart_started_ns) / 1000000 ))"
if (( shutdown_restart_elapsed_ms > 2000 )); then
  echo "graceful restart recovery took ${shutdown_restart_elapsed_ms}ms; expected at most 2000ms" >&2
  exit 1
fi
stop_runner
shutdown_attempts="$(request_json GET "$server_base/jobs/$shutdown_job_id/attempts")"
assert_equal "$(jq -r 'length' <<<"$shutdown_attempts")" "1" "shutdown logical attempt count"
assert_equal "$(jq -r '.[0].state' <<<"$shutdown_attempts")" "succeeded" "shutdown recovered state"
assert_equal "$(jq -r '.[0].recoveryCause' <<<"$shutdown_attempts")" "leaseExpired" "shutdown recovery cause"
assert_equal "$(jq -r '.[0].leaseEpoch' <<<"$shutdown_attempts")" "2" "shutdown recovery epoch"
assert_one_stage_completed_event "$shutdown_job_id"

recover_job_id="$(create_job acceptance.runner.recover 1)"
start_runner acceptance-crash-worker-one
wait_runner_event "$runner_log" "$recover_job_id" waitingForProcessKill
recover_before="$(request_json GET "$server_base/jobs/$recover_job_id/attempts")"
recover_first_attempt_id="$(jq -r '.[0].attemptId' <<<"$recover_before")"
assert_equal "$(jq -r '.[0].state' <<<"$recover_before")" "running" "leased attempt before kill"
kill_runner
sleep 5
start_runner acceptance-crash-worker-two
wait_job_state "$recover_job_id" succeeded
stop_runner
recover_attempts="$(request_json GET "$server_base/jobs/$recover_job_id/attempts")"
recover_checkpoints="$(request_json GET "$server_base/jobs/$recover_job_id/stage-checkpoints")"
assert_equal "$(jq -r 'length' <<<"$recover_attempts")" "1" "recovery logical attempt count"
assert_equal "$(jq -r '.[0].attemptId' <<<"$recover_attempts")" "$recover_first_attempt_id" "recovered attempt identity"
assert_equal "$(jq -r '.[0].state' <<<"$recover_attempts")" "succeeded" "recovered attempt state"
assert_equal "$(jq -r '.[0].recoveryCause' <<<"$recover_attempts")" "leaseExpired" "expired lease cause"
assert_equal "$(jq -r '.[0].leaseEpoch' <<<"$recover_attempts")" "2" "recovered lease epoch"
assert_equal "$(request_json GET "$server_base/jobs/$recover_job_id" | jq -r '.operations[0].maxAttempts')" "1" "crash recovery attempt budget"
assert_equal "$(jq -r '.[0].checkpoint.payload.scenario' <<<"$recover_checkpoints")" "lease-recovered" "recovery checkpoint"
assert_one_stage_completed_event "$recover_job_id"

fence_job_id="$(create_job acceptance.runner.fence 1)"
start_runner acceptance-fence-worker-one
wait_runner_event "$runner_log" "$fence_job_id" waitingForProcessKill
fence_before="$(request_json GET "$server_base/jobs/$fence_job_id/attempts")"
fence_attempt_id="$(jq -r '.[0].attemptId' <<<"$fence_before")"
kill_runner
sleep 5
start_runner acceptance-fence-worker-two
wait_runner_event "$runner_log" "$fence_job_id" newLeaseHeld
fence_current="$(request_json GET "$server_base/jobs/$fence_job_id/attempts")"
assert_equal "$(jq -r 'length' <<<"$fence_current")" "1" "fenced logical attempt count"
assert_equal "$(jq -r '.[0].attemptId' <<<"$fence_current")" "$fence_attempt_id" "fenced attempt identity"
assert_equal "$(jq -r '.[0].ownerInstanceId' <<<"$fence_current")" "acceptance-fence-worker-two" "new lease owner"
assert_equal "$(jq -r '.[0].leaseEpoch' <<<"$fence_current")" "2" "new lease epoch"
request_json POST "$server_base/jobs/$fence_job_id/cancel" >/dev/null
wait_job_state "$fence_job_id" cancelled
stop_runner
FACTORY_COORDINATOR_TEST_DATABASE_URL="$database_url" \
  cargo test -p factory-coordinator --test lease_fencing \
    expired_max_attempt_one_transfers_same_attempt_and_fences_stale_owner \
    -- --ignored --exact >/dev/null

isolation_failure_job_id="$(create_job acceptance.runner.invalid-checkpoint 1)"
isolation_success_job_id="$(create_job acceptance.runner.slow-success 1)"
start_runner acceptance-isolation-worker 2
wait_job_state "$isolation_success_job_id" succeeded
wait_job_state "$isolation_failure_job_id" failed
if ! kill -0 "$runner_pid" 2>/dev/null; then
  echo "one slot failure terminated the runner" >&2
  exit 1
fi
isolation_failure_state="$(request_json GET "$server_base/jobs/$isolation_failure_job_id" | jq -r '.job.state')"
assert_equal "$isolation_failure_state" "failed" "invalid checkpoint job state"
assert_one_stage_completed_event "$isolation_success_job_id"
stop_runner

overlap_source="$temp_dir/overlap-source"
git init -q -b main "$overlap_source"
git -C "$overlap_source" config user.name "Factory Runner Acceptance"
git -C "$overlap_source" config user.email "factory-runner@example.invalid"
printf 'workspace fencing fixture\n' >"$overlap_source/README.md"
git -C "$overlap_source" add README.md
git -C "$overlap_source" commit -q -m "workspace fencing fixture"
overlap_job_id="$(create_job acceptance.runner.workspace-overlap 1)"
overlap_workspace_body="$(jq -nc --arg repository "$overlap_source" '{
  repositoryId:"acceptance-workspace-overlap",repository:$repository,baseRef:"main"
}')"
request_json PUT "$server_base/jobs/$overlap_job_id/workspace" "$overlap_workspace_body" >/dev/null
overlap_gate="$temp_dir/release-old-runtime"
start_runner acceptance-overlap-worker-one 1 "$overlap_gate"
wait_runner_event "$runner_log" "$overlap_job_id" workspaceEntered
overlap_old_pid="$runner_pid"
overlap_old_log="$runner_log"
overlap_before="$(request_json GET "$server_base/jobs/$overlap_job_id/attempts")"
overlap_attempt_id="$(jq -r '.[0].attemptId' <<<"$overlap_before")"
docker exec "$container_name" psql -U postgres -d factoryd_acceptance -v ON_ERROR_STOP=1 \
  -c "UPDATE factory_attempts SET lease_expires_at = clock_timestamp() - interval '1 second' WHERE attempt_id = '$overlap_attempt_id'" \
  >/dev/null
wait_runner_event "$overlap_old_log" "$overlap_job_id" cancellationObserved
start_runner acceptance-overlap-worker-two 1 "$overlap_gate"
wait_runner_event "$runner_log" "$overlap_job_id" waitingForWorkspace
overlap_new_pid="$runner_pid"
overlap_new_log="$runner_log"
sleep 0.5
if jq -e -R -s --arg job "$overlap_job_id" \
  'split("\n") | map(fromjson?) | any(.jobId == $job and .event == "workspaceEntered")' \
  "$overlap_new_log" >/dev/null 2>&1; then
  echo "replacement runtime entered the workspace before the old runtime drained" >&2
  exit 1
fi
if ! kill -0 "$overlap_old_pid" 2>/dev/null; then
  echo "old runtime process died instead of draining cooperatively" >&2
  exit 1
fi

request_json PUT "$server_base/jobs/$overlap_job_id/workspace" "$overlap_workspace_body" \
  >"$temp_dir/overlap-workspace-response.json" &
lifecycle_pid=$!
sleep 0.5
if ! kill -0 "$lifecycle_pid" 2>/dev/null; then
  echo "cross-process workspace lifecycle request bypassed the active runtime lock" >&2
  exit 1
fi

touch "$overlap_gate"
wait_runner_event "$overlap_old_log" "$overlap_job_id" runtimeDrained
wait_runner_event "$overlap_new_log" "$overlap_job_id" workspaceEntered
wait_job_state "$overlap_job_id" succeeded
wait "$lifecycle_pid"
lifecycle_pid=""
runner_pid="$overlap_old_pid"
stop_runner
runner_pid="$overlap_new_pid"
stop_runner
overlap_attempts="$(request_json GET "$server_base/jobs/$overlap_job_id/attempts")"
overlap_checkpoints="$(request_json GET "$server_base/jobs/$overlap_job_id/stage-checkpoints")"
assert_equal "$(jq -r 'length' <<<"$overlap_attempts")" "1" "overlap logical attempt count"
assert_equal "$(jq -r '.[0].ownerInstanceId' <<<"$overlap_attempts")" "acceptance-overlap-worker-two" "overlap replacement owner"
assert_equal "$(jq -r '.[0].leaseEpoch' <<<"$overlap_attempts")" "2" "overlap replacement epoch"
assert_equal "$(jq -r '.[0].checkpoint.payload.scenario' <<<"$overlap_checkpoints")" "workspace-fenced" "overlap checkpoint"
assert_one_stage_completed_event "$overlap_job_id"

jq -nc \
  --arg phase runnerRecoveryAccepted \
  --arg successJobId "$success_job_id" \
  --arg retryJobId "$retry_job_id" \
  --arg planRetryJobId "$plan_retry_job_id" \
  --arg reviewPanicJobId "$review_panic_job_id" \
  --arg cancelJobId "$cancel_job_id" \
  --arg shutdownJobId "$shutdown_job_id" \
  --arg recoveredJobId "$recover_job_id" \
  --arg fenceJobId "$fence_job_id" \
  --arg isolationFailureJobId "$isolation_failure_job_id" \
  --arg isolationSuccessJobId "$isolation_success_job_id" \
  --arg overlapJobId "$overlap_job_id" \
  --argjson leaseSeconds "$lease_seconds" \
  --argjson cancelElapsedMs "$cancel_elapsed_ms" \
  --argjson shutdownRestartMs "$shutdown_restart_elapsed_ms" \
  '{phase:$phase,successJobId:$successJobId,retryJobId:$retryJobId,planRetryJobId:$planRetryJobId,reviewPanicJobId:$reviewPanicJobId,cancelJobId:$cancelJobId,shutdownJobId:$shutdownJobId,recoveredJobId:$recoveredJobId,fenceJobId:$fenceJobId,isolationFailureJobId:$isolationFailureJobId,isolationSuccessJobId:$isolationSuccessJobId,overlapJobId:$overlapJobId,leaseSeconds:$leaseSeconds,heartbeat:true,retryAttempts:2,planValidationBusinessRetry:true,reviewTaskPanicRetry:true,cancellationRequestLifecycle:true,cancellationObservationMilliseconds:$cancelElapsedMs,gracefulRestartMilliseconds:$shutdownRestartMs,recoveryLeaseEpoch:2,staleWorkerFenced:true,twoSlotIsolation:true,oldRuntimeDrainedBeforeReplacement:true,crossProcessWorkspaceLifecycleFenced:true}'
