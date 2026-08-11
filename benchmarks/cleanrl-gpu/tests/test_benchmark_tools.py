from __future__ import annotations

import csv
import hashlib
import io
import json
import re
import sys
import tempfile
import unittest
import zipfile
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


def pytorch_checkpoint_fixture() -> bytes:
    payload = io.BytesIO()
    with zipfile.ZipFile(payload, "w", compression=zipfile.ZIP_STORED) as archive:
        archive.writestr("c51/data.pkl", b"state-dict-fixture")
        archive.writestr("c51/byteorder", b"little")
        archive.writestr("c51/version", b"3\n")
        archive.writestr("c51/.data/serialization_id", b"fixture-id")
        archive.writestr("c51/data/0", b"x" * 4_096)
    return payload.getvalue()


CHECKPOINT_PAYLOAD = pytorch_checkpoint_fixture()
CHECKPOINT_SHA256 = hashlib.sha256(CHECKPOINT_PAYLOAD).hexdigest()
FIXTURE_JOB_ID = "12345678-1234-4123-8123-123456789abc"


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


def rollout_usage(multiplier: int = 1) -> dict:
    return {
        "total_tokens": 100 * multiplier,
        "input_tokens": 70 * multiplier,
        "cached_input_tokens": 20 * multiplier,
        "cache_write_input_tokens": 5 * multiplier,
        "output_tokens": 30 * multiplier,
        "reasoning_output_tokens": 10 * multiplier,
    }


def rollout_records(thread_id: str = "thread-1") -> list[dict]:
    first = rollout_usage()
    second = rollout_usage(2)
    cumulative = {field: first[field] + second[field] for field in first}
    return [
        {"type": "session_meta", "payload": {"id": thread_id, "prompt": SECRET}},
        {
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {"total_token_usage": first, "last_token_usage": first},
            },
        },
        {
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {"total_token_usage": first, "last_token_usage": first},
            },
        },
        {
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {"total_token_usage": cumulative, "last_token_usage": second},
            },
        },
    ]


def configured_job_input(*, provider: str = "zai", model: str = "glm-5.2") -> dict:
    return {
        "task": SECRET,
        "executionProfile": {"provider": provider, "model": model},
    }


def profile_show() -> str:
    return "\n".join(collect.PROFILE_SHOW_LINES) + "\n"


def factory_payload(*, state: str = "succeeded", task: str = "benchmark fixture task") -> dict:
    operations = [
        {
            "operationId": f"op-{name}",
            "ordinal": ordinal,
            "kind": f"codex.{name}",
            "state": "succeeded",
        }
        for ordinal, name in enumerate(("plan", "execute", "review", "remediate"))
    ]
    repository_url = "https://github.com/vwxyzjn/cleanrl.git"
    repository_id = "remote:" + hashlib.sha256(repository_url.encode()).hexdigest()
    return {
        "job": {
            "job": {
                "jobId": FIXTURE_JOB_ID,
                "kind": "factory.task",
                "state": state,
                "createdAt": "2026-08-11T12:00:00Z",
                "updatedAt": "2026-08-11T12:01:30Z",
                "input": {
                    **configured_job_input(),
                    "repositoryId": repository_id,
                    "task": task,
                },
            },
            "operations": operations,
        },
        "stageCheckpoints": [
            {
                "operationId": operation["operationId"],
                "ordinal": operation["ordinal"],
                "operationKind": operation["kind"],
                "checkpoint": {
                    "checkpointId": f"checkpoint-{operation['ordinal']}",
                    "attemptId": f"attempt-{operation['ordinal']}",
                    "sequence": 1,
                    "kind": "factory.stage",
                    "payload": {
                        "operation": operation["kind"],
                        "phase": "completed",
                    },
                    "workspaceRoot": f"/workspaces/jobs/{FIXTURE_JOB_ID}",
                    "workspaceRevision": "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb",
                    "createdAt": "2026-08-11T12:01:00Z",
                },
            }
            for operation in operations
        ],
        "attempts": [
            {
                "attemptId": f"attempt-{operation['ordinal']}",
                "operationId": operation["operationId"],
                "attemptNumber": 1,
                "state": "succeeded",
                "failure": None,
            }
            for operation in operations
        ],
        "fullResult": {"markdown": "Benchmark completed."},
    }


def model_api_job(
    job_id: str,
    *,
    state: str = "succeeded",
    task: str = "benchmark fixture task",
    provider: str = "zai",
    model: str = "glm-5.2",
) -> dict:
    job = factory_payload(state=state, task=task)["job"]["job"]
    job["jobId"] = job_id
    job["input"]["executionProfile"] = {"provider": provider, "model": model}
    return job


def workspace_payload() -> dict:
    repository_url = "https://github.com/vwxyzjn/cleanrl.git"
    repository_id = "remote:" + hashlib.sha256(repository_url.encode()).hexdigest()
    return {
        "jobId": FIXTURE_JOB_ID,
        "repositoryId": repository_id,
        "repository": repository_url,
        "baseRef": "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb",
        "baseRevision": "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb",
        "branchName": f"factory/{FIXTURE_JOB_ID}",
        "root": f"/workspaces/jobs/{FIXTURE_JOB_ID}",
        "revision": "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb",
        "state": "active",
    }


def manifest_for_task(manifest: dict, run_id: str, task: str) -> dict:
    value = json.loads(json.dumps(manifest))
    entry = next(item for item in value["issue_jobs"] if item["id"] == run_id)
    entry["submitted_task_sha256"] = hashlib.sha256(task.encode()).hexdigest()
    return value


def collect_factory_fixture(run_id: str, payload: dict, manifest: dict) -> dict:
    task = payload["job"]["job"]["input"]["task"]
    return collect.collect_factory(
        run_id,
        FIXTURE_JOB_ID,
        payload,
        workspace_payload(),
        manifest_for_task(manifest, run_id, task),
    )


def pod_payload(profile_name: str, *, node_name: str | None = None) -> dict:
    profile = load_manifest()["execution_profiles"][profile_name]
    return {
        "items": [
            {
                "metadata": {
                    "name": SECRET,
                    "labels": {"software-factory.io/job-id": FIXTURE_JOB_ID},
                },
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
                            "workingDir": f"/workspaces/jobs/{FIXTURE_JOB_ID}",
                            "volumeMounts": [
                                {
                                    "name": "workspace",
                                    "mountPath": f"/workspaces/jobs/{FIXTURE_JOB_ID}",
                                    "subPath": f"jobs/{FIXTURE_JOB_ID}",
                                }
                            ],
                        }
                    ],
                    "volumes": [
                        {
                            "name": "workspace",
                            "persistentVolumeClaim": {
                                "claimName": "software-factory-workspaces"
                            },
                        }
                    ],
                },
                "status": {
                    "phase": "Running",
                    "containerStatuses": [
                        {
                            "restartCount": 2,
                            "imageID": f"docker-pullable://image@{profile['resolved_image_digest']}",
                        }
                    ],
                },
            }
        ]
    }


def gpu_stdout(profile_name: str, *, gpu_name: str | None = None) -> str:
    name = gpu_name or load_manifest()["execution_profiles"][profile_name]["gpu_name"]
    return "".join(
        f"2026/08/11 12:{sample // 60:02d}:{sample % 60:02d}.000, "
        f"0, {name}, {50 if sample % 2 == 0 else 70}, "
        f"{100 if sample % 2 == 0 else 300}, {20 if sample % 2 == 0 else 'N/A'}\n"
        for sample in range(120)
    )


def rl_stdout() -> str:
    return "\n".join(
        [
            "global_step=499700, episodic_return=100.0",
            "SPS: 1800",
            *(f"eval_episode={episode}, episodic_return=1.0" for episode in range(10)),
            "factory_training_steps=500000",
        ]
    )


def cuda_receipt(profile_name: str) -> str:
    profile = load_manifest()["execution_profiles"][profile_name]
    return (
        f"GPU name: {profile['gpu_name']}\n"
        "weight changed: True\n"
        "bias changed: True\n"
        "CUDA PREFLIGHT PASS\n"
    )


def issue_logs(run_id: str, profile_name: str) -> tuple[str, str, str]:
    entry = next(item for item in load_manifest()["issue_jobs"] if item["id"] == run_id)
    cuda = cuda_receipt(profile_name)
    pytest = f"{'.' * entry['focused_test_count']} [100%]\n{entry['focused_test_count']} passed in 1.25s\n"
    observed = (
        entry["ppo_training_steps"]
        // entry["ppo_rollout_size"]
        * entry["ppo_rollout_size"]
    )
    ppo = f"global_step={observed}, episodic_return=[1.]\nSPS: 321\n"
    return cuda, pytest, ppo


def complete_observations(manifest: dict) -> list[dict]:
    observations: list[dict] = []
    for run_id in ("cleanrl-488-factory", "cleanrl-562-factory"):
        observations.extend(
            [
                collect.collect_profile(run_id, profile_show(), manifest),
                collect_factory_fixture(run_id, factory_payload(), manifest),
                collect.collect_model(
                    run_id,
                    [
                        {"events": [job_event(1, "model.usage", usage_payload())], "nextCursor": 1},
                        {"events": [], "nextCursor": 1},
                    ],
                ),
                collect.collect_issue(
                    run_id,
                    *issue_logs(
                        run_id,
                        "gb10" if run_id == "cleanrl-488-factory" else "a100",
                    ),
                    manifest,
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
                collect.collect_kubernetes(
                    run_id, FIXTURE_JOB_ID, pod_payload(profile_name), manifest
                ),
                collect.collect_gpu(run_id, gpu_stdout(profile_name), 1, manifest),
                collect.collect_rl(
                    run_id,
                    rl_stdout(),
                    cuda_receipt(profile_name),
                    CHECKPOINT_PAYLOAD,
                    manifest,
                ),
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

    def test_model_usage_can_be_recovered_from_attributed_rollout_counters(self) -> None:
        pages = [
            {
                "events": [
                    job_event(
                        1,
                        "turn.started",
                        {"threadId": "thread-1", "turnId": SECRET, "status": "inProgress"},
                    ),
                    job_event(2, "tool.started", {"arguments": SECRET}),
                ],
                "nextCursor": 2,
            },
            {"events": [], "nextCursor": 2},
        ]

        observation = collect.collect_model(
            "cleanrl-488-factory", pages, [rollout_records()]
        )

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
                "tool_started_count": 1,
                "context_compacted_count": 0,
            },
        )
        self.assertNotIn(SECRET, json.dumps(observation))

    def test_model_rollout_recovery_rejects_wrong_thread_or_counter_delta(self) -> None:
        pages = [
            {
                "events": [
                    job_event(
                        1,
                        "turn.started",
                        {"threadId": "thread-1", "turnId": SECRET, "status": "inProgress"},
                    )
                ],
                "nextCursor": 1,
            },
            {"events": [], "nextCursor": 1},
        ]
        invalid_delta = rollout_records()
        invalid_delta[-1]["payload"]["info"]["last_token_usage"]["input_tokens"] += 1

        for records in (rollout_records("different-thread"), invalid_delta):
            with self.subTest(records=records), self.assertRaises(ContractError):
                collect.collect_model("cleanrl-488-factory", pages, [records])

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

        with self.assertRaises(ContractError):
            collect.collect_model(
                "cleanrl-c51-gb10",
                [
                    {"events": [valid_event], "nextCursor": 1},
                    {"events": [], "nextCursor": 1},
                ],
                expected_job_id="cli-job-id",
            )

    def test_model_api_fetch_checks_terminal_job_then_advances_cursor(self) -> None:
        task = "benchmark fixture task"
        manifest = manifest_for_task(
            load_manifest(), "cleanrl-488-factory", task
        )
        responses = [
            {
                "job": model_api_job("job/id", task=task),
                "operations": [],
            },
            {
                "events": [job_event(4, "tool.started", {}, job_id="job/id")],
                "nextCursor": 4,
            },
            {
                "events": [job_event(9, "context.compacted", {}, job_id="job/id")],
                "nextCursor": 9,
            },
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
                    "http://factoryd:8787/",
                    "job/id",
                    "cleanrl-488-factory",
                    manifest,
                    1000,
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
        task = "benchmark fixture task"
        manifest = manifest_for_task(
            load_manifest(), "cleanrl-488-factory", task
        )
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
                        "job": model_api_job(
                            "job/id", state="running", task=task
                        ),
                        "operations": [],
                    }
                ).encode()
            )

        with mock.patch.object(collect.urllib.request, "urlopen", side_effect=open_response):
            with self.assertRaises(ContractError):
                list(
                    collect.fetch_job_event_pages(
                        "http://factoryd:8787/",
                        "job/id",
                        "cleanrl-488-factory",
                        manifest,
                        1000,
                    )
                )
        self.assertEqual(requests, ["http://factoryd:8787/jobs/job%2Fid"])

    def test_model_api_fetch_rejects_identity_mismatches_before_events(self) -> None:
        task = "benchmark fixture task"
        manifest = manifest_for_task(
            load_manifest(), "cleanrl-488-factory", task
        )
        valid_job = model_api_job("job/id", task=task)
        mutations = {
            "job id": lambda job: job.update({"jobId": "different-job"}),
            "kind": lambda job: job.update({"kind": "other.task"}),
            "repository": lambda job: job["input"].update(
                {"repositoryId": "remote:" + "0" * 64}
            ),
            "task": lambda job: job["input"].update({"task": "different task"}),
            "provider": lambda job: job["input"]["executionProfile"].update(
                {"provider": "deepseek"}
            ),
            "model": lambda job: job["input"]["executionProfile"].update(
                {"model": "other-model"}
            ),
        }

        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                self.close()

        for name, mutate in mutations.items():
            job = json.loads(json.dumps(valid_job))
            mutate(job)
            requests: list[str] = []

            def open_response(request, timeout):
                self.assertEqual(timeout, 30)
                requests.append(request.full_url)
                return Response(json.dumps({"job": job, "operations": []}).encode())

            with self.subTest(name=name), mock.patch.object(
                collect.urllib.request, "urlopen", side_effect=open_response
            ):
                with self.assertRaises(ContractError):
                    list(
                        collect.fetch_job_event_pages(
                            "http://factoryd:8787/",
                            "job/id",
                            "cleanrl-488-factory",
                            manifest,
                            1000,
                        )
                    )
            self.assertEqual(requests, ["http://factoryd:8787/jobs/job%2Fid"])

    def test_factory_workspace_receipt_is_loaded_without_a_shell_curl_step(self) -> None:
        requests: list[str] = []

        class Response(io.BytesIO):
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                self.close()

        def open_response(request, timeout):
            self.assertEqual(timeout, 30)
            requests.append(request.full_url)
            return Response(json.dumps(workspace_payload()).encode())

        with mock.patch.object(collect.urllib.request, "urlopen", side_effect=open_response):
            result = collect.fetch_job_workspace("http://factoryd:8787/", FIXTURE_JOB_ID)
        self.assertEqual(result, workspace_payload())
        self.assertEqual(
            requests,
            [f"http://factoryd:8787/jobs/{FIXTURE_JOB_ID}/workspace"],
        )
        with self.assertRaises(ContractError):
            collect.fetch_job_workspace("http://factoryd:8787", "not-a-job-id")

    def test_factory_parses_real_envelope_and_drops_hostile_fields(self) -> None:
        payload = factory_payload()
        payload["job"]["job"].update(
            {"failure": SECRET, "path": SECRET, "name": SECRET}
        )
        for operation in payload["job"]["operations"]:
            operation["input"] = {"task": SECRET}
        for checkpoint in payload["stageCheckpoints"]:
            checkpoint["checkpoint"]["payload"]["hostile"] = SECRET
        payload["attempts"][2:3] = [
            {
                "attemptId": "attempt-review-1",
                "operationId": "op-review",
                "attemptNumber": 1,
                "state": "failed",
                "failure": {"raw": SECRET},
            },
            {
                "attemptId": "attempt-review-2",
                "operationId": "op-review",
                "attemptNumber": 2,
                "state": "succeeded",
                "failure": None,
            },
        ]
        payload["stageCheckpoints"][2]["checkpoint"]["attemptId"] = "attempt-review-2"
        payload["fullResult"].update({"reasoning": SECRET, "localPath": SECRET})
        payload["prompt"] = SECRET
        manifest = load_manifest()
        observation = collect_factory_fixture("cleanrl-488-factory", payload, manifest)
        self.assertEqual(
            observation["metrics"],
            {
                "status": "succeeded",
                "wall_seconds": 90.0,
                "operation_count": 4,
                "completed_operations": 4,
                "stage_checkpoint_count": 4,
                "attempt_count": 5,
                "retry_count": 1,
                "stage_sequence_verified": 1,
            },
        )
        serialized = json.dumps(observation)
        self.assertNotIn(SECRET, serialized)
        for forbidden in ("prompt", "reasoning", "failure", "path", "name", "payload", "task"):
            self.assertNotIn(forbidden, serialized.lower())

        reproduction_manifest = load_manifest()
        reproduction_entry = next(
            entry
            for entry in reproduction_manifest["issue_jobs"]
            if entry["id"] == "cleanrl-488-factory"
        )
        reproduction_entry["submitted_task_sha256"] = "0" * 64
        reproduction_entry["reproduction_task_sha256"] = hashlib.sha256(
            payload["job"]["job"]["input"]["task"].encode()
        ).hexdigest()
        collect.collect_factory(
            "cleanrl-488-factory",
            FIXTURE_JOB_ID,
            payload,
            workspace_payload(),
            reproduction_manifest,
        )

        for state in ("failed", "cancelled"):
            with self.subTest(state=state), self.assertRaises(ContractError):
                collect_factory_fixture(
                    "cleanrl-488-factory", factory_payload(state=state), manifest
                )

        invalid_successes = []
        failed_operation = factory_payload()
        failed_operation["job"]["operations"][1]["state"] = "failed"
        invalid_successes.append(failed_operation)
        duplicate_checkpoint = factory_payload()
        duplicate_checkpoint["stageCheckpoints"][1]["operationId"] = "op-plan"
        invalid_successes.append(duplicate_checkpoint)
        swapped_checkpoints = factory_payload()
        (
            swapped_checkpoints["stageCheckpoints"][0]["operationId"],
            swapped_checkpoints["stageCheckpoints"][1]["operationId"],
        ) = (
            swapped_checkpoints["stageCheckpoints"][1]["operationId"],
            swapped_checkpoints["stageCheckpoints"][0]["operationId"],
        )
        invalid_successes.append(swapped_checkpoints)
        skipped_attempt = factory_payload()
        skipped_attempt["attempts"][0]["attemptNumber"] = 2
        invalid_successes.append(skipped_attempt)
        missing_checkpoint = factory_payload()
        missing_checkpoint["stageCheckpoints"][0]["checkpoint"] = {}
        invalid_successes.append(missing_checkpoint)
        missing_result = factory_payload()
        missing_result["fullResult"] = None
        invalid_successes.append(missing_result)
        for invalid in invalid_successes:
            with self.subTest(invalid=invalid), self.assertRaises(ContractError):
                collect_factory_fixture("cleanrl-488-factory", invalid, manifest)

        mismatched = json.loads(json.dumps(payload))
        mismatched["job"]["job"]["input"]["executionProfile"]["provider"] = "deepseek"
        with self.assertRaises(ContractError):
            collect_factory_fixture("cleanrl-488-factory", mismatched, manifest)

        unrelated = factory_payload(task="unrelated task")
        expected_manifest = manifest_for_task(
            manifest, "cleanrl-488-factory", "benchmark fixture task"
        )
        with self.assertRaises(ContractError):
            collect.collect_factory(
                "cleanrl-488-factory",
                FIXTURE_JOB_ID,
                unrelated,
                workspace_payload(),
                expected_manifest,
            )

        wrong_workspace = workspace_payload()
        wrong_workspace["baseRevision"] = "0" * 40
        with self.assertRaises(ContractError):
            collect.collect_factory(
                "cleanrl-488-factory",
                FIXTURE_JOB_ID,
                factory_payload(),
                wrong_workspace,
                expected_manifest,
            )

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

    def test_issue_gate_collects_real_cuda_test_and_ppo_receipts(self) -> None:
        manifest = load_manifest()
        observation = collect.collect_issue(
            "cleanrl-488-factory",
            *issue_logs("cleanrl-488-factory", "gb10"),
            manifest,
        )
        self.assertEqual(
            observation["metrics"],
            {
                "cuda_optimizer_step_passed": 1,
                "focused_test_count": 2,
                "focused_test_seconds": 1.25,
                "ppo_configured_steps": 500000,
                "ppo_completed_steps": 499712,
                "ppo_last_logged_step": 499712,
                "ppo_completion_verified": 1,
                "ppo_final_sps": 321,
                "ppo_last_training_return": 1.0,
            },
        )
        cuda, pytest, ppo = issue_logs("cleanrl-488-factory", "gb10")
        legacy_below_boundary = ppo.replace("global_step=499712", "global_step=499200")
        with self.assertRaises(ContractError):
            collect.collect_issue(
                "cleanrl-488-factory",
                cuda,
                pytest,
                legacy_below_boundary,
                manifest,
            )
        receipt_observation = collect.collect_issue(
            "cleanrl-488-factory",
            cuda,
            pytest,
            legacy_below_boundary + "factory_ppo_training_steps=500000\n",
            manifest,
        )
        self.assertEqual(receipt_observation["metrics"]["ppo_last_logged_step"], 499200)
        self.assertEqual(receipt_observation["metrics"]["ppo_completed_steps"], 499712)
        with self.assertRaises(ContractError):
            collect.collect_issue(
                "cleanrl-488-factory",
                cuda,
                pytest,
                legacy_below_boundary
                + "factory_ppo_training_steps=500000\nextra output\n",
                manifest,
            )
        for inputs in (
            (cuda.replace("NVIDIA GB10", "NVIDIA GB100"), pytest, ppo),
            (cuda.replace("weight changed: True", "weight changed: False"), pytest, ppo),
            (cuda.replace("bias changed: True", "bias changed: False"), pytest, ppo),
            (cuda.replace("CUDA PREFLIGHT PASS", "CUDA PREFLIGHT FAIL"), pytest, ppo),
            (
                cuda.replace(
                    "CUDA PREFLIGHT PASS",
                    "CUDA backward + optimizer.step() PASS: parameter changed",
                ),
                pytest,
                ppo,
            ),
            (cuda, pytest.replace("2 passed", "1 passed"), ppo),
            (cuda, pytest, ppo.replace("SPS: 321", "SPS: 0")),
            (cuda, pytest + "1 failed in 2.00s\n", ppo),
            (cuda, pytest, ppo + "Traceback (most recent call last):\nboom\n"),
            (cuda, pytest, ppo + "factory_ppo_training_steps=499999\n"),
        ):
            with self.subTest(inputs=inputs), self.assertRaises(ContractError):
                collect.collect_issue("cleanrl-488-factory", *inputs, manifest)

    def test_issue_gate_accepts_the_two_measured_legacy_cuda_receipts(self) -> None:
        manifest = load_manifest()
        gb10_cuda = "\n".join(
            [
                "device: cuda:0 | NVIDIA GB10",
                "torch: 2.8.0a0+34c6371d24.nv25.08",
                "loss: 2.9546937942504883",
                "weight changed: True",
                "bias changed: True",
                "max |delta weight|: 0.3000860810279846",
                "weight on cuda: True",
                "tensor on cuda: True",
                "CUDA PREFLIGHT PASS",
            ]
        )
        _, gb10_pytest, gb10_ppo = issue_logs("cleanrl-488-factory", "gb10")
        self.assertEqual(
            collect.collect_issue(
                "cleanrl-488-factory", gb10_cuda, gb10_pytest, gb10_ppo, manifest
            )["metrics"]["cuda_optimizer_step_passed"],
            1,
        )

        a100_cuda = "\n".join(
            [
                "device: NVIDIA A100-SXM4-40GB",
                "loss: 1.379621",
                "param weight changed, max abs diff: 0.007483",
                "param bias changed, max abs diff: 0.005116",
                "CUDA backward + optimizer.step() PASS: parameter changed",
            ]
        )
        _, a100_pytest, a100_ppo = issue_logs("cleanrl-562-factory", "a100")
        self.assertEqual(
            collect.collect_issue(
                "cleanrl-562-factory", a100_cuda, a100_pytest, a100_ppo, manifest
            )["metrics"]["cuda_optimizer_step_passed"],
            1,
        )
        with self.assertRaises(ContractError):
            collect.collect_issue(
                "cleanrl-562-factory",
                a100_cuda.replace("0.005116", "0.0"),
                a100_pytest,
                a100_ppo,
                manifest,
            )

    def test_kubernetes_validates_profile_and_drops_metadata(self) -> None:
        manifest = load_manifest()
        payload = pod_payload("gb10")
        metrics = collect.collect_kubernetes(
            "cleanrl-c51-gb10", FIXTURE_JOB_ID, payload, manifest
        )["metrics"]
        self.assertEqual(metrics["pod_count"], 1)
        self.assertEqual(metrics["running_pods"], 1)
        self.assertEqual(metrics["succeeded_pods"], 0)
        self.assertEqual(metrics["unknown_pods"], 0)
        self.assertEqual(metrics["pod_restart_count"], 2)
        self.assertEqual(metrics["runtime_class_pods"], 1)
        self.assertEqual(metrics["isolated_workspace_pods"], 1)
        self.assertNotIn(SECRET, json.dumps(metrics))

        production_shape = json.loads(json.dumps(payload))
        mirror_id = "a" * 64
        production_shape["items"][0]["spec"]["containers"][0][
            "volumeMounts"
        ].append(
            {
                "name": "workspace",
                "mountPath": f"/workspaces/mirrors/{mirror_id}.git",
                "subPath": f"mirrors/{mirror_id}.git",
            }
        )
        production_metrics = collect.collect_kubernetes(
            "cleanrl-c51-gb10", FIXTURE_JOB_ID, production_shape, manifest
        )["metrics"]
        self.assertEqual(production_metrics["isolated_workspace_pods"], 1)

        invalid_mirror = json.loads(json.dumps(production_shape))
        invalid_mirror["items"][0]["spec"]["containers"][0]["volumeMounts"][
            1
        ]["subPath"] = "mirrors/not-a-repository.git"
        with self.assertRaises(ContractError):
            collect.collect_kubernetes(
                "cleanrl-c51-gb10", FIXTURE_JOB_ID, invalid_mirror, manifest
            )

        mutations = (
            ("spec", "nodeName", "wrong-node"),
            ("spec", "runtimeClassName", "runc"),
        )
        for section, field, value in mutations:
            mismatched = json.loads(json.dumps(payload))
            mismatched["items"][0][section][field] = value
            with self.subTest(field=field), self.assertRaises(ContractError):
                collect.collect_kubernetes(
                    "cleanrl-c51-gb10", FIXTURE_JOB_ID, mismatched, manifest
                )
        for mismatch in ("image", "gpu", "imageID", "workspace"):
            payload_copy = json.loads(json.dumps(payload))
            container = payload_copy["items"][0]["spec"]["containers"][0]
            if mismatch == "image":
                container["image"] = "wrong:image"
            elif mismatch == "gpu":
                container["resources"]["limits"]["nvidia.com/gpu"] = "0"
            else:
                if mismatch == "imageID":
                    payload_copy["items"][0]["status"]["containerStatuses"][0]["imageID"] = (
                        "containerd://sha256:" + "0" * 64
                    )
                else:
                    container["volumeMounts"][0]["subPath"] = "jobs/different"
            with self.subTest(mismatch=mismatch), self.assertRaises(ContractError):
                collect.collect_kubernetes(
                    "cleanrl-c51-gb10", FIXTURE_JOB_ID, payload_copy, manifest
                )

        broad_mount = json.loads(json.dumps(payload))
        broad_mount["items"][0]["spec"]["containers"][0]["volumeMounts"].append(
            {"name": "workspace", "mountPath": "/workspaces"}
        )
        with self.assertRaises(ContractError):
            collect.collect_kubernetes(
                "cleanrl-c51-gb10", FIXTURE_JOB_ID, broad_mount, manifest
            )
        with self.assertRaises(ContractError):
            collect.collect_kubernetes(
                "cleanrl-c51-gb10",
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                payload,
                manifest,
            )

    def test_gpu_parses_nvidia_smi_csv_and_omits_partial_power(self) -> None:
        raw = gpu_stdout("gb10")
        manifest = load_manifest()
        metrics = collect.collect_gpu("cleanrl-c51-gb10", raw, 1, manifest)["metrics"]
        self.assertEqual(metrics["gpu_count"], 1)
        self.assertEqual(metrics["sample_count"], 120)
        self.assertEqual(metrics["gpu_seconds"], 120.0)
        self.assertEqual(metrics["gpu_sample_span_seconds"], 119.0)
        self.assertEqual(metrics["gpu_utilization_mean_pct"], 60.0)
        self.assertEqual(metrics["gpu_memory_peak_mib"], 300.0)
        self.assertNotIn("gpu_power_mean_watts", metrics)

        with self.assertRaises(ContractError):
            collect.collect_gpu(
                "cleanrl-c51-gb10",
                raw.replace("NVIDIA GB10", "NVIDIA A100-SXM4-40GB"),
                1,
                manifest,
            )

    def test_gpu_accepts_unified_memory_na_without_fabricating_memory(self) -> None:
        raw = "".join(
            f"2026/08/11 12:{sample // 60:02d}:{sample % 60:02d}.000, "
            f"0, NVIDIA GB10, {20 if sample % 2 == 0 else 30}, [N/A], "
            f"{16.5 if sample % 2 == 0 else 17.5}\n"
            for sample in range(120)
        )
        metrics = collect.collect_gpu(
            "cleanrl-c51-gb10", raw, 1, load_manifest()
        )["metrics"]
        self.assertEqual(metrics["gpu_utilization_mean_pct"], 25.0)
        self.assertEqual(metrics["gpu_power_mean_watts"], 17.0)
        self.assertNotIn("gpu_memory_mean_mib", metrics)
        self.assertNotIn("gpu_memory_peak_mib", metrics)

    def test_gpu_rejects_a_single_snapshot_or_unbalanced_series(self) -> None:
        invalid_series = (
            "2026/08/11 12:00:00.000, 0, NVIDIA GB10, 50, 100, 20\n",
            "2026/08/11 12:00:00.000, 0, NVIDIA GB10, 50, 100, 20\n"
            "2026/08/11 12:00:01.000, 1, NVIDIA GB10, 70, 300, 40\n",
            "".join(
                "2026/08/11 12:00:00.000, 0, NVIDIA GB10, 50, 100, 20\n"
                for _ in range(120)
            ),
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
                "SPS: 1800",
                *(f"eval_episode={episode}, episodic_return=1.0" for episode in range(10)),
                f"reasoning={SECRET}",
                "factory_training_steps=500000",
            ]
        )
        observation = collect.collect_rl(
            "cleanrl-c51-gb10",
            stdout,
            cuda_receipt("gb10"),
            CHECKPOINT_PAYLOAD,
            load_manifest(),
        )
        self.assertEqual(
            observation["metrics"],
            {
                "training_steps": 500000,
                "final_observed_step": 499700,
                "training_sps": 1800,
                "evaluation_episodes": 10,
                "evaluation_return_mean": 1.0,
                "evaluation_return_stddev": 0.0,
                "checkpoint_bytes": len(CHECKPOINT_PAYLOAD),
                "checkpoint_sha256": CHECKPOINT_SHA256,
            },
        )
        self.assertNotIn(SECRET, json.dumps(observation))

    def test_rl_parses_upstream_vector_episode_returns(self) -> None:
        stdout = "\n".join(
            [
                "global_step=499517, episodic_return=[500.]",
                "SPS: 1900",
                *(f"eval_episode={episode}, episodic_return=[500.]" for episode in range(10)),
                "factory_training_steps=500000",
            ]
        )
        metrics = collect.collect_rl(
            "cleanrl-c51-gb10",
            stdout,
            cuda_receipt("gb10"),
            CHECKPOINT_PAYLOAD,
            load_manifest(),
        )["metrics"]
        self.assertEqual(metrics["evaluation_episodes"], 10)
        self.assertEqual(metrics["evaluation_return_mean"], 500.0)

    def test_rl_requires_a_matching_cuda_optimizer_receipt(self) -> None:
        manifest = load_manifest()
        collect.validate_c51_cuda_receipt(
            "cleanrl-c51-gb10", cuda_receipt("gb10"), manifest
        )
        collect.validate_c51_cuda_receipt(
            "cleanrl-c51-a100", cuda_receipt("a100"), manifest
        )
        invalid_receipts = (
            "",
            cuda_receipt("gb10").replace("weight changed: True", "weight changed: False"),
            cuda_receipt("gb10").replace("NVIDIA GB10", "NVIDIA A100-SXM4-40GB"),
            cuda_receipt("gb10") + "Traceback (most recent call last):\nboom\n",
        )
        for receipt in invalid_receipts:
            with self.subTest(receipt=receipt), self.assertRaises(ContractError):
                collect.collect_rl(
                    "cleanrl-c51-gb10",
                    rl_stdout(),
                    receipt,
                    CHECKPOINT_PAYLOAD,
                    manifest,
                )

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
                cuda_receipt("gb10"),
                CHECKPOINT_PAYLOAD,
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
                collect.collect_rl(
                    "cleanrl-c51-gb10",
                    payload,
                    cuda_receipt("gb10"),
                    CHECKPOINT_PAYLOAD,
                    manifest,
                )

    def test_rl_rejects_an_invalid_checkpoint_or_uncorrelated_sps(self) -> None:
        for checkpoint in (b"", b"x"):
            with self.subTest(checkpoint=checkpoint), self.assertRaises(ContractError):
                collect.collect_rl(
                    "cleanrl-c51-gb10",
                    rl_stdout(),
                    cuda_receipt("gb10"),
                    checkpoint,
                    load_manifest(),
                )
        with self.assertRaises(ContractError):
            collect.collect_rl(
                "cleanrl-c51-gb10",
                rl_stdout().replace("SPS: 1800", "SPS: 999999999"),
                cuda_receipt("gb10"),
                CHECKPOINT_PAYLOAD,
                load_manifest(),
            )
        with self.assertRaises(ContractError):
            collect.collect_rl(
                "cleanrl-c51-gb10",
                rl_stdout() + "\nTraceback (most recent call last):\nboom\n",
                cuda_receipt("gb10"),
                CHECKPOINT_PAYLOAD,
                load_manifest(),
            )

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
        self.assertEqual(issue_row["cuda_optimizer_step_passed"], "1")
        self.assertEqual(issue_row["evaluation_return_mean"], "")
        self.assertEqual(c51_row["status"], "")
        self.assertEqual(c51_row["total_tokens"], "")
        self.assertEqual(c51_row["training_steps"], "500000")
        self.assertEqual(c51_row["checkpoint_bytes"], str(len(CHECKPOINT_PAYLOAD)))
        self.assertEqual(c51_row["checkpoint_sha256"], CHECKPOINT_SHA256)
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
        self.assertEqual(parsed[0]["sources"], ("factory", "model", "issue"))
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
                    "final_observed_step": 1,
                    "training_sps": 1,
                    "evaluation_episodes": 1,
                    "evaluation_return_mean": 1.0,
                    "evaluation_return_stddev": 0.0,
                    "checkpoint_bytes": len(CHECKPOINT_PAYLOAD),
                    "checkpoint_sha256": CHECKPOINT_SHA256,
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
    def test_observation_schema_matches_optional_gpu_and_checkpoint_contracts(self) -> None:
        with (ROOT / "schemas/aggregate-observation.schema.json").open(
            encoding="utf-8"
        ) as stream:
            schema = json.load(stream)
        gpu = schema["$defs"]["gpu"]
        self.assertEqual(
            gpu["dependentRequired"]["gpu_memory_mean_mib"],
            ["gpu_memory_peak_mib"],
        )
        self.assertEqual(gpu["properties"]["gpu_utilization_max_pct"]["maximum"], 100)
        self.assertEqual(schema["$defs"]["factory"]["properties"]["status"], {"const": "succeeded"})
        self.assertEqual(schema["$defs"]["factory"]["properties"]["operation_count"], {"const": 4})
        self.assertEqual(schema["$defs"]["gpu"]["properties"]["sample_count"], {"const": 120})
        self.assertEqual(schema["$defs"]["gpu"]["properties"]["gpu_seconds"], {"const": 120})
        self.assertEqual(
            schema["$defs"]["gpu"]["properties"]["gpu_sample_span_seconds"],
            {"type": "number", "minimum": 115, "maximum": 150},
        )
        self.assertIn("isolated_workspace_pods", schema["$defs"]["kubernetes"]["required"])
        rl = schema["$defs"]["rl"]
        self.assertEqual(rl["properties"]["training_steps"], {"const": 500000})
        self.assertEqual(rl["properties"]["evaluation_episodes"], {"const": 10})
        self.assertIn("checkpoint_bytes", rl["required"])
        self.assertIn("checkpoint_sha256", rl["required"])

        with (ROOT / "schemas/run-manifest.schema.json").open(encoding="utf-8") as stream:
            manifest_schema = json.load(stream)
        self.assertEqual(
            manifest_schema["$defs"]["rlRun"]["properties"]["source_revision"],
            {"const": "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb"},
        )
        issue_conditions = manifest_schema["$defs"]["issueJob"]["allOf"]
        self.assertEqual(
            {condition["if"]["properties"]["id"]["const"] for condition in issue_conditions},
            {"cleanrl-488-factory", "cleanrl-562-factory"},
        )
        manifest = load_manifest()
        self.assertEqual(
            manifest_schema["$defs"]["cudaPreflight"]["const"],
            manifest["execution_profiles"]["gb10"]["cuda_preflight_command"],
        )
        self.assertEqual(
            manifest["execution_profiles"]["gb10"]["cuda_preflight_command"],
            manifest["execution_profiles"]["a100"]["cuda_preflight_command"],
        )
        self.assertEqual(
            manifest_schema["properties"]["collection"]["properties"]
            ["gpu_sample_command"]["const"],
            manifest["collection"]["gpu_sample_command"],
        )

    def test_public_factory_observations_require_terminal_success(self) -> None:
        observation = next(
            item
            for item in complete_observations(load_manifest())
            if item["source"] == "factory"
        )
        observation["metrics"]["status"] = "failed"
        with self.assertRaises(ContractError):
            validate_observation(observation)

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
                "130s",
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
            "--query-gpu=timestamp,index,name,utilization.gpu,memory.used,power.draw",
            collection["gpu_sample_command"][-1],
        )
        sampler = collection["gpu_sample_command"][-1]
        c51_process_pattern = (
            r"^([^ ]*/)?python([0-9]+([.][0-9]+)*)?[ ]+"
            r"[c]leanrl/c51[.]py([ ]|$).*--total-timesteps[ ]+500000([ ]|$)"
        )
        self.assertIn(f"pgrep -f '{c51_process_pattern}'", sampler)
        for command in (
            ".venv/bin/python cleanrl/c51.py --total-timesteps 500000",
            "/tmp/venv/bin/python3.10 cleanrl/c51.py --seed 1 --total-timesteps 500000",
        ):
            with self.subTest(command=command):
                self.assertIsNotNone(re.search(c51_process_pattern, command))
        for command in (
            "/bin/bash -lc '.venv/bin/python cleanrl/c51.py --total-timesteps 500000'",
            "pgrep -f '^([^ ]*/)?python.*[c]leanrl/c51[.]py'",
            "sh -lc monitor cleanrl/c51.py --total-timesteps 500000",
        ):
            with self.subTest(command=command):
                self.assertIsNone(re.search(c51_process_pattern, command))
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
            "timeout 130s kubectl --namespace software-factory-execution exec",
            readme,
        )
        self.assertEqual(
            readme.count(
                "pgrep -f '^([^ ]*/)?python([0-9]+([.][0-9]+)*)?[ ]+"
                "[c]leanrl/c51[.]py([ ]|$).*--total-timesteps[ ]+500000([ ]|$)'"
            ),
            2,
        )
        self.assertNotIn(
            "pgrep -f '^.*python.*[c]leanrl/c51.py.*--total-timesteps 500000'",
            readme,
        )
        self.assertIn('tee "$C51_OUT/gpu-samples.csv"', readme)
        self.assertIn(
            '--cuda-input "$CUDA_LOG"', readme
        )
        self.assertIn("GPU name: NVIDIA GB10", readme)
        self.assertIn("NVIDIA A100-SXM4-40GB", readme)
        self.assertIn("weight changed: True", readme)
        self.assertIn("bias changed: True", readme)
        self.assertIn("CUDA PREFLIGHT PASS", readme)
        self.assertEqual(
            readme.count(
                "export FACTORY_KUBERNETES_IMAGE=ghcr.io/fpolica91/software-factory@sha256:"
            ),
            2,
        )
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
        self.assertIn("factory_ppo_training_steps=TOTAL", readme)
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
                "execution_image": "docker.io/library/software-factory-gpu@sha256:a5e2582363acdbf172e1ebac1c5ba1de87a0fcb7420d655263b0477fa9861557",
                "image_reference": "software-factory-gpu:benchmark-2b88673",
                "public_digest": "sha256:a1ee9c9920eb45cbe8362b6aa1b34c34207322b52b280b0a5428315e6d6c09a1",
            },
            "a100": {
                "platform": "linux/amd64",
                "gpu_class": "NVIDIA A100",
                "gpu_name": "NVIDIA A100-SXM4-40GB",
                "node_name": "kent-ai-stuff",
                "digest": "sha256:ae8a2aac52a5fd32489e16812113092fe21c931b2167968a2fd93811c26c2c01",
                "execution_image": "ghcr.io/fpolica91/software-factory@sha256:ae8a2aac52a5fd32489e16812113092fe21c931b2167968a2fd93811c26c2c01",
                "image_reference": "ghcr.io/fpolica91/software-factory@sha256:ae8a2aac52a5fd32489e16812113092fe21c931b2167968a2fd93811c26c2c01",
                "public_digest": "sha256:ae8a2aac52a5fd32489e16812113092fe21c931b2167968a2fd93811c26c2c01",
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
            self.assertEqual(profile["resolved_image_digest"], expected["digest"])
            self.assertEqual(profile["execution_image"], expected["execution_image"])
            self.assertEqual(profile["image_reference"], expected["image_reference"])
            self.assertEqual(profile["public_image_digest"], expected["public_digest"])

        self.assertTrue(profiles["gb10"]["system_site_packages"])
        self.assertEqual(
            profiles["gb10"]["setup_commands"],
            [
                "python3.12 -m venv --system-site-packages --without-pip "
                "/tmp/factory-cleanrl-venv",
                "UV_LINK_MODE=copy uv pip install --python "
                "/tmp/factory-cleanrl-venv/bin/python --require-hashes --no-deps "
                "-r .factory/benchmark/gb10.txt",
                "ln -sfn /tmp/factory-cleanrl-venv .venv",
                "mkdir -p /tmp/factory-runs && ln -sfn /tmp/factory-runs runs",
            ],
        )
        self.assertFalse(profiles["a100"]["system_site_packages"])
        self.assertEqual(
            profiles["a100"]["setup_commands"],
            [
                "UV_CACHE_DIR=/tmp/factory-uv-cache uv python install 3.10.16",
                "UV_CACHE_DIR=/tmp/factory-uv-cache UV_PYTHON=3.10.16 "
                "UV_PROJECT_ENVIRONMENT=/tmp/factory-cleanrl-venv uv sync --frozen "
                "--extra envpool --extra pytest --no-dev",
                "ln -sfn /tmp/factory-cleanrl-venv .venv",
                "mkdir -p /tmp/factory-runs && ln -sfn /tmp/factory-runs runs",
            ],
        )
        for profile in profiles.values():
            preflight = profile["cuda_preflight_command"][2]
            compile(preflight, "<cuda-preflight>", "exec")
            for token in (
                "torch.cuda.is_available()",
                "torch.cuda.set_device(0)",
                "torch.cuda.get_device_name(0)",
                "model=torch.nn.Linear",
                "torch.nn.init.constant_(model.weight, 0.25)",
                "torch.nn.init.constant_(model.bias, 0.5)",
                "optimizer.zero_grad()",
                "loss.backward()",
                "optimizer.step()",
                "parameters()).device.type == 'cuda'",
                "weight_changed=not torch.equal(before_weight, model.weight.detach())",
                "bias_changed=not torch.equal(before_bias, model.bias.detach())",
                "assert weight_changed and bias_changed",
                "print(f'GPU name: {gpu_name}')",
                "print(f'weight changed: {weight_changed}')",
                "print(f'bias changed: {bias_changed}')",
                "print('CUDA PREFLIGHT PASS')",
            ):
                self.assertIn(token, preflight)
            self.assertTrue(preflight.endswith("print('CUDA PREFLIGHT PASS')"))
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
            "env", "PYTHONPATH=.",
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
            {run["id"]: run["checkpoint_path"] for run in manifest["rl_runs"]},
            {
                "cleanrl-c51-gb10": ".factory/benchmark/c51-gb10.cleanrl_model",
                "cleanrl-c51-a100": ".factory/benchmark/c51-a100.cleanrl_model",
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
                "GPU name: NVIDIA GB10",
            ),
            "issue-562.md": (
                "fe8d8a03c41a7ef5b523e2e354bd01c363e786bb",
                "tests/test_ppo_atari_envpool_dummy_step.py",
                "cleanrl/ppo_atari_envpool.py --env-id Breakout-v5",
                "c51-a100.log",
                "GPU name: NVIDIA A100-SXM4-40GB",
            ),
        }
        for name, tokens in expected_prompt_tokens.items():
            prompt = (ROOT / "prompts" / name).read_text(encoding="utf-8")
            normalized_prompt = " ".join(prompt.split())
            self.assertLess(len(prompt.split()), 220)
            run_id = f"cleanrl-{name.removeprefix('issue-').removesuffix('.md')}-factory"
            entry = next(item for item in manifest["issue_jobs"] if item["id"] == run_id)
            self.assertEqual(
                hashlib.sha256(prompt.rstrip("\n").encode()).hexdigest(),
                entry["reproduction_task_sha256"],
            )
            for token in (
                *tokens,
                ".factory/benchmark/",
                "--save-model",
                "factory_training_steps=500000",
                "factory_ppo_training_steps=",
                "Do not push upstream or open a pull request.",
                "Do not print, tee, or cat raw logs.",
                "optimizer.step()",
                "weight changed: True",
                "bias changed: True",
                "CUDA PREFLIGHT PASS",
            ):
                self.assertIn(" ".join(token.split()), normalized_prompt)

    def test_public_image_profile_is_a_valid_reproduction_variant(self) -> None:
        manifest = load_manifest()
        profile = manifest["execution_profiles"]["gb10"]
        public_digest = profile["public_image_digest"]
        profile["execution_image"] = f"ghcr.io/fpolica91/software-factory@{public_digest}"
        profile["image_reference"] = profile["execution_image"]
        profile["resolved_image_digest"] = public_digest
        validate_manifest(manifest)

        profile["resolved_image_digest"] = "sha256:" + "a" * 64
        with self.assertRaises(ContractError):
            validate_manifest(manifest)

        manifest = load_manifest()
        manifest["agent_profile"]["model"] = "another-model"
        with self.assertRaises(ContractError):
            validate_manifest(manifest)

    def test_header_only_metrics_do_not_emit_charts(self) -> None:
        manifest = load_manifest()
        with tempfile.TemporaryDirectory() as directory:
            metrics_path = Path(directory) / "empty-metrics.csv"
            with metrics_path.open("w", encoding="utf-8", newline="") as stream:
                csv.DictWriter(stream, fieldnames=METRIC_FIELDS).writeheader()
            with metrics_path.open(encoding="utf-8", newline="") as stream:
                rows = read_metrics_csv(stream, manifest)
            self.assertEqual(rows, [])
            output_dir = Path(directory) / "charts"
            output_dir.mkdir()
            with redirect_stderr(io.StringIO()):
                result = render_charts.main(
                    [
                        "--manifest",
                        str(ROOT / "run-manifest.json"),
                        "--metrics",
                        str(metrics_path),
                        "--output-dir",
                        str(output_dir),
                    ]
                )
            self.assertEqual(result, 2)
            self.assertEqual(list(output_dir.iterdir()), [])

    def test_issue_metric_text_uses_only_bitmap_font_glyphs(self) -> None:
        metric_text = render_charts._metric_text(
            {
                "sources": ("issue",),
                "focused_test_count": 5,
                "focused_test_seconds": 1.25,
                "ppo_configured_steps": 1000,
                "ppo_completed_steps": 999,
                "ppo_last_logged_step": 999,
                "ppo_completion_verified": 1,
                "ppo_final_sps": 321,
                "ppo_last_training_return": 1,
            }
        )
        self.assertIn("PPO 999/1000 steps | 321 SPS", metric_text)
        self.assertEqual(set(metric_text.upper()) - render_charts.FONT.keys(), set())

    def test_rendering_is_byte_deterministic(self) -> None:
        manifest = load_manifest()
        merged = collect.merge_observations(complete_observations(manifest), manifest)
        payload = io.StringIO()
        writer = csv.DictWriter(payload, fieldnames=METRIC_FIELDS, lineterminator="\n")
        writer.writeheader()
        writer.writerows(merged)
        payload.seek(0)
        rows = read_metrics_csv(payload, manifest)
        svg = render_charts.render_svg(manifest, rows)
        self.assertEqual(svg, render_charts.render_svg(manifest, rows))
        self.assertIn(b"1 responses | 0 tools | 100 tokens", svg)
        self.assertIn(b"C51 500000 AT 1800 SPS", svg)
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

    def test_committed_results_and_charts_are_an_atomic_publication(self) -> None:
        manifest = load_manifest()
        complete = all(
            entry["status"] == "completed"
            for entry in (*manifest["issue_jobs"], *manifest["rl_runs"])
        )
        svg_path = ROOT / "charts" / "benchmark-summary.svg"
        png_path = ROOT / "charts" / "benchmark-summary.png"
        if not complete:
            self.assertFalse(svg_path.exists())
            self.assertFalse(png_path.exists())
            return

        metrics_path = ROOT / "data" / "metrics.csv"
        with metrics_path.open(encoding="utf-8", newline="") as stream:
            rows = read_metrics_csv(stream, manifest)
        self.assertEqual(len(rows), 4)

        run_ids = [
            entry["id"]
            for entry in (*manifest["issue_jobs"], *manifest["rl_runs"])
        ]
        observation_paths = [
            path
            for run_id in run_ids
            for path in sorted((ROOT / "data" / run_id).glob("*.json"))
        ]
        self.assertEqual(len(observation_paths), 16)
        observations = [
            json.loads(path.read_text(encoding="utf-8"))
            for path in observation_paths
        ]
        expected_rows = collect.merge_observations(observations, manifest)
        expected_csv = io.StringIO(newline="")
        writer = csv.DictWriter(
            expected_csv,
            fieldnames=METRIC_FIELDS,
            lineterminator="\n",
        )
        writer.writeheader()
        writer.writerows(expected_rows)
        self.assertEqual(metrics_path.read_text(encoding="utf-8"), expected_csv.getvalue())
        self.assertEqual(svg_path.read_bytes(), render_charts.render_svg(manifest, rows))
        self.assertEqual(png_path.read_bytes(), render_charts.render_png(manifest, rows))

    def test_committed_c51_cuda_receipts_are_publication_valid(self) -> None:
        manifest = load_manifest()
        for run_id in ("cleanrl-c51-gb10", "cleanrl-c51-a100"):
            receipt_path = ROOT / "data" / run_id / "cuda-preflight.txt"
            self.assertTrue(receipt_path.is_file())
            collect.validate_c51_cuda_receipt(
                run_id,
                receipt_path.read_text(encoding="utf-8"),
                manifest,
            )


if __name__ == "__main__":
    unittest.main()
