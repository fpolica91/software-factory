#!/usr/bin/env python3
"""Strict, dependency-free contracts shared by the benchmark tools."""

from __future__ import annotations

import csv
import json
import math
import re
from collections.abc import Iterable, Mapping
from typing import Any, TextIO


class ContractError(ValueError):
    """An input failed a benchmark contract.

    Messages are deliberately constant so untrusted payload values are never
    reflected into logs or terminal output.
    """


RUN_ID_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,63}$")
REVISION_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
ISSUE_URL_RE = re.compile(r"^https://github\.com/vwxyzjn/cleanrl/issues/[0-9]+$")

MANIFEST_STATUSES = frozenset({"planned", "running", "completed", "failed"})
FACTORY_STATUSES = frozenset(
    {
        "planned",
        "queued",
        "ready",
        "running",
        "cancelling",
        "succeeded",
        "completed",
        "failed",
        "cancelled",
    }
)

CUDA_PREFLIGHT = [
    ".venv/bin/python",
    "-c",
    "import torch; assert torch.cuda.is_available(); torch.cuda.set_device(0); device=torch.device('cuda:0'); model=torch.nn.Linear(4, 1).to(device); assert next(model.parameters()).device.type == 'cuda'; tensor=torch.ones((1, 4), device=device); optimizer=torch.optim.SGD(model.parameters(), lr=0.01); before=[parameter.detach().clone() for parameter in model.parameters()]; optimizer.zero_grad(); loss=model(tensor).square().mean(); loss.backward(); assert all(parameter.grad is not None for parameter in model.parameters()); optimizer.step(); after=list(model.parameters()); assert any(not torch.equal(old, new) for old, new in zip(before, after)); assert torch.cuda.current_device() == 0",
]
AGENT_PROFILE = {"provider": "zai", "model": "glm-5.2", "base": "coding"}
EXECUTION_PROFILES = {
    "gb10": {
        "base_image": "nvcr.io/nvidia/pytorch:25.08-py3",
        "execution_image": "docker.io/library/software-factory-gpu@sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557",
        "node_local_image": "software-factory-gpu:benchmark-2b88673",
        "node_local_content_digest": "sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557",
        "public_image_digest": None,
        "platform": "linux/arm64",
        "gpu_class": "NVIDIA GB10",
        "gpu_name": "NVIDIA GB10",
        "namespace": "software-factory-execution",
        "node_name": "spark-91b3",
        "runtime_class": "nvidia",
        "gpu_resource": "nvidia.com/gpu",
        "gpu_count": 1,
        "python": "3.12",
        "virtual_environment": ".venv",
        "system_site_packages": True,
        "dependency_input": "benchmarks/cleanrl-gpu/requirements/gb10.in",
        "dependency_input_sha256": "eaf3fa41a93aad1b2ee80aed5fae1b36248dad8bf5ff8b151de6f12dd814b105",
        "dependency_lock": "benchmarks/cleanrl-gpu/requirements/gb10.txt",
        "dependency_lock_sha256": "11523ca83e7da13ef6aa7c5f45945d0e93f601f55d80348211504ccf1ac1bd47",
        "setup_commands": [
            "python3.12 -m venv --system-site-packages .venv",
            "uv pip install --python .venv/bin/python --require-hashes --no-deps -r .factory/benchmark/gb10.txt",
        ],
        "cuda_preflight_command": CUDA_PREFLIGHT,
    },
    "a100": {
        "base_image": "nvcr.io/nvidia/pytorch:25.08-py3",
        "execution_image": "docker.io/library/software-factory-gpu@sha256:4d518b4a8304930a98ecd214ee580e360291c055219c1eaf0b1bef887a4b4673",
        "node_local_image": "software-factory-gpu:benchmark-2b88673",
        "node_local_content_digest": "sha256:4d518b4a8304930a98ecd214ee580e360291c055219c1eaf0b1bef887a4b4673",
        "public_image_digest": None,
        "platform": "linux/amd64",
        "gpu_class": "NVIDIA A100",
        "gpu_name": "NVIDIA A100-SXM4-40GB",
        "namespace": "software-factory-execution",
        "node_name": "kent-ai-stuff",
        "runtime_class": "nvidia",
        "gpu_resource": "nvidia.com/gpu",
        "gpu_count": 1,
        "python": "3.10.16",
        "virtual_environment": ".venv",
        "system_site_packages": False,
        "dependency_input": "pyproject.toml",
        "dependency_input_sha256": "a628568b59baf3bc2c7615ba8910a4c1454d7284dcab098c5e90d04d665d7d13",
        "dependency_lock": "uv.lock",
        "dependency_lock_sha256": "34ecd77065f7f99fabac27a8e562ba4894142e67774cb98d120ab527bb44df5b",
        "setup_commands": [
            "uv python install 3.10.16",
            "UV_PYTHON=3.10.16 uv sync --frozen --extra envpool --extra pytest --no-dev",
        ],
        "cuda_preflight_command": CUDA_PREFLIGHT,
    },
}
C51_COMMAND = [
    ".venv/bin/python",
    "cleanrl/c51.py",
    "--env-id",
    "CartPole-v1",
    "--seed",
    "1",
    "--total-timesteps",
    "500000",
    "--save-model",
]
COLLECTION = {
    "namespace": "software-factory-execution",
    "pod_selector_label": "software-factory.io/job-id",
    "running_pod_command": [
        "kubectl",
        "--namespace",
        "software-factory-execution",
        "get",
        "pods",
        "--selector",
        "software-factory.io/job-id=JOB_ID",
        "--field-selector",
        "status.phase=Running",
        "--output",
        'jsonpath={range .items[*]}{.metadata.name}{"\\n"}{end}',
    ],
    "gpu_sample_duration_seconds": 120,
    "gpu_sample_interval_seconds": 1,
    "gpu_sample_command": [
        "timeout",
        "120s",
        "kubectl",
        "--namespace",
        "software-factory-execution",
        "exec",
        "POD_NAME",
        "--",
        "sh",
        "-lc",
        "while pgrep -f '[c]leanrl/c51.py.*--total-timesteps 500000' >/dev/null; do nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,power.draw --format=csv,noheader,nounits; sleep 1; done",
    ],
}

FACTORY_METRICS = (
    "status",
    "wall_seconds",
    "operation_count",
    "completed_operations",
    "stage_checkpoint_count",
    "attempt_count",
    "retry_count",
)
PROFILE_METRICS = ("provider", "model", "provider_base")
KUBERNETES_METRICS = (
    "pod_count",
    "pending_pods",
    "running_pods",
    "succeeded_pods",
    "failed_pods",
    "unknown_pods",
    "pod_restart_count",
    "runtime_class_pods",
)
GPU_REQUIRED_METRICS = (
    "gpu_count",
    "sample_count",
    "gpu_seconds",
    "gpu_utilization_mean_pct",
    "gpu_utilization_max_pct",
    "gpu_memory_mean_mib",
    "gpu_memory_peak_mib",
)
GPU_OPTIONAL_METRICS = ("gpu_power_mean_watts", "gpu_power_peak_watts")
RL_METRICS = (
    "training_steps",
    "evaluation_episodes",
    "evaluation_return_mean",
    "evaluation_return_stddev",
)
MODEL_METRICS = (
    "response_count",
    "total_tokens",
    "input_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "tool_started_count",
    "context_compacted_count",
)

SOURCE_METRICS = {
    "profile": PROFILE_METRICS,
    "factory": FACTORY_METRICS,
    "model": MODEL_METRICS,
    "kubernetes": KUBERNETES_METRICS,
    "gpu": GPU_REQUIRED_METRICS,
    "rl": RL_METRICS,
}

METRIC_FIELDS = (
    "run_id",
    "provider",
    "model",
    "provider_base",
    *FACTORY_METRICS,
    *MODEL_METRICS,
    *KUBERNETES_METRICS,
    "gpu_count",
    "gpu_sample_count",
    "gpu_seconds",
    "gpu_utilization_mean_pct",
    "gpu_utilization_max_pct",
    "gpu_memory_mean_mib",
    "gpu_memory_peak_mib",
    *GPU_OPTIONAL_METRICS,
    *RL_METRICS,
)

CSV_GROUPS = {
    "factory": FACTORY_METRICS,
    "model": MODEL_METRICS,
    "kubernetes": KUBERNETES_METRICS,
    "gpu": (
        "gpu_count",
        "gpu_sample_count",
        "gpu_seconds",
        "gpu_utilization_mean_pct",
        "gpu_utilization_max_pct",
        "gpu_memory_mean_mib",
        "gpu_memory_peak_mib",
        *GPU_OPTIONAL_METRICS,
    ),
    "rl": RL_METRICS,
}

INTEGER_FIELDS = frozenset(
    {
        "operation_count",
        "completed_operations",
        "stage_checkpoint_count",
        "attempt_count",
        "retry_count",
        *MODEL_METRICS,
        "pod_count",
        "pending_pods",
        "running_pods",
        "succeeded_pods",
        "failed_pods",
        "unknown_pods",
        "pod_restart_count",
        "runtime_class_pods",
        "gpu_count",
        "gpu_sample_count",
        "training_steps",
        "evaluation_episodes",
    }
)
NONNEGATIVE_FIELDS = frozenset(
    set(METRIC_FIELDS)
    - {"run_id", "provider", "model", "provider_base", "status", "evaluation_return_mean"}
)
PERCENT_FIELDS = frozenset({"gpu_utilization_mean_pct", "gpu_utilization_max_pct"})


def _fail(message: str = "invalid benchmark input") -> None:
    raise ContractError(message)


def require_object(value: Any) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        _fail()
    return value


def require_exact_keys(value: Mapping[str, Any], required: Iterable[str], optional: Iterable[str] = ()) -> None:
    required_set = set(required)
    allowed = required_set | set(optional)
    if set(value) - allowed or not required_set.issubset(value):
        _fail()


def require_run_id(value: Any) -> str:
    if not isinstance(value, str) or RUN_ID_RE.fullmatch(value) is None:
        _fail()
    return value


def require_string(value: Any, *, nonempty: bool = True) -> str:
    if not isinstance(value, str) or (nonempty and not value):
        _fail()
    return value


def require_integer(value: Any, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        _fail()
    return value


def require_number(
    value: Any,
    *,
    minimum: float | None = None,
    maximum: float | None = None,
) -> float | int:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        _fail()
    number = float(value)
    if not math.isfinite(number):
        _fail()
    if minimum is not None and number < minimum:
        _fail()
    if maximum is not None and number > maximum:
        _fail()
    return value


def _strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _fail()
        result[key] = value
    return result


def load_json(stream: TextIO) -> Any:
    try:
        return json.load(
            stream,
            object_pairs_hook=_strict_object,
            parse_constant=lambda _value: _fail(),
        )
    except ContractError:
        raise
    except (json.JSONDecodeError, UnicodeError, RecursionError):
        _fail()


def validate_manifest(value: Any) -> dict[str, Any]:
    manifest = require_object(value)
    require_exact_keys(
        manifest,
        {
            "$schema",
            "benchmark",
            "agent_profile",
            "repository",
            "collection",
            "execution_profiles",
            "issue_jobs",
            "rl_runs",
        },
    )
    if manifest["$schema"] != "./schemas/run-manifest.schema.json":
        _fail("invalid run manifest")
    if manifest["benchmark"] != "cleanrl-gpu":
        _fail("invalid run manifest")

    agent_profile = require_object(manifest["agent_profile"])
    require_exact_keys(agent_profile, AGENT_PROFILE)
    if agent_profile != AGENT_PROFILE:
        _fail("invalid run manifest")

    repository = require_object(manifest["repository"])
    require_exact_keys(repository, {"url", "revision"})
    if repository["url"] != "https://github.com/vwxyzjn/cleanrl":
        _fail("invalid run manifest")
    revision = repository["revision"]
    if not isinstance(revision, str) or REVISION_RE.fullmatch(revision) is None:
        _fail("invalid run manifest")

    collection = require_object(manifest["collection"])
    require_exact_keys(collection, COLLECTION)
    if collection != COLLECTION:
        _fail("invalid run manifest")

    execution_profiles = require_object(manifest["execution_profiles"])
    require_exact_keys(execution_profiles, {"gb10", "a100"})
    for profile_name, expected_profile in EXECUTION_PROFILES.items():
        profile = require_object(execution_profiles[profile_name])
        require_exact_keys(profile, expected_profile)
        public_digest = profile["public_image_digest"]
        if public_digest is not None and (
            not isinstance(public_digest, str) or SHA256_RE.fullmatch(public_digest) is None
        ):
            _fail("invalid run manifest")
        invariant_profile = dict(profile)
        invariant_profile["public_image_digest"] = None
        if invariant_profile != expected_profile:
            _fail("invalid run manifest")

    issue_jobs = manifest["issue_jobs"]
    rl_runs = manifest["rl_runs"]
    if not isinstance(issue_jobs, list) or len(issue_jobs) != 2:
        _fail("invalid run manifest")
    if not isinstance(rl_runs, list) or len(rl_runs) != 2:
        _fail("invalid run manifest")

    issue_ids: list[str] = []
    issue_numbers: set[int] = set()
    for entry_value in issue_jobs:
        entry = require_object(entry_value)
        require_exact_keys(
            entry,
            {"id", "issue", "status", "execution_profile", "prompt_file", "validation_commands"},
        )
        issue_id = require_run_id(entry["id"])
        if issue_id in issue_ids:
            _fail("invalid run manifest")
        issue_ids.append(issue_id)
        if entry["status"] not in MANIFEST_STATUSES:
            _fail("invalid run manifest")
        issue = require_object(entry["issue"])
        require_exact_keys(issue, {"number", "title", "url"})
        number = require_integer(issue["number"], minimum=1)
        if number in issue_numbers:
            _fail("invalid run manifest")
        issue_numbers.add(number)
        require_string(issue["title"])
        issue_url = issue["url"]
        if not isinstance(issue_url, str) or ISSUE_URL_RE.fullmatch(issue_url) is None:
            _fail("invalid run manifest")
        if not issue_url.endswith(f"/{number}"):
            _fail("invalid run manifest")
        expected_issue_profile = {
            "cleanrl-488-factory": (
                488,
                "gb10",
                "benchmarks/cleanrl-gpu/prompts/issue-488.md",
            ),
            "cleanrl-562-factory": (
                562,
                "a100",
                "benchmarks/cleanrl-gpu/prompts/issue-562.md",
            ),
        }
        if issue_id not in expected_issue_profile:
            _fail("invalid run manifest")
        expected_number, expected_profile, expected_prompt = expected_issue_profile[issue_id]
        if (
            number != expected_number
            or entry["execution_profile"] != expected_profile
            or entry["prompt_file"] != expected_prompt
        ):
            _fail("invalid run manifest")
        commands = entry["validation_commands"]
        if not isinstance(commands, list) or len(commands) < 2:
            _fail("invalid run manifest")
        for command in commands:
            require_string(command)

    rl_ids: list[str] = []
    for entry_value in rl_runs:
        entry = require_object(entry_value)
        require_exact_keys(
            entry,
            {
                "id",
                "role",
                "status",
                "source_revision",
                "execution_profile",
                "framework",
                "algorithm",
                "environment",
                "seed",
                "training_steps",
                "evaluation_episodes",
                "gpu_required",
                "cuda_device",
                "cuda_preflight_required",
                "log_path",
                "log_signals",
                "command",
            },
        )
        run_id = require_run_id(entry["id"])
        if run_id in rl_ids or run_id in issue_ids:
            _fail("invalid run manifest")
        rl_ids.append(run_id)
        role = entry["role"]
        if role != "product-validation":
            _fail("invalid run manifest")
        if entry["status"] not in MANIFEST_STATUSES:
            _fail("invalid run manifest")
        if entry["source_revision"] != revision:
            _fail("invalid run manifest")
        if entry["framework"] != "pytorch":
            _fail("invalid run manifest")
        if entry["algorithm"] != "c51" or entry["environment"] != "CartPole-v1":
            _fail("invalid run manifest")
        if entry["seed"] != 1 or entry["training_steps"] != 500000:
            _fail("invalid run manifest")
        if entry["evaluation_episodes"] != 10:
            _fail("invalid run manifest")
        if entry["gpu_required"] is not True:
            _fail("invalid run manifest")
        if entry["cuda_device"] != 0:
            _fail("invalid run manifest")
        if entry["cuda_preflight_required"] is not True:
            _fail("invalid run manifest")
        if entry["log_signals"] != ["global_step", "episodic_return", "SPS", "eval_episode"]:
            _fail("invalid run manifest")
        command = entry["command"]
        if command != C51_COMMAND:
            _fail("invalid run manifest")
        expected_run = {
            "cleanrl-c51-gb10": ("gb10", ".factory/benchmark/c51-gb10.log"),
            "cleanrl-c51-a100": ("a100", ".factory/benchmark/c51-a100.log"),
        }.get(run_id)
        if expected_run is None or (
            entry["execution_profile"],
            entry["log_path"],
        ) != expected_run:
            _fail("invalid run manifest")
    if set(rl_ids) != {"cleanrl-c51-gb10", "cleanrl-c51-a100"}:
        _fail("invalid run manifest")
    return dict(manifest)


def manifest_run_ids(manifest: Mapping[str, Any]) -> tuple[str, ...]:
    return tuple(entry["id"] for entry in (*manifest["issue_jobs"], *manifest["rl_runs"]))


def validate_rl_metrics_for_manifest(
    manifest: Mapping[str, Any], run_id: str, metrics: Mapping[str, Any]
) -> None:
    """Reject RL aggregates that do not prove their manifest run completed."""

    expected = next(
        (entry for entry in manifest["rl_runs"] if entry["id"] == run_id),
        None,
    )
    if expected is None or (
        metrics.get("training_steps") != expected["training_steps"]
        or metrics.get("evaluation_episodes") != expected["evaluation_episodes"]
    ):
        _fail("invalid aggregate observation")


def validate_observation(value: Any) -> dict[str, Any]:
    observation = require_object(value)
    require_exact_keys(observation, {"run_id", "source", "metrics"})
    run_id = require_run_id(observation["run_id"])
    source = observation["source"]
    if source not in SOURCE_METRICS:
        _fail("invalid aggregate observation")
    metrics = require_object(observation["metrics"])

    if source == "gpu":
        require_exact_keys(metrics, GPU_REQUIRED_METRICS, GPU_OPTIONAL_METRICS)
        power_presence = [field in metrics for field in GPU_OPTIONAL_METRICS]
        if any(power_presence) and not all(power_presence):
            _fail("invalid aggregate observation")
    else:
        require_exact_keys(metrics, SOURCE_METRICS[source])

    if source == "profile":
        if dict(metrics) != {
            "provider": AGENT_PROFILE["provider"],
            "model": AGENT_PROFILE["model"],
            "provider_base": AGENT_PROFILE["base"],
        }:
            _fail("invalid aggregate observation")
    elif source == "factory":
        if metrics["status"] not in FACTORY_STATUSES:
            _fail("invalid aggregate observation")
        require_number(metrics["wall_seconds"], minimum=0)
        for field in FACTORY_METRICS[2:]:
            require_integer(metrics[field])
        if metrics["completed_operations"] > metrics["operation_count"]:
            _fail("invalid aggregate observation")
        if metrics["retry_count"] > metrics["attempt_count"]:
            _fail("invalid aggregate observation")
    elif source == "model":
        for field in MODEL_METRICS:
            require_integer(metrics[field])
    elif source == "kubernetes":
        for field in KUBERNETES_METRICS:
            require_integer(metrics[field])
        phases = sum(metrics[field] for field in KUBERNETES_METRICS[1:6])
        if phases != metrics["pod_count"]:
            _fail("invalid aggregate observation")
        if metrics["runtime_class_pods"] > metrics["pod_count"]:
            _fail("invalid aggregate observation")
    elif source == "gpu":
        require_integer(metrics["gpu_count"], minimum=1)
        require_integer(metrics["sample_count"], minimum=1)
        if metrics["sample_count"] < 2 * metrics["gpu_count"]:
            _fail("invalid aggregate observation")
        require_number(metrics["gpu_seconds"], minimum=0)
        require_number(metrics["gpu_utilization_mean_pct"], minimum=0, maximum=100)
        require_number(metrics["gpu_utilization_max_pct"], minimum=0, maximum=100)
        require_number(metrics["gpu_memory_mean_mib"], minimum=0)
        require_number(metrics["gpu_memory_peak_mib"], minimum=0)
        if metrics["gpu_utilization_mean_pct"] > metrics["gpu_utilization_max_pct"]:
            _fail("invalid aggregate observation")
        if metrics["gpu_memory_mean_mib"] > metrics["gpu_memory_peak_mib"]:
            _fail("invalid aggregate observation")
        for field in GPU_OPTIONAL_METRICS:
            if field in metrics:
                require_number(metrics[field], minimum=0)
        if metrics.get("gpu_power_mean_watts", 0) > metrics.get("gpu_power_peak_watts", math.inf):
            _fail("invalid aggregate observation")
    else:
        require_integer(metrics["training_steps"], minimum=1)
        require_integer(metrics["evaluation_episodes"], minimum=1)
        require_number(metrics["evaluation_return_mean"])
        require_number(metrics["evaluation_return_stddev"], minimum=0)

    return {"run_id": run_id, "source": source, "metrics": dict(metrics)}


def _parse_csv_number(field: str, value: str) -> int | float:
    if not value or value != value.strip():
        _fail("invalid metrics table")
    if field in INTEGER_FIELDS:
        if re.fullmatch(r"0|[1-9][0-9]*", value) is None:
            _fail("invalid metrics table")
        number: int | float = int(value)
    else:
        try:
            number = float(value)
        except ValueError:
            _fail("invalid metrics table")
        if not math.isfinite(number):
            _fail("invalid metrics table")
    if field in NONNEGATIVE_FIELDS and number < 0:
        _fail("invalid metrics table")
    if field in PERCENT_FIELDS and number > 100:
        _fail("invalid metrics table")
    return number


def read_metrics_csv(stream: TextIO, manifest: Mapping[str, Any]) -> list[dict[str, Any]]:
    manifest = validate_manifest(dict(manifest))
    try:
        reader = csv.DictReader(stream, strict=True)
        if tuple(reader.fieldnames or ()) != METRIC_FIELDS:
            _fail("invalid metrics table")
        rows: list[dict[str, Any]] = []
        seen: set[str] = set()
        allowed = set(manifest_run_ids(manifest))
        agent_profile = manifest["agent_profile"]
        for raw_row in reader:
            if None in raw_row or any(value is None for value in raw_row.values()):
                _fail("invalid metrics table")
            run_id = require_run_id(raw_row["run_id"])
            if run_id not in allowed or run_id in seen:
                _fail("invalid metrics table")
            seen.add(run_id)
            if (
                raw_row["provider"] != agent_profile["provider"]
                or raw_row["model"] != agent_profile["model"]
                or raw_row["provider_base"] != agent_profile["base"]
            ):
                _fail("invalid metrics table")
            row: dict[str, Any] = {
                "run_id": run_id,
                "provider": raw_row["provider"],
                "model": raw_row["model"],
                "provider_base": raw_row["provider_base"],
                "sources": (),
            }
            sources: list[str] = []
            for source, fields in CSV_GROUPS.items():
                present = [raw_row[field] != "" for field in fields]
                if not any(present):
                    continue
                required_fields = fields
                if source == "gpu":
                    required_fields = fields[:7]
                    optional_presence = present[7:]
                    if any(optional_presence) and not all(optional_presence):
                        _fail("invalid metrics table")
                if any(raw_row[field] == "" for field in required_fields):
                    _fail("invalid metrics table")
                sources.append(source)
                for field in fields:
                    if raw_row[field] == "":
                        continue
                    if field == "status":
                        if raw_row[field] not in FACTORY_STATUSES:
                            _fail("invalid metrics table")
                        row[field] = raw_row[field]
                    else:
                        row[field] = _parse_csv_number(field, raw_row[field])
            if not sources:
                _fail("invalid metrics table")
            row["sources"] = tuple(sources)
            _validate_metric_row(row, manifest)
            rows.append(row)
        if rows and seen != allowed:
            _fail("invalid metrics table")
        return rows
    except ContractError:
        raise
    except (csv.Error, UnicodeError):
        _fail("invalid metrics table")


def _validate_metric_row(row: Mapping[str, Any], manifest: Mapping[str, Any]) -> None:
    sources = row["sources"]
    expected_sources = {
        "cleanrl-488-factory": ("factory", "model"),
        "cleanrl-562-factory": ("factory", "model"),
        "cleanrl-c51-gb10": ("kubernetes", "gpu", "rl"),
        "cleanrl-c51-a100": ("kubernetes", "gpu", "rl"),
    }
    if sources != expected_sources.get(row["run_id"]):
        _fail("invalid metrics table")
    if "factory" in sources:
        if row["completed_operations"] > row["operation_count"]:
            _fail("invalid metrics table")
        if row["retry_count"] > row["attempt_count"]:
            _fail("invalid metrics table")
    if "kubernetes" in sources:
        phase_total = sum(row[field] for field in KUBERNETES_METRICS[1:6])
        if phase_total != row["pod_count"]:
            _fail("invalid metrics table")
        if row["runtime_class_pods"] > row["pod_count"]:
            _fail("invalid metrics table")
    if "gpu" in sources:
        if row["gpu_count"] < 1 or row["gpu_sample_count"] < 2 * row["gpu_count"]:
            _fail("invalid metrics table")
        if row["gpu_utilization_mean_pct"] > row["gpu_utilization_max_pct"]:
            _fail("invalid metrics table")
        if row["gpu_memory_mean_mib"] > row["gpu_memory_peak_mib"]:
            _fail("invalid metrics table")
        if "gpu_power_mean_watts" in row and row["gpu_power_mean_watts"] > row["gpu_power_peak_watts"]:
            _fail("invalid metrics table")
    if "rl" in sources:
        validate_rl_metrics_for_manifest(manifest, row["run_id"], row)


def format_number(value: int | float) -> str:
    if isinstance(value, int):
        return str(value)
    if value == 0:
        return "0"
    return format(value, ".15g")
