#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
run_root="$(mktemp -d /tmp/factory-workspace-acceptance.XXXXXX)"
container="factory-workspace-acceptance-$(date +%s)-$$"
factoryd_pid=""

cleanup() {
  if [[ -n "$factoryd_pid" ]]; then
    kill "$factoryd_pid" 2>/dev/null || true
    wait "$factoryd_pid" 2>/dev/null || true
  fi
  docker rm -f "$container" >/dev/null 2>&1 || true
  rm -rf -- "$run_root"
}
trap cleanup EXIT

docker run -d --name "$container" \
  -e POSTGRES_USER=factory \
  -e POSTGRES_PASSWORD=factory \
  -e POSTGRES_DB=factory \
  -p 127.0.0.1::5432 \
  postgres:16-alpine >/dev/null
postgres_port="$(docker port "$container" 5432/tcp | sed 's/.*://')"
database_url="postgresql://factory:factory@127.0.0.1:${postgres_port}/factory"

for _ in $(seq 1 60); do
  if docker exec "$container" pg_isready -U factory >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
docker exec "$container" pg_isready -U factory >/dev/null

source_repo="$run_root/source"
mkdir -p "$source_repo"
git -C "$source_repo" init -b main >/dev/null
git -C "$source_repo" config user.name "Factory Acceptance"
git -C "$source_repo" config user.email "factory-acceptance@example.invalid"
printf '%s\n' 'workspace-source' >"$source_repo/README.md"
git -C "$source_repo" add README.md
git -C "$source_repo" commit -m "workspace source" >/dev/null
source_revision="$(git -C "$source_repo" rev-parse HEAD)"

cargo build --locked -p factory-coordinator --bin factoryd >/dev/null
factoryd="$repo_root/target/debug/factoryd"
server_log="$run_root/factoryd.log"

start_factoryd() {
  : >"$server_log"
  FACTORY_WORKSPACE_ROOT="$run_root/workspaces" \
    "$factoryd" --database-url "$database_url" serve --bind 127.0.0.1:0 \
    >"$server_log" 2>&1 &
  factoryd_pid=$!
  for _ in $(seq 1 80); do
    if [[ -s "$server_log" ]] && jq -e -s 'map(select(.listening != null)) | length > 0' "$server_log" >/dev/null 2>&1; then
      break
    fi
    sleep 0.25
  done
  local listening
  listening="$(jq -r -s 'map(select(.listening != null)) | last | .listening' "$server_log")"
  base_url="http://${listening}"
  curl -fsS "$base_url/healthz" >/dev/null
}

stop_factoryd() {
  kill -INT "$factoryd_pid"
  wait "$factoryd_pid"
  factoryd_pid=""
}

start_factoryd
job="$({
  jq -n '{kind:"workspace.acceptance",input:{},operations:[{kind:"codex.execute",input:{},maxAttempts:1}]}'
} | curl -fsS -X POST "$base_url/v1/jobs" -H 'content-type: application/json' --data-binary @-)"
job_id="$(jq -r '.job.jobId' <<<"$job")"
workspace="$({
  jq -n --arg repository "$source_repo" '{repository:$repository,baseRef:"main"}'
} | curl -fsS -X PUT "$base_url/v1/jobs/$job_id/workspace" -H 'content-type: application/json' --data-binary @-)"
workspace_root="$(jq -r '.root' <<<"$workspace")"

[[ "$(jq -r '.revision' <<<"$workspace")" == "$source_revision" ]]
[[ "$(jq -r '.state' <<<"$workspace")" == "active" ]]
[[ "$(git -C "$workspace_root" branch --show-current)" == "factory/$job_id" ]]
[[ "$(git -C "$workspace_root" rev-parse HEAD)" == "$source_revision" ]]
[[ "$(<"$workspace_root/README.md")" == "workspace-source" ]]

stop_factoryd
start_factoryd
reloaded="$(curl -fsS "$base_url/v1/jobs/$job_id/workspace")"
[[ "$(jq -r '.root' <<<"$reloaded")" == "$workspace_root" ]]
[[ "$(jq -r '.revision' <<<"$reloaded")" == "$source_revision" ]]
reused="$({
  jq -n --arg repository "$source_repo" '{repository:$repository,baseRef:"main"}'
} | curl -fsS -X PUT "$base_url/v1/jobs/$job_id/workspace" -H 'content-type: application/json' --data-binary @-)"
[[ "$(jq -r '.root' <<<"$reused")" == "$workspace_root" ]]

removed="$(curl -fsS -X DELETE "$base_url/v1/jobs/$job_id/workspace")"
[[ "$(jq -r '.state' <<<"$removed")" == "removed" ]]
[[ ! -e "$workspace_root" ]]

jq -n \
  --arg phase workspaceRecoveryAccepted \
  --arg jobId "$job_id" \
  --arg repository "$source_repo" \
  --arg root "$workspace_root" \
  --arg revision "$source_revision" \
  '{phase:$phase,jobId:$jobId,repository:$repository,root:$root,revision:$revision,restarted:true,reused:true,removed:true}'
