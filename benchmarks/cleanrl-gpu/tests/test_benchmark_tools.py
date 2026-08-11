from __future__ import annotations

import csv
import hashlib
import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = ROOT.parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

import collect  # noqa: E402
import render_charts  # noqa: E402
from contracts import (  # noqa: E402
    ContractError,
    METRIC_FIELDS,
    MODEL_METRICS,
    read_metrics_csv,
    validate_manifest,
    validate_observation,
)


SECRET = "SECRET-PROMPT-KEY-PATH-NAME-FAILURE-PAYLOAD"


def load_manifest() -> dict:
    with (ROOT / "run-manifest.json").open(encoding="utf-8") as stream:
        return validate_manifest(json.load(stream))


def job_event(sequence: int, kind: str, payload: object, *, job_id: str = f"job-{SECRET}") -> dict:
    return {
        "sequence": sequence,
        "jobId": job_id,
        "operationId": f"operation-{SECRET}",
        "attemptId": f"attempt-{SECRET}",
        "kind": kind,
        "payload": payload,
        "createdAt": "2026-08-11T12:00:00Z",
    }


def usage_payload(multiplier: int = 1) -> dict:
    return {
        "totalTokens": 100 * multiplier,
        "inputTokens": 70 * multiplier,
        "cachedInputTokens": 20 * multiplier,
        "cacheWriteInputTokens": 5 * multiplier,
        "outputTokens": 30 * multiplier,
        "reasoningOutputTokens": 10 * multiplier,
        "threadId": SECRET,
        "turnId": SECRET,
        "prompt": SECRET,
        "reasoning": SECRET,
        "toolArguments": SECRET,
        "toolOutput": SECRET,
        "path": SECRET,
    }


def configured_job_input(*, provider: str = "zai", model: str = "glm-5.2") -> dict:
    return {
        "task": SECRET,
        "executionProfile": {"provider": provider, "model": model},
    }


def profile_show() -> str:
    return "\n".join(collect.PROFILE_SHOW_LINES) + "\n"


def factory_payload(*, state: str = "succeeded") -> dict:
    return {
        "job": {
            "job": {
                "state": state,
                "createdAt": "2026-08-11T12:00:00Z",
                "updatedAt": "2026-08-11T12:01:30Z",
                "input": configured_job_input(),
            },
            "operations": [{"operationId": "op-a", "state": "succeeded"}],
        },
        "stageCheckpoints": [],
        "attempts": [],
        "fullResult": None,
    }


def pod_payload(profile_name: str, *, node_name: str | None = None) -> dict:
    profile = load_manifest()["execution_profiles"][profile_name]
    return {
        "items": [
            {
                "metadata": {"name": SECRET},
                "spec": {
                    "nodeName": node_name or profile["node_name"],
                    "runtimeClassName": profile["runtime_class"],
                    "containers": [
                        {
                            "image": profile["execution_image"],
                            "resources": {
                                "requests": {profile["gpu_resource"]: "1"},
                                "limits": {profile["gpu_resource"]: "1"},
                            },
                        }
                    ],
                },
                "status": {
                    "phase": "Running",
                    "containerStatuses": [
                        {
                            "restartCount": 2,
                            "imageID": f"docker-pullable://image@{profile['node_local_content_digest']}",
                        }
                    ],
                },
            }
        ]
    }


def gpu_stdout(profile_name: str, *, gpu_name: str | None = None) -> str:
    name = gpu_name or load_manifest()["execution_profiles"][profile_name]["gpu_name"]
    return f"0, {name}, 50, 100, 20\n0, {name}, 70, 300, N/A\n"


def rl_stdout() -> str:
    return "\n".join(
        [
            "global_step=499700, episodic_return=100.0",
            *(f"eval_episode={episode}, episodic_return=1.0" for episode in range(10)),
            "factory_training_steps=500000",
        ]
    )


def complete_observations(manifest: dict) -> list[dict]:
    observations: list[dict] = []
    for run_id in ("cleanrl-488-factory", "cleanrl-562-factory"):
        observations.extend(
            [
                collect.collect_profile(run_id, profile_show(), manifest),
                collect.collect_factory(run_id, factory_payload(), manifest),
                collect.collect_model(
                    run_id,
                    [
                        {"events": [job_event(1, "model.usage", usage_payload())], "nextCursor": 1},
                        {"events": [], "nextCursor": 1},
                    ],
                ),
            ]
        )
    for run_id, profile_name in (
        ("cleanrl-c51-gb10", "gb10"),
        ("cleanrl-c51-a100", "a100"),
    ):
        observations.extend(
            [
                collect.collect_profile(run_id, profile_show(), manifest),
                collect.collect_kubernetes(run_id, pod_payload(profile_name), manifest),
                collect.collect_gpu(run_id, gpu_stdout(profile_name), 1, manifest),
                collect.collect_rl(run_id, rl_stdout(), manifest),
            ]
        )
    return observations


class CollectorTests(unittest.TestCase):
    def test_model_pages_sum_deduplicate_and_drop_secret_content(self) -> None:
        duplicate_tool = job_event(2, "tool.started", {"arguments": SECRET, "output": SECRET})
        pages = [
            {
                "events": [job_event(1, "model.usage", usage_payload()), duplicate_tool],
                "nextCursor": 2,
            },
            {
                "events": [
                    duplicate_tool,
                    job_event(3, "model.usage", usage_payload(2)),
                    job_event(4, "unrelated.event", {"prompt": SECRET}),
                    job_event(5, "context.compacted", {"reasoning": SECRET}),
                    job_event(7, "tool.started", {"path": SECRET}),
                ],
                "nextCursor": 7,
            },
            {"events": [], "nextCursor": 7},
        ]
        observation = collect.collect_model("cleanrl-c51-gb10", pages)
        self.assertEqual(
            observation["metrics"],
            {
                "response_count": 2,
                "total_tokens": 300,
                "input_tokens": 210,
                "cached_input_tokens": 60,
                "cache_write_input_tokens": 15,
                "output_tokens": 90,
                "reasoning_output_tokens": 30,
                "tool_started_count": 2,
                "context_compacted_count": 1,
            },
        )
        serialized = json.dumps(observation)
        self.assertNotIn(SECRET, serialized)
        for forbidden in (
            "jobid", "operationid", "attemptid", "prompt", "arguments",
            "tooloutput", "path", "payload", "createdat",
        ):
            self.assertNotIn(forbidden, serialized.lower())

    def test_model_pages_do_not_stop_at_the_first_thousand_events(self) -> None:
        first_page = [job_event(sequence, "model.usage", usage_payload()) for sequence in range(1, 1001)]
        second_page = [job_event(sequence, "model.usage", usage_payload()) for sequence in range(1001, 1006)]
        observation = collect.collect_model(
            "cleanrl-c51-a100",
            [
                {"events": first_page, "nextCursor": 1000},
                {"events": second_page, "nextCursor": 1005},
                {"events": [], "nextCursor": 1005},
            ],
        )
        self.assertEqual(observation["metrics"]["response_count"], 1005)
        self.assertEqual(observation["metrics"]["total_tokens"], 100500)

    def test_model_pages_reject_ordering_and_conflicting_duplicates(self) -> None:
        with self.assertRaises(ContractError):
            collect.collect_model(
                "cleanrl-c51-gb10",
                [
                    {
                        "events": [
                            job_event(2, "tool.started", {}),
                            job_event(1, "tool.started", {}),
                        ],
                        "nextCursor": 1,
                    },
                    {"events": [], "nextCursor": 1},
                ],
            )
        original = job_event(1, "model.usage", usage_payload())
        conflicting = job_event(1, "model.usage", usage_payload(2))
        with self.assertRaises(ContractError):
            collect.collect_model(
                "cleanrl-c51-gb10",
                [
                    {"events": [original], "nextCursor": 1},
                    {
                        "events": [conflicting, job_event(2, "tool.started", {})],
                        "nextCursor": 2,
                    },
                    {"events": [], "nextCursor": 2},
                ],
            )

    def test_model_pages_reject_malformed_or_incomplete_streams(self) -> None:
        valid_event = job_event(1, "model.usage", usage_payload())
        invalid_streams = [
            [],
            [{"events": [valid_event], "nextCursor": 1}],
            [{"events": [valid_event], "nextCursor": 2}, {"events": [], "nextCursor": 2}],
            [{"events": [], "nextCursor": 1}],
            [
                {
                    "events": [job_event(1, "model.usage", {**usage_payload(), "totalTokens": -1})],
                    "nextCursor": 1,
                },
                {"events": [], "nextCursor": 1},
            ],
            [
                {"events": [valid_event], "nextCursor": 1},
                {
                    "events": [job_event(2, "tool.started", {}, job_id="different-job")],
                    "nextCursor": 2,
                },
                {"events": [], "nextCursor": 2},
            ],
        ]
        for pages in invalid_streams:
            with self.subTest(pages=pages), self.assertRaises(ContractError):
                collect.collect_model("cleanrl-c51-gb10", pages)

    def test_model_api_fetch_checks_terminal_job_then_advances_cursor(self) -> None:
        responses = [
            {
                "job": {
                    "jobId": "job/id",
                    "state": "succeeded",
                    "input": configured_job_input(),
                },
                "operations": [],
            },
            {"events": [job_event(4, "tool.started", {})], "nextCursor": 4},
            {"events": [job_event(9, "context.compacted", {})], "nextCursor": 9},
            {"events": [], "nextCursor": 9},
        ]
        requests: list[str] = []

        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                self.close()

        def open_response(request, timeout):
            self.assertEqual(timeout, 30)
            requests.append(request.full_url)
            return Response(json.dumps(responses[len(requests) - 1]).encode())

        with mock.patch.object(collect.urllib.request, "urlopen", side_effect=open_response):
            pages = list(
                collect.fetch_job_event_pages(
                    "http://factoryd:8787/", "job/id", load_manifest(), 1000
                )
            )
        self.assertEqual(pages, responses[1:])
        self.assertEqual(
            requests,
            [
                "http://factoryd:8787/jobs/job%2Fid",
                "http://factoryd:8787/jobs/job%2Fid/events?after=0&limit=1000",
                "http://factoryd:8787/jobs/job%2Fid/events?after=4&limit=1000",
                "http://factoryd:8787/jobs/job%2Fid/events?after=9&limit=1000",
            ],
        )

    def test_model_api_fetch_rejects_running_job_before_event_pagination(self) -> None:
        requests: list[str] = []

        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                self.close()

        def open_response(request, timeout):
            self.assertEqual(timeout, 30)
            requests.append(request.full_url)
            return Response(
                json.dumps(
                    {
                        "job": {
                            "jobId": "job/id",
                            "state": "running",
                            "input": configured_job_input(),
                        },
                        "operations": [],
                    }
                ).encode()
            )

        with mock.patch.object(collect.urllib.request, "urlopen", side_effect=open_response):
            with self.assertRaises(ContractError):
                list(
                    collect.fetch_job_event_pages(
                        "http://factoryd:8787/", "job/id", load_manifest(), 1000
                    )
                )
        self.assertEqual(requests, ["http://factoryd:8787/jobs/job%2Fid"])

    def test_factory_parses_real_envelope_and_drops_hostile_fields(self) -> None:
        payload = {
            "job": {
                "job": {
                    "state": "succeeded",
                    "createdAt": "2026-08-11T12:00:00Z",
                    "updatedAt": "2026-08-11T12:01:30Z",
                    "input": configured_job_input(),
                    "failure": SECRET,
                    "path": SECRET,
                    "name": SECRET,
                },
                "operations": [
                    {"operationId": "op-a", "state": "succeeded", "input": {"task": SECRET}},
                    {"operationId": "op-b", "state": "failed", "rawPayload": SECRET},
                ],
            },
            "stageCheckpoints": [{"operationId": "op-a", "checkpoint": {"payload": SECRET}}],
            "attempts": [
                {
                    "operationId": "op-a",
                    "attemptNumber": 1,
                    "state": "failed",
                    "failure": {"raw": SECRET},
                },
                {
                    "operationId": "op-a",
                    "attemptNumber": 2,
                    "state": "succeeded",
                    "failure": None,
                },
            ],
            "fullResult": {"reasoning": SECRET, "localPath": SECRET},
            "prompt": SECRET,
        }
        manifest = load_manifest()
        observation = collect.collect_factory("cleanrl-488-factory", payload, manifest)
        self.assertEqual(
            observation["metrics"],
            {
                "status": "succeeded",
                "wall_seconds": 90.0,
                "operation_count": 2,
                "completed_operations": 1,
                "stage_checkpoint_count": 1,
                "attempt_count": 2,
                "retry_count": 1,
            },
        )
        serialized = json.dumps(observation)
        self.assertNotIn(SECRET, serialized)
        for forbidden in ("prompt", "reasoning", "failure", "path", "name", "payload", "task"):
            self.assertNotIn(forbidden, serialized.lower())

        mismatched = json.loads(json.dumps(payload))
        mismatched["job"]["job"]["input"]["executionProfile"]["provider"] = "deepseek"
        with self.assertRaises(ContractError):
            collect.collect_factory("cleanrl-488-factory", mismatched, manifest)

    def test_profile_requires_exact_configured_zai_coding_receipt(self) -> None:
        manifest = load_manifest()
        observation = collect.collect_profile(
            "cleanrl-488-factory", profile_show(), manifest
        )
        self.assertEqual(
            observation["metrics"],
            {"provider": "zai", "model": "glm-5.2", "provider_base": "coding"},
        )
        for mismatch in (
            profile_show().replace("glm-5.2", "deepseek-v4-pro"),
            profile_show().replace("/coding/", "/paas/"),
            profile_show().replace("configured", "missing"),
        ):
            with self.subTest(mismatch=mismatch), self.assertRaises(ContractError):
                collect.collect_profile("cleanrl-488-factory", mismatch, manifest)

    def test_kubernetes_validates_profile_and_drops_metadata(self) -> None:
        manifest = load_manifest()
        payload = pod_payload("gb10")
        metrics = collect.collect_kubernetes("cleanrl-c51-gb10", payload, manifest)["metrics"]
        self.assertEqual(metrics["pod_count"], 1)
        self.assertEqual(metrics["running_pods"], 1)
        self.assertEqual(metrics["succeeded_pods"], 0)
        self.assertEqual(metrics["unknown_pods"], 0)
        self.assertEqual(metrics["pod_restart_count"], 2)
        self.assertEqual(metrics["runtime_class_pods"], 1)
        self.assertNotIn(SECRET, json.dumps(metrics))

        mutations = (
            ("spec", "nodeName", "wrong-node"),
            ("spec", "runtimeClassName", "runc"),
        )
        for section, field, value in mutations:
            mismatched = json.loads(json.dumps(payload))
            mismatched["items"][0][section][field] = value
            with self.subTest(field=field), self.assertRaises(ContractError):
                collect.collect_kubernetes("cleanrl-c51-gb10", mismatched, manifest)
        for mismatch in ("image", "gpu", "imageID"):
            payload_copy = json.loads(json.dumps(payload))
            container = payload_copy["items"][0]["spec"]["containers"][0]
            if mismatch == "image":
                container["image"] = "wrong:image"
            elif mismatch == "gpu":
                container["resources"]["limits"]["nvidia.com/gpu"] = "0"
            else:
                payload_copy["items"][0]["status"]["containerStatuses"][0]["imageID"] = (
                    "containerd://sha256:" + "0" * 64
                )
            with self.subTest(mismatch=mismatch), self.assertRaises(ContractError):
                collect.collect_kubernetes("cleanrl-c51-gb10", payload_copy, manifest)

    def test_gpu_parses_nvidia_smi_csv_and_omits_partial_power(self) -> None:
        raw = (
            "0, NVIDIA GB10, 50, 100, 20\n"
            "0, NVIDIA GB10, 80, 400, N/A\n"
        )
        manifest = load_manifest()
        metrics = collect.collect_gpu("cleanrl-c51-gb10", raw, 1, manifest)["metrics"]
        self.assertEqual(metrics["gpu_count"], 1)
        self.assertEqual(metrics["sample_count"], 2)
        self.assertEqual(metrics["gpu_seconds"], 2.0)
        self.assertEqual(metrics["gpu_utilization_mean_pct"], 65.0)
        self.assertEqual(metrics["gpu_memory_peak_mib"], 400.0)
        self.assertNotIn("gpu_power_mean_watts", metrics)

        with self.assertRaises(ContractError):
            collect.collect_gpu(
                "cleanrl-c51-gb10",
                raw.replace("NVIDIA GB10", "NVIDIA A100-SXM4-40GB"),
                1,
                manifest,
            )

    def test_gpu_rejects_a_single_snapshot_or_unbalanced_series(self) -> None:
        invalid_series = (
            "0, NVIDIA GB10, 50, 100, 20\n",
            "0, NVIDIA GB10, 50, 100, 20\n1, NVIDIA GB10, 70, 300, 40\n",
        )
        for raw in invalid_series:
            with self.subTest(raw=raw), self.assertRaises(ContractError):
                collect.collect_gpu("cleanrl-c51-gb10", raw, 1, load_manifest())

    def test_rl_parses_cleanrl_stdout_only(self) -> None:
        stdout = "\n".join(
            [
                f"prompt={SECRET}",
                "global_step=100, episodic_return=12.0",
                "global_step=499700, episodic_return=100.0",
                *(f"eval_episode={episode}, episodic_return=1.0" for episode in range(10)),
                f"reasoning={SECRET}",
                "factory_training_steps=500000",
            ]
        )
        observation = collect.collect_rl("cleanrl-c51-gb10", stdout, load_manifest())
        self.assertEqual(
            observation["metrics"],
            {
                "training_steps": 500000,
                "evaluation_episodes": 10,
                "evaluation_return_mean": 1.0,
                "evaluation_return_stddev": 0.0,
            },
        )
        self.assertNotIn(SECRET, json.dumps(observation))

    def test_strict_observation_and_duplicate_evaluation_rejection(self) -> None:
        with self.assertRaises(ContractError):
            validate_observation(
                {
                    "run_id": "cleanrl-c51-gb10",
                    "source": "rl",
                    "metrics": {
                        "training_steps": 1,
                        "evaluation_episodes": 1,
                        "evaluation_return_mean": 1,
                        "evaluation_return_stddev": 0,
                        "rawPayload": SECRET,
                    },
                }
            )
        with self.assertRaises(ContractError):
            collect.collect_rl(
                "cleanrl-c51-gb10",
                "global_step=1\neval_episode=0, episodic_return=1\neval_episode=0, episodic_return=2\n",
                load_manifest(),
            )

    def test_rl_rejects_truncated_steps_or_evaluation(self) -> None:
        manifest = load_manifest()
        evaluations = "\n".join(
            f"eval_episode={episode}, episodic_return=1" for episode in range(10)
        )
        wrong_indices = "\n".join(
            f"eval_episode={episode}, episodic_return=1" for episode in range(1, 11)
        )
        invalid_logs = (
            f"global_step=1\n{evaluations}\nfactory_training_steps=500000\n",
            "global_step=499900\neval_episode=0, episodic_return=1\n"
            "factory_training_steps=500000\n",
            f"global_step=499900\n{evaluations}\n",
            f"global_step=499900\n{wrong_indices}\nfactory_training_steps=500000\n",
        )
        for payload in invalid_logs:
            with self.subTest(payload=payload), self.assertRaises(ContractError):
                collect.collect_rl("cleanrl-c51-gb10", payload, manifest)

    def test_merge_requires_four_truthfully_scoped_groups(self) -> None:
        manifest = load_manifest()
        observations = complete_observations(manifest)
        rows = collect.merge_observations(observations, manifest)
        self.assertEqual(len(rows), 4)
        rows_by_id = {row["run_id"]: row for row in rows}
        issue_row = rows_by_id["cleanrl-488-factory"]
        c51_row = rows_by_id["cleanrl-c51-gb10"]
        self.assertEqual(issue_row["status"], "succeeded")
        self.assertEqual(issue_row["total_tokens"], "100")
        self.assertEqual(issue_row["evaluation_return_mean"], "")
        self.assertEqual(c51_row["status"], "")
        self.assertEqual(c51_row["total_tokens"], "")
        self.assertEqual(c51_row["training_steps"], "500000")
        for row in rows:
            self.assertEqual(row["provider"], "zai")
            self.assertEqual(row["model"], "glm-5.2")
            self.assertEqual(row["provider_base"], "coding")

        payload = io.StringIO()
        writer = csv.DictWriter(payload, fieldnames=METRIC_FIELDS, lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)
        payload.seek(0)
        parsed = read_metrics_csv(payload, manifest)
        self.assertEqual(len(parsed), 4)
        self.assertEqual(parsed[0]["sources"], ("factory", "model"))
        self.assertEqual(parsed[2]["sources"], ("kubernetes", "gpu", "rl"))

        with self.assertRaises(ContractError):
            collect.merge_observations(observations[:-1], manifest)
        with self.assertRaises(ContractError):
            collect.merge_observations([*observations, observations[0]], manifest)

        truncated_rl = validate_observation(
            {
                "run_id": "cleanrl-c51-gb10",
                "source": "rl",
                "metrics": {
                    "training_steps": 1,
                    "evaluation_episodes": 1,
                    "evaluation_return_mean": 1.0,
                    "evaluation_return_stddev": 0.0,
                },
            }
        )
        replaced = [
            truncated_rl
            if observation["run_id"] == "cleanrl-c51-gb10" and observation["source"] == "rl"
            else observation
            for observation in observations
        ]
        with self.assertRaises(ContractError):
            collect.merge_observations(replaced, manifest)


class ManifestAndRendererTests(unittest.TestCase):
    def test_manifest_is_exact_two_plus_two_issue_specific_cuda_plan(self) -> None:
        manifest = load_manifest()
        self.assertEqual(
            manifest["agent_profile"],
            {"provider": "zai", "model": "glm-5.2", "base": "coding"},
        )
        self.assertEqual({entry["issue"]["number"] for entry in manifest["issue_jobs"]}, {488, 562})
        self.assertEqual(len(manifest["issue_jobs"]), 2)
        self.assertEqual(len(manifest["rl_runs"]), 2)
        collection = manifest["collection"]
        self.assertEqual(collection["namespace"], "software-factory-execution")
        self.assertEqual(collection["pod_selector_label"], "software-factory.io/job-id")
        self.assertEqual(
            collection["running_pod_command"],
            [
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
        )
        self.assertEqual(collection["gpu_sample_duration_seconds"], 120)
        self.assertEqual(collection["gpu_sample_interval_seconds"], 1)
        self.assertEqual(
            collection["gpu_sample_command"][:8],
            [
                "timeout",
                "120s",
                "kubectl",
                "--namespace",
                "software-factory-execution",
                "exec",
                "POD_NAME",
                "--",
            ],
        )
        self.assertEqual(collection["gpu_sample_command"][8:10], ["sh", "-lc"])
        self.assertIn(
            "--query-gpu=index,name,utilization.gpu,memory.used,power.draw",
            collection["gpu_sample_command"][-1],
        )
        self.assertIn("pgrep -f '[c]leanrl/c51.py", collection["gpu_sample_command"][-1])
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        self.assertNotIn("--run-id cleanrl-488-factory", readme)
        self.assertIn("factory configure --provider zai --model glm-5.2 --base coding", readme)
        self.assertIn(
            "ISSUE_RUN_ID=cleanrl-488-factory",
            readme,
        )
        self.assertIn(
            "ISSUE_RUN_ID=cleanrl-562-factory",
            readme,
        )
        self.assertIn("RL_LOG_PATH=.factory/benchmark/c51-gb10.log", readme)
        self.assertIn("RL_LOG_PATH=.factory/benchmark/c51-a100.log", readme)
        self.assertNotRegex(readme, r"\bkubectl\s+(?:get|exec)\b")
        self.assertIn("--field-selector status.phase=Running", readme)
        self.assertIn("for _ in $(seq 1 180)", readme)
        self.assertIn('if [ "$#" -eq 1 ]', readme)
        self.assertIn('elif [ "$#" -gt 1 ]', readme)
        self.assertIn("execution Pod did not become Running in time", readme)
        self.assertIn(
            "timeout 120s kubectl --namespace software-factory-execution exec",
            readme,
        )
        self.assertEqual(readme.count("sudo k3s ctr images tag --force"), 2)
        self.assertEqual(readme.count("factory run --detach --no-apply --no-clarify"), 2)
        self.assertIn("FACTORY_KUBERNETES_NODE_NAME=spark-91b3", readme)
        self.assertIn("FACTORY_KUBERNETES_NODE_NAME=kent-ai-stuff", readme)
        self.assertIn("FACTORY_KUBERNETES_RUNTIME_CLASS=nvidia", readme)
        self.assertIn("FACTORY_KUBERNETES_GPU_RESOURCE=nvidia.com/gpu", readme)
        self.assertIn("FACTORY_KUBERNETES_GPU_COUNT=1", readme)
        self.assertIn('export KUBECONFIG="$FACTORY_KUBERNETES_KUBECONFIG"', readme)
        self.assertIn(
            "$FACTORY_KUBERNETES_WORKSPACE_HOST_DIR/jobs/$JOB_ID/$RL_LOG_PATH",
            readme,
        )
        self.assertIn("factory export` exports only the Git patch", readme)
        self.assertIn("factory_training_steps=500000", readme)
        phase_a = readme.index("Phase A: while C51 is running")
        phase_b = readme.index("Phase B: only after terminal success")
        self.assertLess(phase_a, phase_b)
        self.assertLess(readme.index("C51_OUT/kubernetes.json", phase_a), phase_b)
        self.assertLess(phase_b, readme.index("ISSUE_OUT/model.json", phase_b))
        for directory in (
            "data/cleanrl-488-factory/*.json",
            "data/cleanrl-562-factory/*.json",
            "data/cleanrl-c51-gb10/*.json",
            "data/cleanrl-c51-a100/*.json",
        ):
            self.assertIn(directory, readme)
        self.assertLess(readme.index("prompts/issue-488.md"), readme.index("prompts/issue-562.md"))
        self.assertNotRegex(readme, r"(?i)(api[_ -]?key|sk-[a-z0-9])\s*=")
        profiles = manifest["execution_profiles"]
        expected_execution = {
            "gb10": {
                "platform": "linux/arm64",
                "gpu_class": "NVIDIA GB10",
                "gpu_name": "NVIDIA GB10",
                "node_name": "spark-91b3",
                "digest": "sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557",
            },
            "a100": {
                "platform": "linux/amd64",
                "gpu_class": "NVIDIA A100",
                "gpu_name": "NVIDIA A100-SXM4-40GB",
                "node_name": "kent-ai-stuff",
                "digest": "sha256:4d518b4a8304930a98ecd214ee580e360291c055219c1eaf0b1bef887a4b4673",
            },
        }
        for profile_name, expected in expected_execution.items():
            profile = profiles[profile_name]
            self.assertEqual(profile["platform"], expected["platform"])
            self.assertEqual(profile["gpu_class"], expected["gpu_class"])
            self.assertEqual(profile["gpu_name"], expected["gpu_name"])
            self.assertEqual(profile["namespace"], "software-factory-execution")
            self.assertEqual(profile["node_name"], expected["node_name"])
            self.assertEqual(profile["runtime_class"], "nvidia")
            self.assertEqual(profile["gpu_resource"], "nvidia.com/gpu")
            self.assertEqual(profile["gpu_count"], 1)
            self.assertEqual(profile["node_local_image"], "software-factory-gpu:benchmark-2b88673")
            self.assertEqual(profile["node_local_content_digest"], expected["digest"])
            self.assertEqual(
                profile["execution_image"],
                f"docker.io/library/software-factory-gpu@{expected['digest']}",
            )
            self.assertIsNone(profile["public_image_digest"])

        self.assertTrue(profiles["gb10"]["system_site_packages"])
        self.assertEqual(
            profiles["gb10"]["setup_commands"],
            [
                "python3.12 -m venv --system-site-packages .venv",
                "uv pip install --python .venv/bin/python --require-hashes --no-deps -r .factory/benchmark/gb10.txt",
            ],
        )
        self.assertFalse(profiles["a100"]["system_site_packages"])
        self.assertEqual(
            profiles["a100"]["setup_commands"],
            [
                "uv python install 3.10.16",
                "UV_PYTHON=3.10.16 uv sync --frozen --extra envpool --extra pytest --no-dev",
            ],
        )
        for profile in profiles.values():
            preflight = profile["cuda_preflight_command"][2]
            for token in (
                "torch.cuda.is_available()",
                "torch.cuda.set_device(0)",
                "model=torch.nn.Linear",
                "optimizer.zero_grad()",
                "loss.backward()",
                "optimizer.step()",
                "parameters()).device.type == 'cuda'",
                "not torch.equal(old, new)",
            ):
                self.assertIn(token, preflight)
        issue_profiles = {entry["id"]: entry["execution_profile"] for entry in manifest["issue_jobs"]}
        self.assertEqual(issue_profiles, {"cleanrl-488-factory": "gb10", "cleanrl-562-factory": "a100"})
        self.assertEqual(
            {entry["id"]: entry["prompt_file"] for entry in manifest["issue_jobs"]},
            {
                "cleanrl-488-factory": "benchmarks/cleanrl-gpu/prompts/issue-488.md",
                "cleanrl-562-factory": "benchmarks/cleanrl-gpu/prompts/issue-562.md",
            },
        )
        issue_commands = "\n".join(
            command for entry in manifest["issue_jobs"] for command in entry["validation_commands"]
        )
        self.assertIn("cleanrl/ppo.py --env-id FrozenLake-v1", issue_commands)
        self.assertIn("cleanrl/ppo_atari_envpool.py --env-id Breakout-v5", issue_commands)
        expected_command = [
            ".venv/bin/python", "cleanrl/c51.py", "--env-id", "CartPole-v1",
            "--seed", "1", "--total-timesteps", "500000", "--save-model",
        ]
        for run in manifest["rl_runs"]:
            self.assertEqual(run["framework"], "pytorch")
            self.assertEqual(run["role"], "product-validation")
            self.assertEqual(run["algorithm"], "c51")
            self.assertEqual(run["command"], expected_command)
            self.assertEqual(run["evaluation_episodes"], 10)
            self.assertEqual(run["log_signals"], ["global_step", "episodic_return", "SPS", "eval_episode"])
            self.assertTrue(run["cuda_preflight_required"])
        self.assertEqual(
            {run["id"]: run["log_path"] for run in manifest["rl_runs"]},
            {
                "cleanrl-c51-gb10": ".factory/benchmark/c51-gb10.log",
                "cleanrl-c51-a100": ".factory/benchmark/c51-a100.log",
            },
        )
        self.assertEqual(
            {run["id"]: run["execution_profile"] for run in manifest["rl_runs"]},
            {"cleanrl-c51-gb10": "gb10", "cleanrl-c51-a100": "a100"},
        )

    def test_dependency_locks_and_issue_prompts_are_pinned(self) -> None:
        manifest = load_manifest()
        profiles = manifest["execution_profiles"]
        gb10 = profiles["gb10"]
        for field in ("dependency_input", "dependency_lock"):
            path = REPOSITORY_ROOT / gb10[field]
            self.assertTrue(path.is_file(), path)
            actual = hashlib.sha256(path.read_bytes()).hexdigest()
            self.assertEqual(actual, gb10[f"{field}_sha256"])

        gb10_input = (REPOSITORY_ROOT / gb10["dependency_input"]).read_text(encoding="utf-8")
        gb10_lock = (REPOSITORY_ROOT / gb10["dependency_lock"]).read_text(encoding="utf-8")
        self.assertIn("uv pip compile --generate-hashes --python-version 3.12", gb10_lock)
        self.assertGreater(gb10_lock.count("--hash=sha256:"), 39)
        self.assertNotRegex(gb10_input.lower(), r"(?m)^torch(?:[<=> ]|$)")
        self.assertNotRegex(gb10_lock.lower(), r"(?m)^torch==")
        self.assertEqual(profiles["a100"]["dependency_input"], "pyproject.toml")
        self.assertEqual(
            profiles["a100"]["dependency_lock_sha256"],
            "34ecd77065f7f99fabac27a8e562ba4894142e67774cb98d120ab527bb44df5b",
        )

        expected_prompt_tokens = {
            "issue-488.md": (
                "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb",
                "tests/test_ppo_toytext.py",
                "cleanrl/ppo.py --env-id FrozenLake-v1",
                "c51-gb10.log",
            ),
            "issue-562.md": (
                "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb",
                "tests/test_ppo_atari_envpool_dummy_step.py",
                "cleanrl/ppo_atari_envpool.py --env-id Breakout-v5",
                "c51-a100.log",
            ),
        }
        for name, tokens in expected_prompt_tokens.items():
            prompt = (ROOT / "prompts" / name).read_text(encoding="utf-8")
            normalized_prompt = " ".join(prompt.split())
            self.assertLess(len(prompt.split()), 220)
            for token in (
                *tokens,
                ".factory/benchmark/",
                "--save-model",
                "factory_training_steps=500000",
                "Do not push upstream or open a pull request.",
                "Do not print raw logs.",
                "optimizer.step()",
            ):
                self.assertIn(" ".join(token.split()), normalized_prompt)

    def test_public_image_digest_is_nullable_but_strict(self) -> None:
        manifest = load_manifest()
        manifest["execution_profiles"]["gb10"]["public_image_digest"] = "sha256:" + "a" * 64
        validate_manifest(manifest)
        manifest["execution_profiles"]["gb10"]["public_image_digest"] = "pending"
        with self.assertRaises(ContractError):
            validate_manifest(manifest)

        manifest = load_manifest()
        manifest["agent_profile"]["model"] = "another-model"
        with self.assertRaises(ContractError):
            validate_manifest(manifest)

    def test_header_only_metrics_do_not_emit_charts(self) -> None:
        manifest = load_manifest()
        metrics_path = ROOT / "data" / "metrics.csv"
        original = metrics_path.read_bytes()
        with metrics_path.open(encoding="utf-8", newline="") as stream:
            rows = read_metrics_csv(stream, manifest)
        self.assertEqual(rows, [])
        with tempfile.TemporaryDirectory() as directory, redirect_stderr(io.StringIO()):
            result = render_charts.main(
                ["--manifest", str(ROOT / "run-manifest.json"), "--metrics", str(metrics_path), "--output-dir", directory]
            )
            self.assertEqual(result, 2)
            self.assertEqual(list(Path(directory).iterdir()), [])
        self.assertEqual(metrics_path.read_bytes(), original)

    def test_rendering_is_byte_deterministic(self) -> None:
        manifest = load_manifest()
        rows = [
            {
                "run_id": "cleanrl-c51-gb10",
                "sources": ("model", "gpu"),
                "response_count": 3,
                "total_tokens": 123,
                "gpu_utilization_mean_pct": 50.0,
                "gpu_memory_peak_mib": 1024.0,
            }
        ]
        svg = render_charts.render_svg(manifest, rows)
        self.assertEqual(svg, render_charts.render_svg(manifest, rows))
        self.assertIn(b"123 tokens / 3 responses", svg)
        first = render_charts.render_png(manifest, rows)
        second = render_charts.render_png(manifest, rows)
        self.assertEqual(first, second)
        self.assertTrue(first.startswith(b"\x89PNG\r\n\x1a\n"))

    def test_metrics_csv_header_is_exact(self) -> None:
        manifest = load_manifest()
        with (ROOT / "data" / "metrics.csv").open(encoding="utf-8", newline="") as stream:
            header = next(csv.reader(stream))
        self.assertEqual(tuple(header), METRIC_FIELDS)
        bad = io.StringIO("run_id,unexpected\n")
        with self.assertRaises(ContractError):
            read_metrics_csv(bad, manifest)

        truncated = {field: "" for field in METRIC_FIELDS}
        truncated.update(
            {
                "run_id": "cleanrl-c51-gb10",
                "provider": "zai",
                "model": "glm-5.2",
                "provider_base": "coding",
                "training_steps": "1",
                "evaluation_episodes": "1",
                "evaluation_return_mean": "1",
                "evaluation_return_stddev": "0",
            }
        )
        payload = io.StringIO()
        writer = csv.DictWriter(payload, fieldnames=METRIC_FIELDS, lineterminator="\n")
        writer.writeheader()
        writer.writerow(truncated)
        payload.seek(0)
        with self.assertRaises(ContractError):
            read_metrics_csv(payload, manifest)

        wrong_identity = {field: "" for field in METRIC_FIELDS}
        wrong_identity.update(
            {
                "run_id": "cleanrl-c51-gb10",
                "provider": "openai",
                "model": "glm-5.2",
                "provider_base": "coding",
                **{field: "0" for field in MODEL_METRICS},
            }
        )
        payload = io.StringIO()
        writer = csv.DictWriter(payload, fieldnames=METRIC_FIELDS, lineterminator="\n")
        writer.writeheader()
        writer.writerow(wrong_identity)
        payload.seek(0)
        with self.assertRaises(ContractError):
            read_metrics_csv(payload, manifest)


if __name__ == "__main__":
    unittest.main()
