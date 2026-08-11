#!/usr/bin/env python3
"""Collect privacy-safe aggregate observations for the CleanRL benchmark."""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import json
import math
import os
import re
import statistics
import sys
import urllib.parse
import urllib.request
from collections import defaultdict
from collections.abc import Iterable, Iterator, Mapping
from contextlib import nullcontext
from pathlib import Path
from typing import Any, TextIO

from contracts import (
    AGENT_PROFILE,
    ContractError,
    FACTORY_STATUSES,
    METRIC_FIELDS,
    MODEL_METRICS,
    format_number,
    load_json,
    manifest_run_ids,
    require_exact_keys,
    require_integer,
    require_number,
    require_object,
    require_run_id,
    validate_manifest,
    validate_observation,
    validate_rl_metrics_for_manifest,
)


SAFE_ERROR = "error: benchmark input could not be read or validated"
ROOT = Path(__file__).resolve().parents[1]
OPERATION_STATES = frozenset({"ready", "running", "retryWait", "succeeded", "failed", "cancelled"})
ATTEMPT_STATES = frozenset({"running", "succeeded", "failed", "abandoned"})
TERMINAL_JOB_STATES = frozenset({"succeeded", "failed", "cancelled"})
NUMBER_PATTERN = r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?"
GLOBAL_STEP_RE = re.compile(r"(?:^|,\s*)global_step=(\d+)(?=,|\s|$)")
EVAL_RETURN_RE = re.compile(
    rf"(?:^|,\s*)eval_episode=(\d+),\s*episodic_return=({NUMBER_PATTERN})(?=,|\s|$)"
)
TRAINING_RECEIPT_RE = re.compile(r"^factory_training_steps=(\d+)$")
C51_MAX_EPISODE_STEPS = 500
PROFILE_SHOW_LINES = [
    "Active provider:",
    "  provider: Z.AI (zai)",
    "  model: glm-5.2",
    "  endpoint: https://api.z.ai/api/coding/paas/v4",
    "  API key: configured",
]
MODEL_USAGE_FIELDS = {
    "total_tokens": "totalTokens",
    "input_tokens": "inputTokens",
    "cached_input_tokens": "cachedInputTokens",
    "cache_write_input_tokens": "cacheWriteInputTokens",
    "output_tokens": "outputTokens",
    "reasoning_output_tokens": "reasoningOutputTokens",
}


def _rounded(value: float) -> float:
    rounded = round(value, 6)
    return 0.0 if rounded == 0 else rounded


def _timestamp(value: Any) -> dt.datetime:
    if not isinstance(value, str):
        raise ContractError("invalid factory input")
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise ContractError("invalid factory input") from None
    if parsed.tzinfo is None:
        raise ContractError("invalid factory input")
    return parsed


def _manifest_run_profile(manifest: Mapping[str, Any], run_id: str, kind: str) -> Mapping[str, Any]:
    entries = manifest["issue_jobs"] if kind == "issue" else manifest["rl_runs"]
    entry = next((entry for entry in entries if entry["id"] == run_id), None)
    if entry is None:
        raise ContractError("invalid benchmark run profile")
    return manifest["execution_profiles"][entry["execution_profile"]]


def _validate_job_agent(job: Mapping[str, Any], manifest: Mapping[str, Any]) -> None:
    job_input = require_object(job.get("input"))
    execution_profile = require_object(job_input.get("executionProfile"))
    require_exact_keys(execution_profile, {"provider", "model"})
    expected = manifest["agent_profile"]
    if (
        execution_profile["provider"] != expected["provider"]
        or execution_profile["model"] != expected["model"]
    ):
        raise ContractError("invalid factory input")


def collect_profile(run_id: str, payload_text: str, manifest: dict[str, Any]) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    manifest = validate_manifest(manifest)
    if run_id not in manifest_run_ids(manifest) or payload_text.splitlines() != PROFILE_SHOW_LINES:
        raise ContractError("invalid provider profile input")
    return validate_observation(
        {
            "run_id": run_id,
            "source": "profile",
            "metrics": {
                "provider": AGENT_PROFILE["provider"],
                "model": AGENT_PROFILE["model"],
                "provider_base": AGENT_PROFILE["base"],
            },
        }
    )


def collect_factory(
    run_id: str, payload_value: Any, manifest: dict[str, Any]
) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    manifest = validate_manifest(manifest)
    _manifest_run_profile(manifest, run_id, "issue")
    payload = require_object(payload_value)
    if not {"job", "stageCheckpoints", "attempts", "fullResult"}.issubset(payload):
        raise ContractError("invalid factory input")
    durable_job = require_object(payload["job"])
    if not {"job", "operations"}.issubset(durable_job):
        raise ContractError("invalid factory input")
    job = require_object(durable_job["job"])
    if not {"state", "createdAt", "updatedAt"}.issubset(job):
        raise ContractError("invalid factory input")
    _validate_job_agent(job, manifest)
    status = job["state"]
    if status not in FACTORY_STATUSES:
        raise ContractError("invalid factory input")
    created_at = _timestamp(job["createdAt"])
    updated_at = _timestamp(job["updatedAt"])
    wall_seconds = (updated_at - created_at).total_seconds()
    if wall_seconds < 0:
        raise ContractError("invalid factory input")
    operations = durable_job["operations"]
    if not isinstance(operations, list) or not operations:
        raise ContractError("invalid factory input")

    completed = 0
    operation_ids: set[str] = set()
    for operation_value in operations:
        operation = require_object(operation_value)
        if not {"operationId", "state"}.issubset(operation):
            raise ContractError("invalid factory input")
        operation_id = operation["operationId"]
        if not isinstance(operation_id, str) or not operation_id or operation_id in operation_ids:
            raise ContractError("invalid factory input")
        operation_ids.add(operation_id)
        operation_status = operation["state"]
        if operation_status not in OPERATION_STATES:
            raise ContractError("invalid factory input")
        if operation_status == "succeeded":
            completed += 1

    checkpoints = payload["stageCheckpoints"]
    attempts = payload["attempts"]
    if not isinstance(checkpoints, list) or not isinstance(attempts, list):
        raise ContractError("invalid factory input")
    for checkpoint_value in checkpoints:
        checkpoint = require_object(checkpoint_value)
        if checkpoint.get("operationId") not in operation_ids:
            raise ContractError("invalid factory input")
    retry_count = 0
    seen_attempts: set[tuple[str, int]] = set()
    for attempt_value in attempts:
        attempt = require_object(attempt_value)
        if not {"operationId", "attemptNumber", "state", "failure"}.issubset(attempt):
            raise ContractError("invalid factory input")
        operation_id = attempt["operationId"]
        attempt_number = require_integer(attempt["attemptNumber"], minimum=1)
        key = (operation_id, attempt_number)
        if operation_id not in operation_ids or key in seen_attempts:
            raise ContractError("invalid factory input")
        if attempt["state"] not in ATTEMPT_STATES:
            raise ContractError("invalid factory input")
        seen_attempts.add(key)
        if attempt_number > 1:
            retry_count += 1

    return validate_observation(
        {
            "run_id": run_id,
            "source": "factory",
            "metrics": {
                "status": status,
                "wall_seconds": _rounded(wall_seconds),
                "operation_count": len(operations),
                "completed_operations": completed,
                "stage_checkpoint_count": len(checkpoints),
                "attempt_count": len(attempts),
                "retry_count": retry_count,
            },
        }
    )


def _gpu_quantity_is_one(value: Any) -> bool:
    return (isinstance(value, int) and not isinstance(value, bool) and value == 1) or value == "1"


def collect_kubernetes(
    run_id: str, payload_value: Any, manifest: dict[str, Any]
) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    manifest = validate_manifest(manifest)
    expected = _manifest_run_profile(manifest, run_id, "rl")
    payload = require_object(payload_value)
    items = payload.get("items")
    if not isinstance(items, list) or len(items) != 1:
        raise ContractError("invalid kubernetes input")

    phases = {"Pending": 0, "Running": 0, "Succeeded": 0, "Failed": 0, "Unknown": 0}
    restarts = 0
    runtime_class_pods = 0
    for item_value in items:
        item = require_object(item_value)
        spec = require_object(item.get("spec", {}))
        status = require_object(item.get("status", {}))
        phase = status.get("phase")
        if phase != "Running":
            raise ContractError("invalid kubernetes input")
        phases[phase] += 1
        if (
            spec.get("nodeName") != expected["node_name"]
            or spec.get("runtimeClassName") != expected["runtime_class"]
        ):
            raise ContractError("invalid kubernetes input")
        runtime_class_pods += 1
        containers = spec.get("containers")
        container_statuses = status.get("containerStatuses")
        if (
            not isinstance(containers, list)
            or len(containers) != 1
            or not isinstance(container_statuses, list)
            or len(container_statuses) != 1
        ):
            raise ContractError("invalid kubernetes input")
        container = require_object(containers[0])
        if container.get("image") != expected["execution_image"]:
            raise ContractError("invalid kubernetes input")
        resources = require_object(container.get("resources"))
        requests = require_object(resources.get("requests"))
        limits = require_object(resources.get("limits"))
        gpu_resource = expected["gpu_resource"]
        if not (
            _gpu_quantity_is_one(requests.get(gpu_resource))
            and _gpu_quantity_is_one(limits.get(gpu_resource))
        ):
            raise ContractError("invalid kubernetes input")
        container_status = require_object(container_statuses[0])
        image_id = container_status.get("imageID")
        if not isinstance(image_id, str):
            raise ContractError("invalid kubernetes input")
        digest_match = re.search(r"(sha256:[0-9a-f]{64})$", image_id)
        if digest_match is None or digest_match.group(1) != expected["node_local_content_digest"]:
            raise ContractError("invalid kubernetes input")
        restarts += require_integer(container_status.get("restartCount"))

    return validate_observation(
        {
            "run_id": run_id,
            "source": "kubernetes",
            "metrics": {
                "pod_count": len(items),
                "pending_pods": phases["Pending"],
                "running_pods": phases["Running"],
                "succeeded_pods": phases["Succeeded"],
                "failed_pods": phases["Failed"],
                "unknown_pods": phases["Unknown"],
                "pod_restart_count": restarts,
                "runtime_class_pods": runtime_class_pods,
            },
        }
    )


def collect_gpu(
    run_id: str,
    payload_text: str,
    interval_seconds: Any,
    manifest: dict[str, Any],
) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    manifest = validate_manifest(manifest)
    expected = _manifest_run_profile(manifest, run_id, "rl")
    interval = require_number(interval_seconds, minimum=0)
    if float(interval) == 0 or not isinstance(payload_text, str):
        raise ContractError("invalid gpu input")

    gpu_ids: set[int] = set()
    samples_per_gpu: dict[int, int] = defaultdict(int)
    utilization: list[float] = []
    memory: list[float] = []
    power: list[float] = []
    rows = list(csv.reader(payload_text.splitlines(), strict=True))
    if not rows:
        raise ContractError("invalid gpu input")
    for row in rows:
        if len(row) != 5:
            raise ContractError("invalid gpu input")
        fields = [field.strip() for field in row]
        if any(not field for field in fields):
            raise ContractError("invalid gpu input")
        try:
            gpu_index = int(fields[0])
            util_value = float(fields[2])
            memory_value = float(fields[3])
        except ValueError:
            raise ContractError("invalid gpu input") from None
        gpu_index = require_integer(gpu_index)
        if fields[1] != expected["gpu_name"]:
            raise ContractError("invalid gpu input")
        gpu_ids.add(gpu_index)
        samples_per_gpu[gpu_index] += 1
        utilization.append(float(require_number(util_value, minimum=0, maximum=100)))
        memory.append(float(require_number(memory_value, minimum=0)))
        if fields[4] != "N/A":
            try:
                power_value = float(fields[4])
            except ValueError:
                raise ContractError("invalid gpu input") from None
            power.append(float(require_number(power_value, minimum=0)))

    sample_counts = tuple(samples_per_gpu.values())
    if (
        len(gpu_ids) != expected["gpu_count"]
        or min(sample_counts) < 2
        or max(sample_counts) - min(sample_counts) > 1
    ):
        raise ContractError("invalid gpu input")

    metrics: dict[str, Any] = {
        "gpu_count": len(gpu_ids),
        "sample_count": len(rows),
        "gpu_seconds": _rounded(float(interval) * len(rows)),
        "gpu_utilization_mean_pct": _rounded(statistics.fmean(utilization)),
        "gpu_utilization_max_pct": _rounded(max(utilization)),
        "gpu_memory_mean_mib": _rounded(statistics.fmean(memory)),
        "gpu_memory_peak_mib": _rounded(max(memory)),
    }
    if len(power) == len(rows):
        metrics["gpu_power_mean_watts"] = _rounded(statistics.fmean(power))
        metrics["gpu_power_peak_watts"] = _rounded(max(power))
    return validate_observation({"run_id": run_id, "source": "gpu", "metrics": metrics})


def collect_rl(run_id: str, payload_text: str, manifest: dict[str, Any]) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    if not isinstance(payload_text, str):
        raise ContractError("invalid rl input")
    manifest = validate_manifest(manifest)
    expected = next(
        (entry for entry in manifest["rl_runs"] if entry["id"] == run_id),
        None,
    )
    if expected is None:
        raise ContractError("invalid rl input")
    steps: list[int] = []
    episode_returns: dict[int, float] = {}
    training_receipts: list[int] = []
    for line in payload_text.splitlines():
        for match in GLOBAL_STEP_RE.finditer(line):
            steps.append(require_integer(int(match.group(1)), minimum=1))
        for match in EVAL_RETURN_RE.finditer(line):
            episode = require_integer(int(match.group(1)))
            if episode in episode_returns:
                raise ContractError("invalid rl input")
            value = float(match.group(2))
            if not math.isfinite(value):
                raise ContractError("invalid rl input")
            episode_returns[episode] = value
        receipt = TRAINING_RECEIPT_RE.fullmatch(line)
        if receipt is not None:
            training_receipts.append(require_integer(int(receipt.group(1)), minimum=1))
    expected_steps = expected["training_steps"]
    expected_episodes = expected["evaluation_episodes"]
    if (
        not steps
        or any(current <= previous for previous, current in zip(steps, steps[1:]))
        or max(steps) < expected_steps - C51_MAX_EPISODE_STEPS
        or max(steps) >= expected_steps
        or training_receipts != [expected_steps]
        or set(episode_returns) != set(range(expected_episodes))
    ):
        raise ContractError("invalid rl input")
    returns = [episode_returns[episode] for episode in sorted(episode_returns)]
    metrics = {
        "training_steps": training_receipts[0],
        "evaluation_episodes": len(returns),
        "evaluation_return_mean": _rounded(statistics.fmean(returns)),
        "evaluation_return_stddev": _rounded(statistics.pstdev(returns)),
    }
    observation = validate_observation({"run_id": run_id, "source": "rl", "metrics": metrics})
    validate_rl_metrics_for_manifest(manifest, run_id, observation["metrics"])
    return observation


def _event_fingerprint(kind: str, usage: Mapping[str, int] | None) -> bytes:
    """Fingerprint only fields that can affect the aggregate."""

    try:
        canonical = json.dumps(
            {"kind": kind, "usage": usage},
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=True,
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError, UnicodeError, RecursionError):
        raise ContractError("invalid model event input") from None
    return hashlib.sha256(canonical).digest()


def collect_model(run_id: str, pages: Iterable[Any]) -> dict[str, Any]:
    """Reduce complete, ordered JobEventPage values to safe model aggregates."""

    run_id = require_run_id(run_id)
    metrics = {field: 0 for field in MODEL_METRICS}
    seen_events: dict[int, bytes] = {}
    expected_job_fingerprint: bytes | None = None
    cursor = 0
    page_count = 0
    end_page_seen = False

    for page_value in pages:
        if end_page_seen:
            raise ContractError("invalid model event input")
        page_count += 1
        page = require_object(page_value)
        require_exact_keys(page, {"events", "nextCursor"})
        events = page["events"]
        next_cursor = require_integer(page["nextCursor"])
        if not isinstance(events, list):
            raise ContractError("invalid model event input")
        if not events:
            if next_cursor != cursor:
                raise ContractError("invalid model event input")
            end_page_seen = True
            continue

        page_sequences: list[int] = []
        for event_value in events:
            event = require_object(event_value)
            require_exact_keys(
                event,
                {"sequence", "jobId", "operationId", "attemptId", "kind", "payload", "createdAt"},
            )
            sequence = require_integer(event["sequence"], minimum=1)
            if page_sequences and sequence <= page_sequences[-1]:
                raise ContractError("invalid model event input")
            page_sequences.append(sequence)

            job_id = event["jobId"]
            if not isinstance(job_id, str) or not job_id:
                raise ContractError("invalid model event input")
            job_fingerprint = hashlib.sha256(job_id.encode("utf-8")).digest()
            if expected_job_fingerprint is None:
                expected_job_fingerprint = job_fingerprint
            elif job_fingerprint != expected_job_fingerprint:
                raise ContractError("invalid model event input")
            for optional_id in (event["operationId"], event["attemptId"]):
                if optional_id is not None and (not isinstance(optional_id, str) or not optional_id):
                    raise ContractError("invalid model event input")
            kind = event["kind"]
            if not isinstance(kind, str) or not kind:
                raise ContractError("invalid model event input")
            _timestamp(event["createdAt"])

            usage: dict[str, int] | None = None
            if kind == "model.usage":
                payload = require_object(event["payload"])
                usage = {}
                for metric, payload_field in MODEL_USAGE_FIELDS.items():
                    if payload_field not in payload:
                        raise ContractError("invalid model event input")
                    usage[metric] = require_integer(payload[payload_field])

            fingerprint = _event_fingerprint(kind, usage)
            previous_fingerprint = seen_events.get(sequence)
            if previous_fingerprint is not None:
                if previous_fingerprint != fingerprint:
                    raise ContractError("invalid model event input")
                continue
            if sequence <= cursor:
                raise ContractError("invalid model event input")
            seen_events[sequence] = fingerprint

            if usage is not None:
                for metric, value in usage.items():
                    metrics[metric] += value
                metrics["response_count"] += 1
            elif kind == "tool.started":
                metrics["tool_started_count"] += 1
            elif kind == "context.compacted":
                metrics["context_compacted_count"] += 1

        if next_cursor != page_sequences[-1] or next_cursor <= cursor:
            raise ContractError("invalid model event input")
        cursor = next_cursor

    if page_count == 0 or not end_page_seen:
        raise ContractError("invalid model event input")
    return validate_observation({"run_id": run_id, "source": "model", "metrics": metrics})


def fetch_job_event_pages(
    factoryd_url: str,
    job_id: str,
    manifest: dict[str, Any],
    page_limit: Any = 1_000,
) -> Iterator[Any]:
    """Assert terminal job state, then stream pages through the final empty page."""

    if not isinstance(factoryd_url, str) or not factoryd_url:
        raise ContractError("invalid model event input")
    if not isinstance(job_id, str) or not job_id:
        raise ContractError("invalid model event input")
    manifest = validate_manifest(manifest)
    limit = require_integer(page_limit, minimum=1)
    if limit > 1_000:
        raise ContractError("invalid model event input")
    encoded_job_id = urllib.parse.quote(job_id, safe="")
    job_url = f"{factoryd_url.rstrip('/')}/jobs/{encoded_job_id}"
    request = urllib.request.Request(job_url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as stream:
        durable_job = require_object(load_json(stream))
    job = require_object(durable_job.get("job"))
    _validate_job_agent(job, manifest)
    if job.get("state") not in TERMINAL_JOB_STATES:
        raise ContractError("invalid model event input")

    cursor = 0
    while True:
        query = urllib.parse.urlencode({"after": cursor, "limit": limit})
        url = f"{factoryd_url.rstrip('/')}/jobs/{encoded_job_id}/events?{query}"
        request = urllib.request.Request(url, headers={"Accept": "application/json"})
        with urllib.request.urlopen(request, timeout=30) as stream:
            page = load_json(stream)
        yield page
        page_object = require_object(page)
        events = page_object.get("events")
        next_cursor = page_object.get("nextCursor")
        if not isinstance(events, list):
            raise ContractError("invalid model event input")
        if not events:
            return
        next_cursor = require_integer(next_cursor, minimum=1)
        if next_cursor <= cursor:
            raise ContractError("invalid model event input")
        cursor = next_cursor


def merge_observations(
    observations: list[Any], manifest: dict[str, Any]
) -> list[dict[str, str]]:
    manifest = validate_manifest(manifest)
    allowed_ids = manifest_run_ids(manifest)
    allowed_set = set(allowed_ids)
    grouped: dict[str, dict[str, dict[str, Any]]] = defaultdict(dict)
    for value in observations:
        observation = validate_observation(value)
        run_id = observation["run_id"]
        source = observation["source"]
        if run_id not in allowed_set or source in grouped[run_id]:
            raise ContractError("invalid aggregate observation")
        if source == "rl":
            validate_rl_metrics_for_manifest(manifest, run_id, observation["metrics"])
        grouped[run_id][source] = observation
    expected_sources = {
        "cleanrl-488-factory": {"profile", "factory", "model"},
        "cleanrl-562-factory": {"profile", "factory", "model"},
        "cleanrl-c51-gb10": {"profile", "kubernetes", "gpu", "rl"},
        "cleanrl-c51-a100": {"profile", "kubernetes", "gpu", "rl"},
    }
    if set(grouped) != allowed_set or any(
        set(grouped[run_id]) != expected_sources[run_id] for run_id in allowed_ids
    ):
        raise ContractError("invalid aggregate observation")

    rows: list[dict[str, str]] = []
    for run_id in allowed_ids:
        row = {field: "" for field in METRIC_FIELDS}
        row["run_id"] = run_id
        for source in ("profile", "factory", "model", "kubernetes", "gpu", "rl"):
            observation = grouped[run_id].get(source)
            if observation is None:
                continue
            for field, value in observation["metrics"].items():
                csv_field = "gpu_sample_count" if source == "gpu" and field == "sample_count" else field
                row[csv_field] = value if isinstance(value, str) else format_number(value)
        rows.append(row)
    return rows


def _open_input(path: str):
    if path == "-":
        return nullcontext(sys.stdin)
    return open(path, "r", encoding="utf-8", newline="")


def _open_output(path: str):
    if path == "-":
        return nullcontext(sys.stdout)
    return open(path, "w", encoding="utf-8", newline="")


def _read_one(path: str) -> Any:
    with _open_input(path) as stream:
        return load_json(stream)


def _read_text(path: str) -> str:
    with _open_input(path) as stream:
        return stream.read()


def _write_json(path: str, value: Any) -> None:
    with _open_output(path) as stream:
        json.dump(value, stream, sort_keys=True, separators=(",", ":"), allow_nan=False)
        stream.write("\n")


def _load_observations(paths: list[str]) -> list[Any]:
    values: list[Any] = []
    if paths.count("-") > 1:
        raise ContractError("invalid aggregate observation")
    for path in paths:
        value = _read_one(path)
        if isinstance(value, list):
            values.extend(value)
        else:
            values.append(value)
    return values


def _write_rows(path: str, rows: list[dict[str, str]]) -> None:
    with _open_output(path) as stream:
        writer = csv.DictWriter(stream, fieldnames=METRIC_FIELDS, lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Collect whitelisted benchmark aggregates.")
    subparsers = parser.add_subparsers(dest="subcommand", required=True)
    for name in ("profile", "factory", "kubernetes", "gpu", "rl"):
        subparser = subparsers.add_parser(name)
        subparser.add_argument("--run-id", required=True)
        subparser.add_argument("--input", default="-")
        subparser.add_argument("--output", default="-")
        if name == "gpu":
            subparser.add_argument("--interval-seconds", required=True)
        if name in {"profile", "factory", "kubernetes", "gpu", "rl"}:
            subparser.add_argument("--manifest", default=str(ROOT / "run-manifest.json"))
    model = subparsers.add_parser("model")
    model.add_argument("--run-id", required=True)
    model.add_argument("--job-id", required=True)
    model.add_argument(
        "--factoryd-url",
        default=os.environ.get("FACTORYD_URL", "http://127.0.0.1:8787"),
    )
    model.add_argument("--page-limit", type=int, default=1_000)
    model.add_argument("--manifest", default=str(ROOT / "run-manifest.json"))
    model.add_argument("--output", default="-")
    merge = subparsers.add_parser("merge")
    merge.add_argument("inputs", nargs="+")
    merge.add_argument("--manifest", default=str(ROOT / "run-manifest.json"))
    merge.add_argument("--output", default="-")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.subcommand == "merge":
            manifest = validate_manifest(_read_one(args.manifest))
            observations = _load_observations(args.inputs)
            _write_rows(args.output, merge_observations(observations, manifest))
            return 0
        if args.subcommand in {"profile", "factory", "kubernetes", "gpu", "rl"}:
            manifest = validate_manifest(_read_one(args.manifest))
        if args.subcommand == "profile":
            observation = collect_profile(args.run_id, _read_text(args.input), manifest)
        elif args.subcommand == "factory":
            observation = collect_factory(args.run_id, _read_one(args.input), manifest)
        elif args.subcommand == "model":
            manifest = validate_manifest(_read_one(args.manifest))
            _manifest_run_profile(manifest, args.run_id, "issue")
            observation = collect_model(
                args.run_id,
                fetch_job_event_pages(args.factoryd_url, args.job_id, manifest, args.page_limit),
            )
        elif args.subcommand == "kubernetes":
            observation = collect_kubernetes(args.run_id, _read_one(args.input), manifest)
        elif args.subcommand == "gpu":
            observation = collect_gpu(
                args.run_id,
                _read_text(args.input),
                float(args.interval_seconds),
                manifest,
            )
        else:
            observation = collect_rl(args.run_id, _read_text(args.input), manifest)
        _write_json(args.output, observation)
        return 0
    except (ContractError, OSError, UnicodeError, BrokenPipeError, ValueError, csv.Error):
        print(SAFE_ERROR, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
