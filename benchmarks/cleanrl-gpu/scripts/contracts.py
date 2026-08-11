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
ISSUE_URL_RE = re.compile(r"^https://github\.com/vwxyzjn/cleanrl/issues/[0-9]+$")
PUBLIC_IMAGE_REPOSITORY = "ghcr.io/fpolica91/software-factory"
FACTORY_REVISION = "6e967bb67a92944ecdfa9b5884cc1924f2dbecba"
CLEANRL_REVISION = "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb"

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
    "import torch; assert torch.cuda.is_available(); torch.cuda.set_device(0); device=torch.device('cuda:0'); gpu_name=torch.cuda.get_device_name(0); model=torch.nn.Linear(4, 1).to(device); assert next(model.parameters()).device.type == 'cuda'; torch.nn.init.constant_(model.weight, 0.25); torch.nn.init.constant_(model.bias, 0.5); tensor=torch.ones((1, 4), device=device); assert tensor.device.type == 'cuda'; optimizer=torch.optim.SGD(model.parameters(), lr=0.01); before_weight=model.weight.detach().clone(); before_bias=model.bias.detach().clone(); optimizer.zero_grad(); loss=model(tensor).square().mean(); loss.backward(); assert model.weight.grad is not None and model.bias.grad is not None; optimizer.step(); weight_changed=not torch.equal(before_weight, model.weight.detach()); bias_changed=not torch.equal(before_bias, model.bias.detach()); assert weight_changed and bias_changed; assert torch.cuda.current_device() == 0; print(f'GPU name: {gpu_name}'); print(f'weight changed: {weight_changed}'); print(f'bias changed: {bias_changed}'); print('CUDA PREFLIGHT PASS')",
]
AGENT_PROFILE = {"provider": "zai", "model": "glm-5.2", "base": "coding"}
EXECUTION_PROFILES = {
    "gb10": {
        "base_image": "nvcr.io/nvidia/pytorch:25.08-py3",
        "execution_image": "docker.io/library/software-factory-gpu@sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557",
        "image_reference": "software-factory-gpu:benchmark-2b88673",
        "resolved_image_digest": "sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557",
        "public_image_digest": "sha256:a1ee9c9920eb45cbe8362b6aa1b34c34207322b52b280b0a5428315e6d6c09a1",
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
            "python3.12 -m venv --system-site-packages --without-pip "
            "/tmp/factory-cleanrl-venv",
            "UV_LINK_MODE=copy uv pip install --python "
            "/tmp/factory-cleanrl-venv/bin/python --require-hashes --no-deps "
            "-r .factory/benchmark/gb10.txt",
            "ln -sfn /tmp/factory-cleanrl-venv .venv",
            "mkdir -p /tmp/factory-runs && ln -sfn /tmp/factory-runs runs",
        ],
        "cuda_preflight_command": CUDA_PREFLIGHT,
    },
    "a100": {
        "base_image": "nvcr.io/nvidia/pytorch:25.08-py3",
        "execution_image": "docker.io/library/software-factory-gpu@sha256:4d518b4a8304930a98ecd214ee580e360291c055219c1eaf0b1bef887a4b4673",
        "image_reference": "software-factory-gpu:benchmark-2b88673",
        "resolved_image_digest": "sha256:4d518b4a8304930a98ecd214ee580e360291c055219c1eaf0b1bef887a4b4673",
        "public_image_digest": "sha256:ae8a2aac52a5fd32489e16812113092fe21c931b2167968a2fd93811c26c2c01",
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
            "UV_CACHE_DIR=/tmp/factory-uv-cache uv python install 3.10.16",
            "UV_CACHE_DIR=/tmp/factory-uv-cache UV_PYTHON=3.10.16 "
            "UV_PROJECT_ENVIRONMENT=/tmp/factory-cleanrl-venv uv sync --frozen "
            "--extra envpool --extra pytest --no-dev",
            "ln -sfn /tmp/factory-cleanrl-venv .venv",
            "mkdir -p /tmp/factory-runs && ln -sfn /tmp/factory-runs runs",
        ],
        "cuda_preflight_command": CUDA_PREFLIGHT,
    },
}
C51_COMMAND = [
    "env",
    "PYTHONPATH=.",
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
    "workspace_claim": "software-factory-workspaces",
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
        "130s",
        "kubectl",
        "--namespace",
        "software-factory-execution",
        "exec",
        "POD_NAME",
        "--",
        "sh",
        "-lc",
        "i=0; while [ \"$i\" -lt 120 ]; do pgrep -f '^([^ ]*/)?python([0-9]+([.][0-9]+)*)?[ ]+[c]leanrl/c51[.]py([ ]|$).*--total-timesteps[ ]+500000([ ]|$)' >/dev/null || exit 1; nvidia-smi --query-gpu=timestamp,index,name,utilization.gpu,memory.used,power.draw --format=csv,noheader,nounits || exit 1; i=$((i + 1)); [ \"$i\" -eq 120 ] || sleep 1; done",
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
    "stage_sequence_verified",
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
    "isolated_workspace_pods",
)
ISSUE_METRICS = (
    "cuda_optimizer_step_passed",
    "focused_test_count",
    "focused_test_seconds",
    "ppo_configured_steps",
    "ppo_completed_steps",
    "ppo_last_logged_step",
    "ppo_completion_verified",
    "ppo_final_sps",
    "ppo_last_training_return",
)
GPU_REQUIRED_METRICS = (
    "gpu_count",
    "sample_count",
    "gpu_seconds",
    "gpu_sample_span_seconds",
    "gpu_utilization_mean_pct",
    "gpu_utilization_max_pct",
)
GPU_OPTIONAL_METRIC_GROUPS = (
    ("gpu_memory_mean_mib", "gpu_memory_peak_mib"),
    ("gpu_power_mean_watts", "gpu_power_peak_watts"),
)
GPU_OPTIONAL_METRICS = tuple(
    field for group in GPU_OPTIONAL_METRIC_GROUPS for field in group
)
RL_CSV_METRICS = (
    "training_steps",
    "final_observed_step",
    "training_sps",
    "evaluation_episodes",
    "evaluation_return_mean",
    "evaluation_return_stddev",
)
RL_METRICS = (*RL_CSV_METRICS, "checkpoint_bytes", "checkpoint_sha256")
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
    "issue": ISSUE_METRICS,
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
    *ISSUE_METRICS,
    *KUBERNETES_METRICS,
    "gpu_count",
    "gpu_sample_count",
    "gpu_seconds",
    "gpu_sample_span_seconds",
    "gpu_utilization_mean_pct",
    "gpu_utilization_max_pct",
    *GPU_OPTIONAL_METRICS,
    *RL_METRICS,
)

CSV_GROUPS = {
    "factory": FACTORY_METRICS,
    "model": MODEL_METRICS,
    "issue": ISSUE_METRICS,
    "kubernetes": KUBERNETES_METRICS,
    "gpu": (
        "gpu_count",
        "gpu_sample_count",
        "gpu_seconds",
        "gpu_sample_span_seconds",
        "gpu_utilization_mean_pct",
        "gpu_utilization_max_pct",
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
        "stage_sequence_verified",
        *MODEL_METRICS,
        "cuda_optimizer_step_passed",
        "focused_test_count",
        "ppo_configured_steps",
        "ppo_completed_steps",
        "ppo_last_logged_step",
        "ppo_completion_verified",
        "ppo_final_sps",
        "pod_count",
        "pending_pods",
        "running_pods",
        "succeeded_pods",
        "failed_pods",
        "unknown_pods",
        "pod_restart_count",
        "runtime_class_pods",
        "isolated_workspace_pods",
        "gpu_count",
        "gpu_sample_count",
        "training_steps",
        "final_observed_step",
        "training_sps",
        "evaluation_episodes",
        "checkpoint_bytes",
    }
)
NONNEGATIVE_FIELDS = frozenset(
    set(METRIC_FIELDS)
    - {
        "run_id",
        "provider",
        "model",
        "provider_base",
        "status",
        "checkpoint_sha256",
        "evaluation_return_mean",
        "ppo_last_training_return",
    }
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
            "factory_revision",
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
    if manifest["factory_revision"] != FACTORY_REVISION:
        _fail("invalid run manifest")

    agent_profile = require_object(manifest["agent_profile"])
    require_exact_keys(agent_profile, AGENT_PROFILE)
    if agent_profile != AGENT_PROFILE:
        _fail("invalid run manifest")

    repository = require_object(manifest["repository"])
    require_exact_keys(repository, {"url", "revision"})
    if repository["url"] != "https://github.com/vwxyzjn/cleanrl":
        _fail("invalid run manifest")
    if repository["revision"] != CLEANRL_REVISION:
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
        public_digest = expected_profile["public_image_digest"]
        measured_profile = dict(expected_profile)
        public_profile = dict(expected_profile)
        public_profile["execution_image"] = f"{PUBLIC_IMAGE_REPOSITORY}@{public_digest}"
        public_profile["image_reference"] = public_profile["execution_image"]
        public_profile["resolved_image_digest"] = public_digest
        if profile not in (measured_profile, public_profile):
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
            {
                "id",
                "issue",
                "status",
                "execution_profile",
                "prompt_file",
                "submitted_task_sha256",
                "reproduction_task_sha256",
                "cuda_log_path",
                "focused_test_log_path",
                "focused_test_count",
                "ppo_log_path",
                "ppo_training_steps",
                "ppo_rollout_size",
                "validation_commands",
            },
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
                ".factory/benchmark/cuda-gb10.log",
                ".factory/benchmark/issue-488-pytest.log",
                2,
                ".factory/benchmark/issue-488-ppo.log",
                500_000,
                512,
            ),
            "cleanrl-562-factory": (
                562,
                "a100",
                "benchmarks/cleanrl-gpu/prompts/issue-562.md",
                ".factory/benchmark/cuda-a100.log",
                ".factory/benchmark/issue-562-pytest.log",
                5,
                ".factory/benchmark/issue-562-ppo.log",
                10_000_000,
                1_024,
            ),
        }
        if issue_id not in expected_issue_profile:
            _fail("invalid run manifest")
        task_hashes = (
            entry["submitted_task_sha256"],
            entry["reproduction_task_sha256"],
        )
        if any(
            not isinstance(task_hash, str)
            or re.fullmatch(r"[0-9a-f]{64}", task_hash) is None
            for task_hash in task_hashes
        ):
            _fail("invalid run manifest")
        expected_values = expected_issue_profile[issue_id]
        if (
            number,
            entry["execution_profile"],
            entry["prompt_file"],
            entry["cuda_log_path"],
            entry["focused_test_log_path"],
            entry["focused_test_count"],
            entry["ppo_log_path"],
            entry["ppo_training_steps"],
            entry["ppo_rollout_size"],
        ) != expected_values:
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
                "checkpoint_path",
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
        if entry["source_revision"] != CLEANRL_REVISION:
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
            "cleanrl-c51-gb10": (
                "gb10",
                ".factory/benchmark/c51-gb10.log",
                ".factory/benchmark/c51-gb10.cleanrl_model",
            ),
            "cleanrl-c51-a100": (
                "a100",
                ".factory/benchmark/c51-a100.log",
                ".factory/benchmark/c51-a100.cleanrl_model",
            ),
        }.get(run_id)
        if expected_run is None or (
            entry["execution_profile"],
            entry["log_path"],
            entry["checkpoint_path"],
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
        or not isinstance(metrics.get("final_observed_step"), int)
        or isinstance(metrics.get("final_observed_step"), bool)
        or metrics["final_observed_step"] >= expected["training_steps"]
        or metrics["final_observed_step"]
        < expected["training_steps"] - 500
        or not isinstance(metrics.get("checkpoint_bytes"), int)
        or isinstance(metrics.get("checkpoint_bytes"), bool)
        or metrics["checkpoint_bytes"] < 1
        or not isinstance(metrics.get("checkpoint_sha256"), str)
        or re.fullmatch(r"[0-9a-f]{64}", metrics["checkpoint_sha256"]) is None
    ):
        _fail("invalid aggregate observation")


def validate_issue_metrics_for_manifest(
    manifest: Mapping[str, Any], run_id: str, metrics: Mapping[str, Any]
) -> None:
    """Reject issue-gate aggregates that do not prove the configured rollout."""

    expected = next(
        (entry for entry in manifest["issue_jobs"] if entry["id"] == run_id),
        None,
    )
    if expected is None:
        _fail("invalid aggregate observation")
    target_steps = expected["ppo_training_steps"]
    rollout_size = expected["ppo_rollout_size"]
    completed_steps = target_steps // rollout_size * rollout_size
    last_logged_step = metrics.get("ppo_last_logged_step")
    if (
        metrics.get("cuda_optimizer_step_passed") != 1
        or metrics.get("focused_test_count") != expected["focused_test_count"]
        or metrics.get("ppo_configured_steps") != target_steps
        or metrics.get("ppo_completed_steps") != completed_steps
        or metrics.get("ppo_completion_verified") != 1
        or not isinstance(last_logged_step, int)
        or isinstance(last_logged_step, bool)
        or last_logged_step < 1
        or last_logged_step > completed_steps
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
        for group in GPU_OPTIONAL_METRIC_GROUPS:
            presence = [field in metrics for field in group]
            if any(presence) and not all(presence):
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
        if metrics["status"] != "succeeded":
            _fail("invalid aggregate observation")
        require_number(metrics["wall_seconds"], minimum=0)
        for field in FACTORY_METRICS[2:]:
            require_integer(metrics[field])
        if (
            metrics["operation_count"] != 4
            or metrics["completed_operations"] != 4
            or metrics["stage_checkpoint_count"] != 4
            or metrics["stage_sequence_verified"] != 1
        ):
            _fail("invalid aggregate observation")
        if metrics["retry_count"] > metrics["attempt_count"]:
            _fail("invalid aggregate observation")
    elif source == "model":
        for field in MODEL_METRICS:
            require_integer(metrics[field])
        if (
            metrics["response_count"] < 1
            or metrics["total_tokens"]
            != metrics["input_tokens"] + metrics["output_tokens"]
            or metrics["cached_input_tokens"] > metrics["input_tokens"]
            or metrics["reasoning_output_tokens"] > metrics["output_tokens"]
        ):
            _fail("invalid aggregate observation")
    elif source == "issue":
        require_integer(metrics["cuda_optimizer_step_passed"], minimum=1)
        require_integer(metrics["focused_test_count"], minimum=1)
        require_number(metrics["focused_test_seconds"], minimum=0)
        require_integer(metrics["ppo_configured_steps"], minimum=1)
        require_integer(metrics["ppo_completed_steps"], minimum=1)
        require_integer(metrics["ppo_last_logged_step"], minimum=1)
        require_integer(metrics["ppo_completion_verified"], minimum=1)
        require_integer(metrics["ppo_final_sps"], minimum=1)
        require_number(metrics["ppo_last_training_return"])
    elif source == "kubernetes":
        for field in KUBERNETES_METRICS:
            require_integer(metrics[field])
        phases = sum(metrics[field] for field in KUBERNETES_METRICS[1:6])
        if phases != metrics["pod_count"]:
            _fail("invalid aggregate observation")
        if metrics["runtime_class_pods"] > metrics["pod_count"]:
            _fail("invalid aggregate observation")
        if metrics["isolated_workspace_pods"] != metrics["pod_count"]:
            _fail("invalid aggregate observation")
    elif source == "gpu":
        require_integer(metrics["gpu_count"], minimum=1)
        require_integer(metrics["sample_count"], minimum=1)
        expected_samples = (
            COLLECTION["gpu_sample_duration_seconds"]
            // COLLECTION["gpu_sample_interval_seconds"]
        ) * metrics["gpu_count"]
        if metrics["sample_count"] != expected_samples:
            _fail("invalid aggregate observation")
        require_number(metrics["gpu_seconds"], minimum=0)
        if metrics["gpu_seconds"] != (
            COLLECTION["gpu_sample_duration_seconds"] * metrics["gpu_count"]
        ):
            _fail("invalid aggregate observation")
        require_number(
            metrics["gpu_sample_span_seconds"], minimum=115, maximum=150
        )
        require_number(metrics["gpu_utilization_mean_pct"], minimum=0, maximum=100)
        require_number(metrics["gpu_utilization_max_pct"], minimum=0, maximum=100)
        if metrics["gpu_utilization_mean_pct"] > metrics["gpu_utilization_max_pct"]:
            _fail("invalid aggregate observation")
        for field in GPU_OPTIONAL_METRICS:
            if field in metrics:
                require_number(metrics[field], minimum=0)
        if metrics.get("gpu_memory_mean_mib", 0) > metrics.get(
            "gpu_memory_peak_mib", math.inf
        ):
            _fail("invalid aggregate observation")
        if metrics.get("gpu_power_mean_watts", 0) > metrics.get("gpu_power_peak_watts", math.inf):
            _fail("invalid aggregate observation")
    else:
        require_integer(metrics["training_steps"], minimum=1)
        require_integer(metrics["final_observed_step"], minimum=1)
        require_integer(metrics["training_sps"], minimum=1)
        require_integer(metrics["evaluation_episodes"], minimum=1)
        require_number(metrics["evaluation_return_mean"])
        require_number(metrics["evaluation_return_stddev"], minimum=0)
        require_integer(metrics["checkpoint_bytes"], minimum=1)
        checkpoint_sha256 = metrics["checkpoint_sha256"]
        if not isinstance(checkpoint_sha256, str) or re.fullmatch(
            r"[0-9a-f]{64}", checkpoint_sha256
        ) is None:
            _fail("invalid aggregate observation")

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
                    required_fields = fields[:6]
                    for start in (6, 8):
                        optional_presence = present[start : start + 2]
                        if any(optional_presence) and not all(optional_presence):
                            _fail("invalid metrics table")
                if any(raw_row[field] == "" for field in required_fields):
                    _fail("invalid metrics table")
                sources.append(source)
                for field in fields:
                    if raw_row[field] == "":
                        continue
                    if field == "status":
                        if raw_row[field] != "succeeded":
                            _fail("invalid metrics table")
                        row[field] = raw_row[field]
                    elif field == "checkpoint_sha256":
                        if re.fullmatch(r"[0-9a-f]{64}", raw_row[field]) is None:
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
        "cleanrl-488-factory": ("factory", "model", "issue"),
        "cleanrl-562-factory": ("factory", "model", "issue"),
        "cleanrl-c51-gb10": ("kubernetes", "gpu", "rl"),
        "cleanrl-c51-a100": ("kubernetes", "gpu", "rl"),
    }
    if sources != expected_sources.get(row["run_id"]):
        _fail("invalid metrics table")
    if "factory" in sources:
        if row["status"] != "succeeded":
            _fail("invalid metrics table")
        if (
            row["operation_count"] != 4
            or row["completed_operations"] != 4
            or row["stage_checkpoint_count"] != 4
            or row["stage_sequence_verified"] != 1
        ):
            _fail("invalid metrics table")
        if row["retry_count"] > row["attempt_count"]:
            _fail("invalid metrics table")
    if "model" in sources:
        if (
            row["response_count"] < 1
            or row["total_tokens"] != row["input_tokens"] + row["output_tokens"]
            or row["cached_input_tokens"] > row["input_tokens"]
            or row["reasoning_output_tokens"] > row["output_tokens"]
        ):
            _fail("invalid metrics table")
    if "issue" in sources:
        validate_issue_metrics_for_manifest(manifest, row["run_id"], row)
    if "kubernetes" in sources:
        phase_total = sum(row[field] for field in KUBERNETES_METRICS[1:6])
        if phase_total != row["pod_count"]:
            _fail("invalid metrics table")
        if (
            row["runtime_class_pods"] > row["pod_count"]
            or row["isolated_workspace_pods"] != row["pod_count"]
        ):
            _fail("invalid metrics table")
    if "gpu" in sources:
        expected_samples = (
            COLLECTION["gpu_sample_duration_seconds"]
            // COLLECTION["gpu_sample_interval_seconds"]
        ) * row["gpu_count"]
        if (
            row["gpu_count"] < 1
            or row["gpu_sample_count"] != expected_samples
            or row["gpu_seconds"]
            != COLLECTION["gpu_sample_duration_seconds"] * row["gpu_count"]
            or row["gpu_sample_span_seconds"] < 115
            or row["gpu_sample_span_seconds"] > 150
        ):
            _fail("invalid metrics table")
        if row["gpu_utilization_mean_pct"] > row["gpu_utilization_max_pct"]:
            _fail("invalid metrics table")
        if row.get("gpu_memory_mean_mib", 0) > row.get("gpu_memory_peak_mib", math.inf):
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
