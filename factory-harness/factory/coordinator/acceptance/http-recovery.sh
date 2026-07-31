#!/usr/bin/env bash
set -euo pipefail

coordinator_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
factory_dir="$(cd "$coordinator_dir/.." && pwd)"
factoryd_bin="${FACTORYD_BIN:-$factory_dir/target/debug/factoryd}"
container_name="factoryd-http-acceptance-$$"
temp_dir="$(mktemp -d /tmp/factoryd-http-acceptance.XXXXXX)"
server_pid=""
server_index=0

cleanup() {
  local exit_status=$?
  if [[ $exit_status -ne 0 ]]; then
    echo "factoryd HTTP acceptance failed with status $exit_status" >&2
    for log_file in "$temp_dir"/server-*.log; do
      [[ -f "$log_file" ]] || continue
      echo "factoryd log: $log_file" >&2
      cat "$log_file" >&2
    done
    docker logs "$container_name" >&2 2>/dev/null || true
  fi
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  docker stop "$container_name" >/dev/null 2>&1 || true
  case "$temp_dir" in
    /tmp/factoryd-http-acceptance.*) rm -rf -- "$temp_dir" ;;
  esac
}
trap cleanup EXIT

for command in cargo curl docker python3; do
  command -v "$command" >/dev/null || {
    echo "required command is missing: $command" >&2
    exit 1
  }
done

json_get() {
  local path="$1"
  python3 -c '
import json, sys
value = json.load(sys.stdin)
for part in sys.argv[1].split("."):
    value = value[int(part)] if part.isdigit() else value[part]
if isinstance(value, bool):
    print("true" if value else "false")
elif value is None:
    print("null")
else:
    print(value)
' "$path"
}

assert_equal() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "$label: expected '$expected', got '$actual'" >&2
    exit 1
  fi
}

json_length() {
  python3 -c 'import json,sys; print(len(json.load(sys.stdin)))'
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

request_no_content() {
  local method="$1"
  local url="$2"
  local body="${3:-}"
  local status
  if [[ -n "$body" ]]; then
    status="$(curl -sS -o /dev/null -w '%{http_code}' -X "$method" \
      -H 'content-type: application/json' --data "$body" "$url")"
  else
    status="$(curl -sS -o /dev/null -w '%{http_code}' -X "$method" "$url")"
  fi
  assert_equal "$status" "204" "HTTP no-content response"
}

assert_no_claim() {
  local url="$1"
  local body="$2"
  local status
  status="$(curl -sS -o /dev/null -w '%{http_code}' -X POST \
    -H 'content-type: application/json' --data "$body" "$url")"
  assert_equal "$status" "204" "blocked operation claim"
}

start_server() {
  server_index=$((server_index + 1))
  local log_file="$temp_dir/server-$server_index.log"
  "$factoryd_bin" --database-url "$database_url" serve --bind 127.0.0.1:0 \
    >"$log_file" 2>&1 &
  server_pid=$!

  local listening=""
  for _ in $(seq 1 100); do
    if [[ -s "$log_file" ]]; then
      listening="$(head -n 1 "$log_file" | json_get listening 2>/dev/null || true)"
    fi
    if [[ -n "$listening" ]] && curl -fsS "http://$listening/healthz" >/dev/null 2>&1; then
      server_base="http://$listening"
      return
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
      echo "factoryd server $server_index exited before readiness" >&2
      cat "$log_file" >&2
      exit 1
    fi
    sleep 0.1
  done
  echo "factoryd server $server_index did not become ready" >&2
  cat "$log_file" >&2
  exit 1
}

stop_server() {
  kill -TERM "$server_pid"
  wait "$server_pid" 2>/dev/null || true
  server_pid=""
}

cd "$factory_dir"
cargo build --locked -p factory-coordinator --bin factoryd >/dev/null

docker run --rm -d --name "$container_name" \
  -e POSTGRES_PASSWORD=factoryd_acceptance \
  -e POSTGRES_DB=factoryd_acceptance \
  -p 127.0.0.1::5432 postgres:16-alpine >/dev/null

postgres_ready=false
for _ in $(seq 1 100); do
  if docker exec "$container_name" psql -U postgres -d factoryd_acceptance \
    -Atc 'SELECT 1' >/dev/null 2>&1; then
    postgres_ready=true
    break
  fi
  sleep 0.1
done
if [[ "$postgres_ready" != true ]]; then
  echo "PostgreSQL did not become ready" >&2
  exit 1
fi
postgres_port="$(docker port "$container_name" 5432/tcp | sed 's/.*://')"
database_url="postgresql://postgres:factoryd_acceptance@127.0.0.1:$postgres_port/factoryd_acceptance"

start_server

thread_id="acceptance-thread-http"
thread_state='{
  "decomposition":{"stages":["plan","execute","review"]},
  "progress":{"stage":"plan","completed":0},
  "review":{"status":"pending"},
  "remediation":{"cycle":0}
}'
thread_receipt="$(request_json PUT "$server_base/v1/threads/$thread_id/state" "$thread_state")"
assert_equal "$(json_get revision <<<"$thread_receipt")" "1" "initial thread-state revision"

job_definition='{
  "kind":"factoryd.http-recovery.acceptance",
  "input":{"marker":"factoryd-http-recovery-v1"},
  "workflowRunId":"acceptance-hatchet-run",
  "operations":[
    {"kind":"plan","input":{"stage":"plan"},"maxAttempts":3},
    {"kind":"execute","input":{"stage":"execute"},"maxAttempts":3},
    {"kind":"review","input":{"stage":"review"},"maxAttempts":3}
  ]
}'
created_job="$(request_json POST "$server_base/v1/jobs" "$job_definition")"
job_id="$(json_get job.jobId <<<"$created_job")"
plan_operation_id="$(json_get operations.0.operationId <<<"$created_job")"
execute_operation_id="$(json_get operations.1.operationId <<<"$created_job")"
review_operation_id="$(json_get operations.2.operationId <<<"$created_job")"

plan_claim="$(request_json POST \
  "$server_base/v1/operations/$plan_operation_id/claim" \
  '{"ownerInstanceId":"http-server-one","leaseSeconds":1}')"
plan_attempt_id="$(json_get attempt.attemptId <<<"$plan_claim")"

renewed="$(request_json POST "$server_base/v1/attempts/$plan_attempt_id/renew" \
  '{"ownerInstanceId":"http-server-one","leaseSeconds":1}')"
assert_equal "$(json_get attemptId <<<"$renewed")" "$plan_attempt_id" "renewed attempt"

assert_no_claim \
  "$server_base/v1/operations/$execute_operation_id/claim" \
  '{"ownerInstanceId":"out-of-order-exact","leaseSeconds":30}'
assert_no_claim \
  "$server_base/v1/recoveries/claim" \
  "{\"jobId\":\"$job_id\",\"ownerInstanceId\":\"out-of-order-generic\",\"leaseSeconds\":30}"

correlation_body="$(printf '{
  "jobId":"%s","operationId":"%s","attemptId":"%s",
  "workflowRunId":"acceptance-hatchet-run","taskRunExternalId":"plan-task",
  "requestId":"plan-request","threadId":"%s","turnId":"plan-turn","itemId":"plan-item"
}' "$job_id" "$plan_operation_id" "$plan_attempt_id" "$thread_id")"
correlation="$(request_json POST "$server_base/v1/correlations" "$correlation_body")"
correlation_id="$(json_get correlationId <<<"$correlation")"

checkpoint_body="$(printf '{
  "attemptId":"%s","kind":"stage-complete",
  "payload":{"marker":"factoryd-http-recovery-v1","threadId":"%s","turnId":"plan-turn","stage":"plan"},
  "workspaceRevision":"acceptance-plan-revision","correlationId":"%s"
}' "$plan_attempt_id" "$thread_id" "$correlation_id")"
checkpoint="$(request_json POST "$server_base/v1/checkpoints" "$checkpoint_body")"
checkpoint_id="$(json_get checkpointId <<<"$checkpoint")"

loaded_job="$(request_json GET "$server_base/v1/jobs/$job_id")"
assert_equal "$(json_get job.state <<<"$loaded_job")" "running" "job before restart"

pending_job="$(request_json POST "$server_base/v1/jobs" '{
  "kind":"factoryd.pending-request.acceptance","input":{},
  "operations":[{"kind":"human-approval","input":{},"maxAttempts":1}]
}')"
pending_job_id="$(json_get job.jobId <<<"$pending_job")"
pending_operation_id="$(json_get operations.0.operationId <<<"$pending_job")"
pending_claim="$(request_json POST \
  "$server_base/v1/operations/$pending_operation_id/claim" \
  '{"ownerInstanceId":"pending-worker","leaseSeconds":120}')"
pending_attempt_id="$(json_get attempt.attemptId <<<"$pending_claim")"
pending_body="$(printf '{
  "attemptId":"%s",
  "request":{
    "id":41,
    "method":"item/commandExecution/requestApproval",
    "params":{
      "threadId":"pending-thread","turnId":"pending-turn","itemId":"pending-item",
      "startedAtMs":1,"environmentId":null,"command":"git status","cwd":"/workspace"
    }
  }
}' "$pending_attempt_id")"
pending_record="$(request_json POST "$server_base/v1/pending-requests" "$pending_body")"
pending_request_id="$(json_get pendingRequestId <<<"$pending_record")"
assert_equal "$(json_get state <<<"$pending_record")" "pending" "registered pending request"
assert_equal "$(json_get request.id <<<"$pending_record")" "41" "numeric request id"
pending_list="$(request_json GET "$server_base/v1/pending-requests?jobId=$pending_job_id")"
assert_equal "$(json_length <<<"$pending_list")" "1" "actionable request before restart"

stop_server
sleep 2.2
start_server

pending_after_restart="$(request_json GET \
  "$server_base/v1/pending-requests/$pending_request_id")"
assert_equal "$(json_get state <<<"$pending_after_restart")" "pending" \
  "pending request after factoryd restart"
resolve_body='{
  "response":{
    "id":41,
    "method":"item/commandExecution/requestApproval",
    "response":{"decision":"accept"}
  }
}'
resolved_pending="$(request_json POST \
  "$server_base/v1/pending-requests/$pending_request_id/resolve" "$resolve_body")"
assert_equal "$(json_get state <<<"$resolved_pending")" "resolved" "resolved request state"
assert_equal "$(json_get response.response.decision <<<"$resolved_pending")" "accept" \
  "durable approval decision"
resolved_again="$(request_json POST \
  "$server_base/v1/pending-requests/$pending_request_id/resolve" "$resolve_body")"
assert_equal "$(json_get pendingRequestId <<<"$resolved_again")" "$pending_request_id" \
  "idempotent request resolution"
pending_list="$(request_json GET "$server_base/v1/pending-requests?jobId=$pending_job_id")"
assert_equal "$(json_length <<<"$pending_list")" "0" "resolved request removed from pending list"
request_no_content POST "$server_base/v1/attempts/$pending_attempt_id/complete"

rehydrated_state="$(request_json GET "$server_base/v1/threads/$thread_id/state")"
assert_equal "$(json_get revision <<<"$rehydrated_state")" "1" "rehydrated thread-state revision"
assert_equal "$(json_get state.progress.stage <<<"$rehydrated_state")" "plan" "rehydrated progress"

updated_state='{
  "decomposition":{"stages":["plan","execute","review"]},
  "progress":{"stage":"execute","completed":1},
  "review":{"status":"pending"},
  "remediation":{"cycle":0}
}'
updated_thread="$(request_json PUT "$server_base/v1/threads/$thread_id/state" "$updated_state")"
assert_equal "$(json_get revision <<<"$updated_thread")" "2" "updated thread-state revision"

recovery_claim="$(request_json POST "$server_base/v1/recoveries/claim" \
  "{\"jobId\":\"$job_id\",\"ownerInstanceId\":\"http-server-two\",\"leaseSeconds\":30}")"
recovered_attempt_id="$(json_get attempt.attemptId <<<"$recovery_claim")"
assert_equal "$(json_get selection.cause <<<"$recovery_claim")" "leaseExpired" "restart recovery cause"
assert_equal "$(json_get selection.resume.checkpoint.checkpointId <<<"$recovery_claim")" \
  "$checkpoint_id" "restart checkpoint"
request_no_content POST "$server_base/v1/attempts/$recovered_attempt_id/complete"

execute_claim="$(request_json POST \
  "$server_base/v1/operations/$execute_operation_id/claim" \
  '{"ownerInstanceId":"http-server-two","leaseSeconds":30}')"
execute_attempt_id="$(json_get attempt.attemptId <<<"$execute_claim")"
assert_equal "$(json_get selection.resume.checkpoint.checkpointId <<<"$execute_claim")" \
  "$checkpoint_id" "cross-stage checkpoint handoff"
assert_equal "$(json_get selection.checkpointCorrelation.correlation.threadId <<<"$execute_claim")" \
  "$thread_id" "cross-stage thread correlation"
request_no_content POST "$server_base/v1/attempts/$execute_attempt_id/complete"

review_claim="$(request_json POST \
  "$server_base/v1/operations/$review_operation_id/claim" \
  '{"ownerInstanceId":"http-server-two","leaseSeconds":30}')"
review_attempt_id="$(json_get attempt.attemptId <<<"$review_claim")"
request_no_content POST "$server_base/v1/attempts/$review_attempt_id/complete"

completed_job="$(request_json GET "$server_base/v1/jobs/$job_id")"
assert_equal "$(json_get job.state <<<"$completed_job")" "succeeded" "recovered job state"

failed_job="$(request_json POST "$server_base/v1/jobs" '{
  "kind":"factoryd.http-failure.acceptance","input":{},
  "operations":[{"kind":"terminal-stage","input":{},"maxAttempts":1}]
}')"
failed_job_id="$(json_get job.jobId <<<"$failed_job")"
failed_operation_id="$(json_get operations.0.operationId <<<"$failed_job")"
failed_claim="$(request_json POST "$server_base/v1/operations/$failed_operation_id/claim" \
  '{"ownerInstanceId":"http-server-two","leaseSeconds":30}')"
failed_attempt_id="$(json_get attempt.attemptId <<<"$failed_claim")"
request_no_content POST "$server_base/v1/attempts/$failed_attempt_id/fail" \
  '{"disposition":"terminal","detail":{"reason":"acceptance-terminal"}}'
failed_loaded="$(request_json GET "$server_base/v1/jobs/$failed_job_id")"
assert_equal "$(json_get job.state <<<"$failed_loaded")" "failed" "terminal failure state"

retry_job="$(request_json POST "$server_base/v1/jobs" '{
  "kind":"factoryd.http-retry.acceptance","input":{},
  "operations":[{"kind":"retry-stage","input":{},"maxAttempts":2}]
}')"
retry_job_id="$(json_get job.jobId <<<"$retry_job")"
retry_operation_id="$(json_get operations.0.operationId <<<"$retry_job")"
retry_claim_one="$(request_json POST "$server_base/v1/operations/$retry_operation_id/claim" \
  '{"ownerInstanceId":"http-server-two","leaseSeconds":30}')"
retry_attempt_one="$(json_get attempt.attemptId <<<"$retry_claim_one")"
request_no_content POST "$server_base/v1/attempts/$retry_attempt_one/fail" \
  '{"disposition":"retryAt","retryAt":"1970-01-01T00:00:00Z","detail":{"reason":"acceptance-retry"}}'
retry_claim_two="$(request_json POST "$server_base/v1/operations/$retry_operation_id/claim" \
  '{"ownerInstanceId":"http-server-two","leaseSeconds":30}')"
assert_equal "$(json_get selection.cause <<<"$retry_claim_two")" "retryScheduled" "retry cause"
retry_attempt_two="$(json_get attempt.attemptId <<<"$retry_claim_two")"
request_no_content POST "$server_base/v1/attempts/$retry_attempt_two/complete"
retry_loaded="$(request_json GET "$server_base/v1/jobs/$retry_job_id")"
assert_equal "$(json_get job.state <<<"$retry_loaded")" "succeeded" "retry job state"

inactive_job="$(request_json POST "$server_base/v1/jobs" '{
  "kind":"factoryd.inactive-request.acceptance","input":{},
  "operations":[{"kind":"human-input","input":{},"maxAttempts":1}]
}')"
inactive_job_id="$(json_get job.jobId <<<"$inactive_job")"
inactive_operation_id="$(json_get operations.0.operationId <<<"$inactive_job")"
inactive_claim="$(request_json POST \
  "$server_base/v1/operations/$inactive_operation_id/claim" \
  '{"ownerInstanceId":"inactive-worker","leaseSeconds":1}')"
inactive_attempt_id="$(json_get attempt.attemptId <<<"$inactive_claim")"
inactive_body="$(printf '{
  "attemptId":"%s",
  "request":{
    "id":"human-input-1",
    "method":"item/tool/requestUserInput",
    "params":{"threadId":"inactive-thread","turnId":"inactive-turn","itemId":"inactive-item","questions":[]}
  }
}' "$inactive_attempt_id")"
inactive_record="$(request_json POST "$server_base/v1/pending-requests" "$inactive_body")"
inactive_request_id="$(json_get pendingRequestId <<<"$inactive_record")"
sleep 1.2
inactive_loaded="$(request_json GET "$server_base/v1/pending-requests/$inactive_request_id")"
assert_equal "$(json_get state <<<"$inactive_loaded")" "inactive" "expired request state"
inactive_status="$(curl -sS -o "$temp_dir/inactive-resolution.json" -w '%{http_code}' \
  -X POST -H 'content-type: application/json' \
  --data '{"response":{"id":"human-input-1","method":"item/tool/requestUserInput","response":{"answers":{}}}}' \
  "$server_base/v1/pending-requests/$inactive_request_id/resolve")"
assert_equal "$inactive_status" "409" "inactive request resolution"
inactive_list="$(request_json GET "$server_base/v1/pending-requests?jobId=$inactive_job_id")"
assert_equal "$(json_length <<<"$inactive_list")" "0" "inactive request removed from pending list"

printf '{"phase":"httpRecoveryAccepted","jobId":"%s","checkpointId":"%s","threadRevision":2,"recoveredAttemptId":"%s","pendingRequestId":"%s","pendingRestart":"resolved","inactiveResolution":"rejected","finalJobState":"succeeded","terminalFailure":"failed","retryFinalState":"succeeded"}\n' \
  "$job_id" "$checkpoint_id" "$recovered_attempt_id" "$pending_request_id"
