#!/usr/bin/env python3
"""Collect privacy-safe aggregate observations for the CleanRL benchmark."""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import io
import json
import math
import os
import re
import statistics
import sys
import urllib.parse
import urllib.request
import zipfile
from collections import defaultdict
from collections.abc import Iterable, Iterator, Mapping
from contextlib import nullcontext
from pathlib import Path
from typing import Any

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
    validate_issue_metrics_for_manifest,
    validate_manifest,
    validate_observation,
    validate_rl_metrics_for_manifest,
)


SAFE_ERROR = "error: benchmark input could not be read or validated"
ROOT = Path(__file__).resolve().parents[1]
ATTEMPT_STATES = frozenset({"running", "succeeded", "failed", "abandoned"})
PUBLISHABLE_JOB_STATES = frozenset({"succeeded"})
NUMBER_PATTERN = r"[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?"
GLOBAL_STEP_RE = re.compile(r"(?:^|,\s*)global_step=(\d+)(?=,|\s|$)")
SPS_RE = re.compile(r"^SPS:\s*(\d+)\s*$")
TRAIN_RETURN_RE = re.compile(
    rf"(?:^|,\s*)global_step=(\d+),\s*episodic_return="
    rf"(?:({NUMBER_PATTERN})|\[({NUMBER_PATTERN})\])(?=,|\s|$)"
)
PYTEST_RESULT_RE = re.compile(
    r"(?m)^(\d+) passed(?:, \d+ warnings?)? in ([0-9]+(?:\.[0-9]+)?)s$"
)
EVAL_RETURN_RE = re.compile(
    rf"(?:^|,\s*)eval_episode=(\d+),\s*episodic_return="
    rf"(?:({NUMBER_PATTERN})|\[({NUMBER_PATTERN})\])(?=,|\s|$)"
)
TRAINING_RECEIPT_RE = re.compile(r"^factory_training_steps=(\d+)$")
PPO_TRAINING_RECEIPT_RE = re.compile(r"^factory_ppo_training_steps=(\d+)$")
UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
)
C51_MAX_EPISODE_STEPS = 500
C51_MAX_SPS = 1_000_000
MIN_PYTORCH_CHECKPOINT_BYTES = 4_096
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
ROLLOUT_USAGE_FIELDS = tuple(MODEL_USAGE_FIELDS)
MODEL_USAGE_METRICS = ("response_count", *ROLLOUT_USAGE_FIELDS)


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


def _gpu_timestamp(value: str) -> dt.datetime:
    for timestamp_format in ("%Y/%m/%d %H:%M:%S.%f", "%Y/%m/%d %H:%M:%S"):
        try:
            return dt.datetime.strptime(value, timestamp_format)
        except ValueError:
            continue
    raise ContractError("invalid gpu input")


def _validate_pytorch_checkpoint(payload: bytes) -> None:
    if len(payload) < MIN_PYTORCH_CHECKPOINT_BYTES:
        raise ContractError("invalid rl input")
    try:
        with zipfile.ZipFile(io.BytesIO(payload)) as archive:
            names = archive.namelist()
            if not names or any(
                not name or name.startswith("/") or ".." in Path(name).parts
                for name in names
            ):
                raise ContractError("invalid rl input")
            roots = {name.split("/", 1)[0] for name in names if "/" in name}
            if len(roots) != 1:
                raise ContractError("invalid rl input")
            root = next(iter(roots))
            required = {
                f"{root}/data.pkl",
                f"{root}/byteorder",
                f"{root}/version",
                f"{root}/.data/serialization_id",
            }
            if not required.issubset(names) or not any(
                name.startswith(f"{root}/data/") for name in names
            ):
                raise ContractError("invalid rl input")
            if any(archive.getinfo(name).file_size == 0 for name in required):
                raise ContractError("invalid rl input")
    except (zipfile.BadZipFile, KeyError, OSError):
        raise ContractError("invalid rl input") from None


def _manifest_run_entry(
    manifest: Mapping[str, Any], run_id: str, kind: str
) -> Mapping[str, Any]:
    entries = manifest["issue_jobs"] if kind == "issue" else manifest["rl_runs"]
    entry = next((entry for entry in entries if entry["id"] == run_id), None)
    if entry is None:
        raise ContractError("invalid benchmark run profile")
    return entry


def _manifest_run_profile(
    manifest: Mapping[str, Any], run_id: str, kind: str
) -> Mapping[str, Any]:
    entry = _manifest_run_entry(manifest, run_id, kind)
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
    run_id: str,
    expected_job_id: str,
    payload_value: Any,
    workspace_value: Any,
    manifest: dict[str, Any],
) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    if not isinstance(expected_job_id, str) or UUID_RE.fullmatch(expected_job_id) is None:
        raise ContractError("invalid factory input")
    manifest = validate_manifest(manifest)
    _manifest_run_profile(manifest, run_id, "issue")
    issue_entry = next(entry for entry in manifest["issue_jobs"] if entry["id"] == run_id)
    payload = require_object(payload_value)
    if not {"job", "stageCheckpoints", "attempts", "fullResult"}.issubset(payload):
        raise ContractError("invalid factory input")
    full_result = require_object(payload["fullResult"])
    if not isinstance(full_result.get("markdown"), str) or not full_result["markdown"].strip():
        raise ContractError("invalid factory input")
    durable_job = require_object(payload["job"])
    if not {"job", "operations"}.issubset(durable_job):
        raise ContractError("invalid factory input")
    job = require_object(durable_job["job"])
    if not {
        "jobId",
        "kind",
        "input",
        "state",
        "createdAt",
        "updatedAt",
    }.issubset(job):
        raise ContractError("invalid factory input")
    repository_url = f'{manifest["repository"]["url"]}.git'
    expected_repository_id = (
        "remote:" + hashlib.sha256(repository_url.encode("utf-8")).hexdigest()
    )
    job_input = require_object(job["input"])
    task = job_input.get("task")
    task_sha256 = (
        hashlib.sha256(task.encode("utf-8")).hexdigest()
        if isinstance(task, str)
        else None
    )
    if (
        job["jobId"] != expected_job_id
        or job["kind"] != "factory.task"
        or job_input.get("repositoryId") != expected_repository_id
        or not isinstance(task, str)
        or task_sha256 not in {
            issue_entry["submitted_task_sha256"],
            issue_entry["reproduction_task_sha256"],
        }
    ):
        raise ContractError("invalid factory input")
    _validate_job_agent(job, manifest)
    workspace = require_object(workspace_value)
    expected_root = f"/workspaces/jobs/{expected_job_id}"
    if (
        workspace.get("jobId") != expected_job_id
        or workspace.get("repositoryId") != expected_repository_id
        or workspace.get("repository") != repository_url
        or workspace.get("baseRef") != manifest["repository"]["revision"]
        or workspace.get("baseRevision") != manifest["repository"]["revision"]
        or workspace.get("branchName") != f"factory/{expected_job_id}"
        or workspace.get("root") != expected_root
        or workspace.get("state") != "active"
    ):
        raise ContractError("invalid factory input")
    status = job["state"]
    if status not in FACTORY_STATUSES or status not in PUBLISHABLE_JOB_STATES:
        raise ContractError("invalid factory input")
    created_at = _timestamp(job["createdAt"])
    updated_at = _timestamp(job["updatedAt"])
    wall_seconds = (updated_at - created_at).total_seconds()
    if wall_seconds < 0:
        raise ContractError("invalid factory input")
    operations = durable_job["operations"]
    expected_kinds = ("codex.plan", "codex.execute", "codex.review", "codex.remediate")
    if not isinstance(operations, list) or len(operations) != len(expected_kinds):
        raise ContractError("invalid factory input")

    completed = 0
    operation_ids: set[str] = set()
    ordered_operation_ids: list[str] = []
    for operation_value, expected_kind in zip(operations, expected_kinds, strict=True):
        operation = require_object(operation_value)
        if not {"operationId", "kind", "state"}.issubset(operation):
            raise ContractError("invalid factory input")
        operation_id = operation["operationId"]
        if not isinstance(operation_id, str) or not operation_id or operation_id in operation_ids:
            raise ContractError("invalid factory input")
        operation_ids.add(operation_id)
        ordered_operation_ids.append(operation_id)
        operation_status = operation["state"]
        if operation["kind"] != expected_kind or operation_status != "succeeded":
            raise ContractError("invalid factory input")
        completed += 1

    attempts = payload["attempts"]
    if not isinstance(attempts, list):
        raise ContractError("invalid factory input")
    attempts_by_operation: dict[str, list[tuple[int, str, str]]] = {
        operation_id: [] for operation_id in ordered_operation_ids
    }
    seen_attempts: set[tuple[str, int]] = set()
    for attempt_value in attempts:
        attempt = require_object(attempt_value)
        if not {
            "attemptId",
            "operationId",
            "attemptNumber",
            "state",
            "failure",
        }.issubset(attempt):
            raise ContractError("invalid factory input")
        attempt_id = attempt["attemptId"]
        operation_id = attempt["operationId"]
        attempt_number = require_integer(attempt["attemptNumber"], minimum=1)
        key = (operation_id, attempt_number)
        if (
            not isinstance(attempt_id, str)
            or not attempt_id
            or operation_id not in operation_ids
            or key in seen_attempts
        ):
            raise ContractError("invalid factory input")
        if attempt["state"] not in ATTEMPT_STATES:
            raise ContractError("invalid factory input")
        seen_attempts.add(key)
        attempts_by_operation[operation_id].append(
            (attempt_number, attempt["state"], attempt_id)
        )

    retry_count = 0
    final_attempt_ids: dict[str, str] = {}
    for operation_id in ordered_operation_ids:
        operation_attempts = sorted(attempts_by_operation[operation_id])
        if (
            not operation_attempts
            or [number for number, _state, _attempt_id in operation_attempts]
            != list(range(1, len(operation_attempts) + 1))
            or operation_attempts[-1][1] != "succeeded"
            or any(
                state not in {"failed", "abandoned"}
                for _number, state, _attempt_id in operation_attempts[:-1]
            )
        ):
            raise ContractError("invalid factory input")
        retry_count += len(operation_attempts) - 1
        final_attempt_ids[operation_id] = operation_attempts[-1][2]

    checkpoints = payload["stageCheckpoints"]
    if not isinstance(checkpoints, list) or len(checkpoints) != len(expected_kinds):
        raise ContractError("invalid factory input")
    checkpoint_operation_ids: set[str] = set()
    for ordinal, (checkpoint_value, expected_kind) in enumerate(
        zip(checkpoints, expected_kinds, strict=True)
    ):
        checkpoint_record = require_object(checkpoint_value)
        checkpoint_operation_id = checkpoint_record.get("operationId")
        if (
            checkpoint_operation_id != ordered_operation_ids[ordinal]
            or checkpoint_operation_id not in operation_ids
            or checkpoint_operation_id in checkpoint_operation_ids
            or checkpoint_record.get("ordinal") != ordinal
            or checkpoint_record.get("operationKind") != expected_kind
        ):
            raise ContractError("invalid factory input")
        checkpoint = require_object(checkpoint_record.get("checkpoint"))
        if not {
            "checkpointId",
            "attemptId",
            "sequence",
            "kind",
            "payload",
            "createdAt",
        }.issubset(checkpoint):
            raise ContractError("invalid factory input")
        checkpoint_id = checkpoint["checkpointId"]
        checkpoint_payload = require_object(checkpoint["payload"])
        if (
            not isinstance(checkpoint_id, str)
            or not checkpoint_id
            or checkpoint["attemptId"] != final_attempt_ids[checkpoint_operation_id]
            or require_integer(checkpoint["sequence"], minimum=1) < 1
            or checkpoint["kind"] != "factory.stage"
            or checkpoint_payload.get("operation") != expected_kind
            or checkpoint_payload.get("phase") != "completed"
            or checkpoint.get("workspaceRoot") != expected_root
            or checkpoint.get("workspaceRevision")
            != manifest["repository"]["revision"]
        ):
            raise ContractError("invalid factory input")
        _timestamp(checkpoint["createdAt"])
        checkpoint_operation_ids.add(checkpoint_operation_id)
    if checkpoint_operation_ids != operation_ids:
        raise ContractError("invalid factory input")

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
                "stage_sequence_verified": 1,
            },
        }
    )


def _gpu_quantity_is_one(value: Any) -> bool:
    return (isinstance(value, int) and not isinstance(value, bool) and value == 1) or value == "1"


def collect_kubernetes(
    run_id: str,
    expected_job_id: str,
    payload_value: Any,
    manifest: dict[str, Any],
) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    if not isinstance(expected_job_id, str) or UUID_RE.fullmatch(expected_job_id) is None:
        raise ContractError("invalid kubernetes input")
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
        metadata = require_object(item.get("metadata", {}))
        labels = require_object(metadata.get("labels", {}))
        job_id = labels.get(manifest["collection"]["pod_selector_label"])
        if job_id != expected_job_id:
            raise ContractError("invalid kubernetes input")
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
        volumes = spec.get("volumes")
        volume_mounts = container.get("volumeMounts")
        if not isinstance(volumes, list) or not isinstance(volume_mounts, list):
            raise ContractError("invalid kubernetes input")
        workspace_volumes = [
            require_object(volume)
            for volume in volumes
            if isinstance(volume, dict) and volume.get("name") == "workspace"
        ]
        if len(workspace_volumes) != 1:
            raise ContractError("invalid kubernetes input")
        claim = require_object(workspace_volumes[0].get("persistentVolumeClaim"))
        if claim.get("claimName") != manifest["collection"]["workspace_claim"]:
            raise ContractError("invalid kubernetes input")
        job_root = f"/workspaces/jobs/{job_id}"
        workspace_mounts = [
            require_object(mount)
            for mount in volume_mounts
            if isinstance(mount, dict) and mount.get("name") == "workspace"
        ]
        workspaces_tree_mounts = [
            require_object(mount)
            for mount in volume_mounts
            if isinstance(mount, dict)
            and isinstance(mount.get("mountPath"), str)
            and (
                mount["mountPath"] == "/workspaces"
                or mount["mountPath"].startswith("/workspaces/")
            )
        ]
        job_mounts = [
            mount
            for mount in workspace_mounts
            if mount.get("mountPath") == job_root
            and mount.get("subPath") == f"jobs/{job_id}"
        ]
        mirror_mounts = []
        for mount in workspace_mounts:
            sub_path = mount.get("subPath")
            mount_path = mount.get("mountPath")
            if (
                isinstance(sub_path, str)
                and re.fullmatch(r"mirrors/[0-9a-f]{64}\.git", sub_path)
                and mount_path == f"/workspaces/{sub_path}"
            ):
                mirror_mounts.append(mount)
        if (
            len(job_mounts) != 1
            or len(mirror_mounts) > 1
            or len(workspace_mounts) != len(job_mounts) + len(mirror_mounts)
            or len(workspaces_tree_mounts) != len(workspace_mounts)
            or container.get("workingDir") != job_root
        ):
            raise ContractError("invalid kubernetes input")
        container_status = require_object(container_statuses[0])
        image_id = container_status.get("imageID")
        if not isinstance(image_id, str):
            raise ContractError("invalid kubernetes input")
        digest_match = re.search(r"(sha256:[0-9a-f]{64})$", image_id)
        if digest_match is None or digest_match.group(1) != expected["resolved_image_digest"]:
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
                "isolated_workspace_pods": len(items),
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
    expected_interval = manifest["collection"]["gpu_sample_interval_seconds"]
    expected_samples_per_gpu = (
        manifest["collection"]["gpu_sample_duration_seconds"] // expected_interval
    )
    if float(interval) != expected_interval or not isinstance(payload_text, str):
        raise ContractError("invalid gpu input")

    gpu_ids: set[int] = set()
    samples_per_gpu: dict[int, int] = defaultdict(int)
    timestamps_per_gpu: dict[int, list[dt.datetime]] = defaultdict(list)
    utilization: list[float] = []
    memory: list[float] = []
    power: list[float] = []
    rows = list(csv.reader(payload_text.splitlines(), strict=True))
    if not rows:
        raise ContractError("invalid gpu input")
    for row in rows:
        if len(row) != 6:
            raise ContractError("invalid gpu input")
        fields = [field.strip() for field in row]
        if any(not field for field in fields):
            raise ContractError("invalid gpu input")
        try:
            timestamp = _gpu_timestamp(fields[0])
            gpu_index = int(fields[1])
            util_value = float(fields[3])
        except ValueError:
            raise ContractError("invalid gpu input") from None
        gpu_index = require_integer(gpu_index)
        if fields[2] != expected["gpu_name"]:
            raise ContractError("invalid gpu input")
        gpu_ids.add(gpu_index)
        samples_per_gpu[gpu_index] += 1
        timestamps_per_gpu[gpu_index].append(timestamp)
        utilization.append(float(require_number(util_value, minimum=0, maximum=100)))
        if fields[4] not in {"N/A", "[N/A]"}:
            try:
                memory_value = float(fields[4])
            except ValueError:
                raise ContractError("invalid gpu input") from None
            memory.append(float(require_number(memory_value, minimum=0)))
        if fields[5] not in {"N/A", "[N/A]"}:
            try:
                power_value = float(fields[5])
            except ValueError:
                raise ContractError("invalid gpu input") from None
            power.append(float(require_number(power_value, minimum=0)))

    sample_counts = tuple(samples_per_gpu.values())
    if (
        len(gpu_ids) != expected["gpu_count"]
        or any(count != expected_samples_per_gpu for count in sample_counts)
    ):
        raise ContractError("invalid gpu input")
    sample_spans: list[float] = []
    for timestamps in timestamps_per_gpu.values():
        deltas = [
            (current - previous).total_seconds()
            for previous, current in zip(timestamps, timestamps[1:])
        ]
        span = (timestamps[-1] - timestamps[0]).total_seconds()
        if (
            any(delta < 0.5 or delta > 5 for delta in deltas)
            or span < manifest["collection"]["gpu_sample_duration_seconds"] - 5
            or span > manifest["collection"]["gpu_sample_duration_seconds"] + 30
        ):
            raise ContractError("invalid gpu input")
        sample_spans.append(span)

    metrics: dict[str, Any] = {
        "gpu_count": len(gpu_ids),
        "sample_count": len(rows),
        "gpu_seconds": _rounded(float(interval) * len(rows)),
        "gpu_sample_span_seconds": _rounded(statistics.fmean(sample_spans)),
        "gpu_utilization_mean_pct": _rounded(statistics.fmean(utilization)),
        "gpu_utilization_max_pct": _rounded(max(utilization)),
    }
    if len(memory) == len(rows):
        metrics["gpu_memory_mean_mib"] = _rounded(statistics.fmean(memory))
        metrics["gpu_memory_peak_mib"] = _rounded(max(memory))
    if len(power) == len(rows):
        metrics["gpu_power_mean_watts"] = _rounded(statistics.fmean(power))
        metrics["gpu_power_peak_watts"] = _rounded(max(power))
    return validate_observation({"run_id": run_id, "source": "gpu", "metrics": metrics})


def collect_issue(
    run_id: str,
    cuda_text: str,
    pytest_text: str,
    ppo_text: str,
    manifest: dict[str, Any],
) -> dict[str, Any]:
    """Reduce the three issue-correctness gates to a safe receipt."""

    run_id = require_run_id(run_id)
    if not all(isinstance(value, str) and value for value in (cuda_text, pytest_text, ppo_text)):
        raise ContractError("invalid issue input")
    manifest = validate_manifest(manifest)
    expected = next(
        (entry for entry in manifest["issue_jobs"] if entry["id"] == run_id),
        None,
    )
    if expected is None:
        raise ContractError("invalid issue input")
    profile = manifest["execution_profiles"][expected["execution_profile"]]
    cuda_lines = [line.strip() for line in cuda_text.splitlines() if line.strip()]
    pytest_lines = [line.strip() for line in pytest_text.splitlines() if line.strip()]
    ppo_lines = [line.strip() for line in ppo_text.splitlines() if line.strip()]
    if not cuda_lines or not pytest_lines or not ppo_lines:
        raise ContractError("invalid issue input")
    expected_cuda_receipt = [
        f"GPU name: {profile['gpu_name']}",
        "weight changed: True",
        "bias changed: True",
        "CUDA PREFLIGHT PASS",
    ]
    legacy_cuda_receipt = False
    if profile["gpu_name"] == "NVIDIA GB10" and len(cuda_lines) >= 9:
        legacy = cuda_lines[-9:]
        legacy_cuda_receipt = (
            legacy[0] == "device: cuda:0 | NVIDIA GB10"
            and legacy[1].startswith("torch: ")
            and legacy[2].startswith("loss: ")
            and legacy[3:5] == ["weight changed: True", "bias changed: True"]
            and legacy[5].startswith("max |delta weight|: ")
            and legacy[6:8] == ["weight on cuda: True", "tensor on cuda: True"]
            and legacy[8] == "CUDA PREFLIGHT PASS"
        )
        if legacy_cuda_receipt:
            try:
                legacy_cuda_receipt = math.isfinite(float(legacy[2].split(": ", 1)[1])) and float(
                    legacy[5].split(": ", 1)[1]
                ) > 0
            except ValueError:
                legacy_cuda_receipt = False
    elif profile["gpu_name"] == "NVIDIA A100-SXM4-40GB" and len(cuda_lines) >= 5:
        legacy = cuda_lines[-5:]
        legacy_cuda_receipt = (
            legacy[0] == "device: NVIDIA A100-SXM4-40GB"
            and legacy[1].startswith("loss: ")
            and legacy[2].startswith("param weight changed, max abs diff: ")
            and legacy[3].startswith("param bias changed, max abs diff: ")
            and legacy[4] == "CUDA backward + optimizer.step() PASS: parameter changed"
        )
        if legacy_cuda_receipt:
            try:
                legacy_cuda_receipt = (
                    math.isfinite(float(legacy[1].split(": ", 1)[1]))
                    and float(legacy[2].rsplit(": ", 1)[1]) > 0
                    and float(legacy[3].rsplit(": ", 1)[1]) > 0
                )
            except ValueError:
                legacy_cuda_receipt = False
    if (
        cuda_lines[-len(expected_cuda_receipt) :] != expected_cuda_receipt
        and not legacy_cuda_receipt
    ):
        raise ContractError("invalid issue input")

    pytest_results = PYTEST_RESULT_RE.findall(pytest_text)
    if len(pytest_results) != 1 or PYTEST_RESULT_RE.fullmatch(pytest_lines[-1]) is None:
        raise ContractError("invalid issue input")
    focused_test_count = require_integer(int(pytest_results[0][0]), minimum=1)
    focused_test_seconds = require_number(float(pytest_results[0][1]), minimum=0)

    steps: list[int] = []
    returns: list[float] = []
    sps_values: list[int] = []
    ppo_training_receipts: list[int] = []
    for line in ppo_text.splitlines():
        steps.extend(require_integer(int(match.group(1)), minimum=1) for match in GLOBAL_STEP_RE.finditer(line))
        for match in TRAIN_RETURN_RE.finditer(line):
            value = float(match.group(2) or match.group(3))
            if not math.isfinite(value):
                raise ContractError("invalid issue input")
            returns.append(value)
        sps_match = SPS_RE.fullmatch(line.strip())
        if sps_match is not None:
            sps_values.append(require_integer(int(sps_match.group(1)), minimum=1))
        receipt_match = PPO_TRAINING_RECEIPT_RE.fullmatch(line.strip())
        if receipt_match is not None:
            ppo_training_receipts.append(
                require_integer(int(receipt_match.group(1)), minimum=1)
            )
    target_steps = expected["ppo_training_steps"]
    completed_steps = (
        target_steps // expected["ppo_rollout_size"]
        * expected["ppo_rollout_size"]
    )
    legacy_terminal_sps = (
        not ppo_training_receipts
        and steps
        and steps[-1] == completed_steps
        and SPS_RE.fullmatch(ppo_lines[-1]) is not None
    )
    terminal_training_receipt = (
        ppo_training_receipts == [target_steps]
        and ppo_lines[-1] == f"factory_ppo_training_steps={target_steps}"
    )
    if (
        not steps
        or not returns
        or not sps_values
        or any(current < previous for previous, current in zip(steps, steps[1:]))
        or not (legacy_terminal_sps or terminal_training_receipt)
        or re.search(r"(?im)^traceback \(most recent call last\):", ppo_text)
        is not None
    ):
        raise ContractError("invalid issue input")

    observation = validate_observation(
        {
            "run_id": run_id,
            "source": "issue",
            "metrics": {
                "cuda_optimizer_step_passed": 1,
                "focused_test_count": focused_test_count,
                "focused_test_seconds": _rounded(float(focused_test_seconds)),
                "ppo_configured_steps": target_steps,
                "ppo_completed_steps": completed_steps,
                "ppo_last_logged_step": steps[-1],
                "ppo_completion_verified": 1,
                "ppo_final_sps": sps_values[-1],
                "ppo_last_training_return": _rounded(returns[-1]),
            },
        }
    )
    validate_issue_metrics_for_manifest(manifest, run_id, observation["metrics"])
    return observation


def validate_c51_cuda_receipt(
    run_id: str,
    cuda_text: str,
    manifest: dict[str, Any],
) -> None:
    """Require a CUDA optimizer-step receipt for a product-validation run."""

    run_id = require_run_id(run_id)
    if not isinstance(cuda_text, str) or not cuda_text:
        raise ContractError("invalid rl input")
    manifest = validate_manifest(manifest)
    profile = _manifest_run_profile(manifest, run_id, "rl")
    lines = [line.strip() for line in cuda_text.splitlines() if line.strip()]
    if not lines or re.search(
        r"(?im)^traceback \(most recent call last\):", cuda_text
    ) is not None:
        raise ContractError("invalid rl input")

    deterministic_receipt = [
        f"GPU name: {profile['gpu_name']}",
        "weight changed: True",
        "bias changed: True",
        "CUDA PREFLIGHT PASS",
    ]
    if lines[-len(deterministic_receipt) :] == deterministic_receipt:
        return

    if profile["gpu_name"] == "NVIDIA GB10" and lines[-4:] == [
        "NVIDIA GB10",
        "weight changed",
        "bias changed",
        "CUDA PREFLIGHT PASS",
    ]:
        return

    if profile["gpu_name"] == "NVIDIA A100-SXM4-40GB" and len(lines) >= 5:
        legacy = lines[-5:]
        if (
            legacy[0] == "device: NVIDIA A100-SXM4-40GB"
            and legacy[1].startswith("loss: ")
            and legacy[2].startswith("param weight changed, max abs diff: ")
            and legacy[3].startswith("param bias changed, max abs diff: ")
            and legacy[4]
            == "CUDA backward + optimizer.step() PASS: parameter changed"
        ):
            try:
                loss = float(legacy[1].split(": ", 1)[1])
                weight_delta = float(legacy[2].rsplit(": ", 1)[1])
                bias_delta = float(legacy[3].rsplit(": ", 1)[1])
            except ValueError:
                pass
            else:
                if (
                    math.isfinite(loss)
                    and math.isfinite(weight_delta)
                    and math.isfinite(bias_delta)
                    and weight_delta > 0
                    and bias_delta > 0
                ):
                    return

    raise ContractError("invalid rl input")


def collect_rl(
    run_id: str,
    payload_text: str,
    cuda_text: str,
    checkpoint_payload: bytes,
    manifest: dict[str, Any],
) -> dict[str, Any]:
    run_id = require_run_id(run_id)
    if (
        not isinstance(payload_text, str)
        or not isinstance(checkpoint_payload, bytes)
        or not checkpoint_payload
    ):
        raise ContractError("invalid rl input")
    manifest = validate_manifest(manifest)
    expected = next(
        (entry for entry in manifest["rl_runs"] if entry["id"] == run_id),
        None,
    )
    if expected is None:
        raise ContractError("invalid rl input")
    validate_c51_cuda_receipt(run_id, cuda_text, manifest)
    _validate_pytorch_checkpoint(checkpoint_payload)
    steps: list[int] = []
    sps_values: list[int] = []
    episode_returns: dict[int, float] = {}
    training_receipts: list[int] = []
    step_line_indices: list[int] = []
    sps_line_indices: list[int] = []
    evaluation_line_indices: list[int] = []
    receipt_line_indices: list[int] = []
    for line_index, line in enumerate(payload_text.splitlines()):
        for match in GLOBAL_STEP_RE.finditer(line):
            steps.append(require_integer(int(match.group(1)), minimum=1))
            step_line_indices.append(line_index)
        for match in EVAL_RETURN_RE.finditer(line):
            episode = require_integer(int(match.group(1)))
            if episode in episode_returns:
                raise ContractError("invalid rl input")
            value = float(match.group(2) or match.group(3))
            if not math.isfinite(value):
                raise ContractError("invalid rl input")
            episode_returns[episode] = value
            evaluation_line_indices.append(line_index)
        receipt = TRAINING_RECEIPT_RE.fullmatch(line)
        if receipt is not None:
            training_receipts.append(require_integer(int(receipt.group(1)), minimum=1))
            receipt_line_indices.append(line_index)
        sps_match = SPS_RE.fullmatch(line.strip())
        if sps_match is not None:
            sps_values.append(
                require_integer(int(sps_match.group(1)), minimum=1)
            )
            sps_line_indices.append(line_index)
    expected_steps = expected["training_steps"]
    expected_episodes = expected["evaluation_episodes"]
    nonempty_lines = [line.strip() for line in payload_text.splitlines() if line.strip()]
    if (
        not steps
        or not sps_values
        or any(current <= previous for previous, current in zip(steps, steps[1:]))
        or max(steps) < expected_steps - C51_MAX_EPISODE_STEPS
        or max(steps) >= expected_steps
        or training_receipts != [expected_steps]
        or set(episode_returns) != set(range(expected_episodes))
        or max(sps_values) > C51_MAX_SPS
        or sps_line_indices[-1] <= step_line_indices[-1]
        or sps_line_indices[-1] >= evaluation_line_indices[0]
        or step_line_indices[-1] >= evaluation_line_indices[0]
        or receipt_line_indices != [max(receipt_line_indices)]
        or receipt_line_indices[0] <= evaluation_line_indices[-1]
        or nonempty_lines[-1] != f"factory_training_steps={expected_steps}"
        or re.search(r"(?im)^traceback \(most recent call last\):", payload_text)
        is not None
    ):
        raise ContractError("invalid rl input")
    returns = [episode_returns[episode] for episode in sorted(episode_returns)]
    metrics = {
        "training_steps": training_receipts[0],
        "final_observed_step": steps[-1],
        "training_sps": sps_values[-1],
        "evaluation_episodes": len(returns),
        "evaluation_return_mean": _rounded(statistics.fmean(returns)),
        "evaluation_return_stddev": _rounded(statistics.pstdev(returns)),
        "checkpoint_bytes": len(checkpoint_payload),
        "checkpoint_sha256": hashlib.sha256(checkpoint_payload).hexdigest(),
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


def _rollout_usage(value: Any) -> dict[str, int]:
    usage = require_object(value)
    require_exact_keys(usage, set(ROLLOUT_USAGE_FIELDS))
    parsed = {
        field: require_integer(usage[field], minimum=0) for field in ROLLOUT_USAGE_FIELDS
    }
    if parsed["total_tokens"] != parsed["input_tokens"] + parsed["output_tokens"]:
        raise ContractError("invalid model rollout input")
    return parsed


def recover_rollout_model_usage(
    rollouts: Iterable[Iterable[Any]], expected_thread_ids: set[str]
) -> dict[str, int]:
    """Recover exact per-response usage from Codex's durable token counters."""

    if not expected_thread_ids:
        raise ContractError("invalid model rollout input")
    recovered = {field: 0 for field in MODEL_USAGE_METRICS}
    seen_thread_ids: set[str] = set()

    for records in rollouts:
        thread_id: str | None = None
        snapshots: list[tuple[dict[str, int], dict[str, int]]] = []
        for record_value in records:
            record = require_object(record_value)
            record_type = record.get("type")
            if record_type == "session_meta":
                payload = require_object(record.get("payload"))
                candidate = payload.get("id")
                if not isinstance(candidate, str) or not candidate:
                    raise ContractError("invalid model rollout input")
                if thread_id is not None and candidate != thread_id:
                    raise ContractError("invalid model rollout input")
                thread_id = candidate
                continue
            if record_type != "event_msg":
                continue
            payload = require_object(record.get("payload"))
            if payload.get("type") != "token_count" or payload.get("info") is None:
                continue
            info = require_object(payload["info"])
            total = _rollout_usage(info.get("total_token_usage"))
            last = _rollout_usage(info.get("last_token_usage"))
            snapshots.append((total, last))

        if (
            thread_id is None
            or thread_id not in expected_thread_ids
            or thread_id in seen_thread_ids
            or not snapshots
        ):
            raise ContractError("invalid model rollout input")
        seen_thread_ids.add(thread_id)

        previous = {field: 0 for field in ROLLOUT_USAGE_FIELDS}
        previous_last: dict[str, int] | None = None
        response_count = 0
        for total, last in snapshots:
            if total == previous:
                if previous_last != last:
                    raise ContractError("invalid model rollout input")
                continue
            for field in ROLLOUT_USAGE_FIELDS:
                if (
                    total[field] < previous[field]
                    or total[field] - previous[field] != last[field]
                ):
                    raise ContractError("invalid model rollout input")
            previous = total
            previous_last = last
            response_count += 1

        recovered["response_count"] += response_count
        for field in ROLLOUT_USAGE_FIELDS:
            recovered[field] += previous[field]

    if seen_thread_ids != expected_thread_ids:
        raise ContractError("invalid model rollout input")
    return recovered


def collect_model(
    run_id: str,
    pages: Iterable[Any],
    rollouts: Iterable[Iterable[Any]] | None = None,
    expected_job_id: str | None = None,
) -> dict[str, Any]:
    """Reduce complete, ordered JobEventPage values to safe model aggregates."""

    run_id = require_run_id(run_id)
    metrics = {field: 0 for field in MODEL_METRICS}
    seen_events: dict[int, bytes] = {}
    expected_job_fingerprint: bytes | None = None
    cursor = 0
    page_count = 0
    end_page_seen = False
    thread_ids: set[str] = set()

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
            if (
                not isinstance(job_id, str)
                or not job_id
                or (expected_job_id is not None and job_id != expected_job_id)
            ):
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

            if kind == "turn.started":
                payload = require_object(event["payload"])
                thread_id = payload.get("threadId")
                if not isinstance(thread_id, str) or not thread_id:
                    raise ContractError("invalid model event input")
                thread_ids.add(thread_id)

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
    if rollouts is not None:
        recovered = recover_rollout_model_usage(rollouts, thread_ids)
        event_usage = {field: metrics[field] for field in MODEL_USAGE_METRICS}
        if metrics["response_count"] and event_usage != recovered:
            raise ContractError("invalid model rollout input")
        metrics.update(recovered)
    if metrics["response_count"] == 0:
        raise ContractError("invalid model event input")
    return validate_observation({"run_id": run_id, "source": "model", "metrics": metrics})


def fetch_job_event_pages(
    factoryd_url: str,
    job_id: str,
    run_id: str,
    manifest: dict[str, Any],
    page_limit: Any = 1_000,
) -> Iterator[Any]:
    """Assert terminal job state, then stream pages through the final empty page."""

    if not isinstance(factoryd_url, str) or not factoryd_url:
        raise ContractError("invalid model event input")
    if not isinstance(job_id, str) or not job_id:
        raise ContractError("invalid model event input")
    manifest = validate_manifest(manifest)
    issue_entry = _manifest_run_entry(manifest, run_id, "issue")
    limit = require_integer(page_limit, minimum=1)
    if limit > 1_000:
        raise ContractError("invalid model event input")
    encoded_job_id = urllib.parse.quote(job_id, safe="")
    job_url = f"{factoryd_url.rstrip('/')}/jobs/{encoded_job_id}"
    request = urllib.request.Request(job_url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as stream:
        durable_job = require_object(load_json(stream))
    job = require_object(durable_job.get("job"))
    repository_url = f'{manifest["repository"]["url"]}.git'
    expected_repository_id = (
        "remote:" + hashlib.sha256(repository_url.encode("utf-8")).hexdigest()
    )
    job_input = require_object(job.get("input"))
    task = job_input.get("task")
    task_sha256 = (
        hashlib.sha256(task.encode("utf-8")).hexdigest()
        if isinstance(task, str)
        else None
    )
    if (
        job.get("jobId") != job_id
        or job.get("kind") != "factory.task"
        or job_input.get("repositoryId") != expected_repository_id
        or task_sha256 not in {
            issue_entry["submitted_task_sha256"],
            issue_entry["reproduction_task_sha256"],
        }
    ):
        raise ContractError("invalid model event input")
    _validate_job_agent(job, manifest)
    if job.get("state") not in PUBLISHABLE_JOB_STATES:
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


def fetch_job_workspace(factoryd_url: str, job_id: str) -> Any:
    """Load the private workspace receipt without exposing it in output."""

    if (
        not isinstance(factoryd_url, str)
        or not factoryd_url
        or not isinstance(job_id, str)
        or UUID_RE.fullmatch(job_id) is None
    ):
        raise ContractError("invalid factory input")
    encoded_job_id = urllib.parse.quote(job_id, safe="")
    url = f"{factoryd_url.rstrip('/')}/jobs/{encoded_job_id}/workspace"
    request = urllib.request.Request(url, headers={"Accept": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as stream:
        return load_json(stream)


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
        if source == "issue":
            validate_issue_metrics_for_manifest(manifest, run_id, observation["metrics"])
        grouped[run_id][source] = observation
    expected_sources = {
        "cleanrl-488-factory": {"profile", "factory", "model", "issue"},
        "cleanrl-562-factory": {"profile", "factory", "model", "issue"},
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
        for source in ("profile", "factory", "model", "issue", "kubernetes", "gpu", "rl"):
            observation = grouped[run_id].get(source)
            if observation is None:
                continue
            for field, value in observation["metrics"].items():
                csv_field = "gpu_sample_count" if source == "gpu" and field == "sample_count" else field
                if csv_field not in row:
                    continue
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


def _read_bytes(path: str) -> bytes:
    if path == "-":
        raise ContractError("invalid rl input")
    with open(path, "rb") as stream:
        return stream.read()


def _iter_jsonl(path: str) -> Iterator[Any]:
    with _open_input(path) as stream:
        for line in stream:
            if not line.strip():
                raise ContractError("invalid model rollout input")
            yield json.loads(line)


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
        if name == "kubernetes":
            subparser.add_argument("--job-id", required=True)
        if name == "factory":
            subparser.add_argument("--job-id", required=True)
            subparser.add_argument(
                "--factoryd-url",
                default=os.environ.get("FACTORYD_URL", "http://127.0.0.1:8787"),
            )
        if name == "rl":
            subparser.add_argument("--cuda-input", required=True)
            subparser.add_argument("--checkpoint", required=True)
        if name in {"profile", "factory", "kubernetes", "gpu", "rl"}:
            subparser.add_argument("--manifest", default=str(ROOT / "run-manifest.json"))
    issue = subparsers.add_parser("issue")
    issue.add_argument("--run-id", required=True)
    issue.add_argument("--cuda-input", required=True)
    issue.add_argument("--pytest-input", required=True)
    issue.add_argument("--ppo-input", required=True)
    issue.add_argument("--manifest", default=str(ROOT / "run-manifest.json"))
    issue.add_argument("--output", default="-")
    model = subparsers.add_parser("model")
    model.add_argument("--run-id", required=True)
    model.add_argument("--job-id", required=True)
    model.add_argument(
        "--factoryd-url",
        default=os.environ.get("FACTORYD_URL", "http://127.0.0.1:8787"),
    )
    model.add_argument("--page-limit", type=int, default=1_000)
    model.add_argument("--rollout", action="append", default=[])
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
        if args.subcommand in {"profile", "factory", "issue", "kubernetes", "gpu", "rl"}:
            manifest = validate_manifest(_read_one(args.manifest))
        if args.subcommand == "profile":
            observation = collect_profile(args.run_id, _read_text(args.input), manifest)
        elif args.subcommand == "factory":
            observation = collect_factory(
                args.run_id,
                args.job_id,
                _read_one(args.input),
                fetch_job_workspace(args.factoryd_url, args.job_id),
                manifest,
            )
        elif args.subcommand == "issue":
            observation = collect_issue(
                args.run_id,
                _read_text(args.cuda_input),
                _read_text(args.pytest_input),
                _read_text(args.ppo_input),
                manifest,
            )
        elif args.subcommand == "model":
            manifest = validate_manifest(_read_one(args.manifest))
            _manifest_run_profile(manifest, args.run_id, "issue")
            observation = collect_model(
                args.run_id,
                fetch_job_event_pages(
                    args.factoryd_url,
                    args.job_id,
                    args.run_id,
                    manifest,
                    args.page_limit,
                ),
                [_iter_jsonl(path) for path in args.rollout] if args.rollout else None,
                expected_job_id=args.job_id,
            )
        elif args.subcommand == "kubernetes":
            observation = collect_kubernetes(
                args.run_id, args.job_id, _read_one(args.input), manifest
            )
        elif args.subcommand == "gpu":
            observation = collect_gpu(
                args.run_id,
                _read_text(args.input),
                float(args.interval_seconds),
                manifest,
            )
        else:
            observation = collect_rl(
                args.run_id,
                _read_text(args.input),
                _read_text(args.cuda_input),
                _read_bytes(args.checkpoint),
                manifest,
            )
        _write_json(args.output, observation)
        return 0
    except (ContractError, OSError, UnicodeError, BrokenPipeError, ValueError, csv.Error):
        print(SAFE_ERROR, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
