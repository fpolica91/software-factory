# Software Factory

Run the native Codex harness as a durable, autonomous software delivery job.

Software Factory is a CLI-first system for long-running repository work. Give
it a goal such as implementing a feature, reviewing a change, or auditing a
codebase. It plans the work, executes it in a managed Git worktree, reviews the
result, remediates findings, and preserves progress across detaches and worker
restarts.

## What Factory Does

- Runs plan, execute, independent review, remediation, and fresh re-review as
  one durable lifecycle.
- Stores jobs, events, checkpoints, retries, leases, and managed worktrees.
- Pins the selected provider and model to each job.
- Supports detach, attach, stop, continuation, result inspection, patch export,
  and safe application to a local checkout.
- Adds repository-scoped long-term memory without replacing Codex context
  compaction or its model and tool loop.

The architecture is one distribution with two lifecycles and no duplicated
harness:

1. **Codex is the execution kernel.** It owns the agent loop, tools, threads,
   planning primitives, context compaction, approvals, and subagents.
2. **Factory is a native extension.** It contributes decomposition, progress,
   memory, review, and remediation behavior through Rust extension APIs.
3. **`factoryd` provides durability.** It coordinates jobs, checkpoints,
   retries, recovery, scheduling, events, and worktrees outside the Codex
   thread lifecycle.

## Quick Start

Normal pulled-image use requires Docker Engine with Compose and Git. It does
not require Node, Rust, or a separately installed database on the host.

```sh
git clone https://github.com/fpolica91/software-factory.git
cd software-factory
./factory install
cd /path/to/your/project
factory
```

The first run guides you through selecting OpenAI, Anthropic, DeepSeek, or
Z.AI, choosing a model, entering its hidden API key, choosing an endpoint where
applicable, and describing the task. Factory then completes required service
startup, submits the durable job, prepares its managed worktree, and streams
durable progress.

Jobs are autonomous after submission. `Ctrl-C` detaches from the output without
stopping the job. Use `factory run --detach "task"` to submit and return
immediately, then reconnect with the printed job ID.

See [Installation](INSTALLATION.md) for prerequisites, configuration locations,
provider setup, and troubleshooting.

## Everyday Commands

| Command | Purpose |
| --- | --- |
| `factory` | Onboard if needed, prompt for a task, and run it |
| `factory run "task"` | Submit a task and attach to its progress |
| `factory run --detach "task"` | Submit a task and return immediately |
| `factory run --no-apply "task"` | Keep the result for explicit apply or export |
| `factory status JOB_ID` | Show durable job and stage state |
| `factory attach JOB_ID` | Reconnect to compact live output |
| `factory attach --verbose JOB_ID` | Replay complete durable event detail |
| `factory continue JOB_ID "feedback"` | Reopen a succeeded job with follow-up work |
| `factory result JOB_ID` | Print the complete durable result |
| `factory artifacts JOB_ID` | List the job's readable artifacts |
| `factory stop JOB_ID` | Request durable cancellation |
| `factory apply JOB_ID` | Apply a completed result to this clean checkout |
| `factory export JOB_ID -o result.patch` | Export a portable Git patch |
| `factory configure` | Run guided provider and model configuration |
| `factory provider list` | List available providers |
| `factory model list` | List models for the active provider |
| `factory logs` | Follow coordinator, worker, and provider logs |
| `factory up` / `factory down` | Start or stop services while preserving data |

Interactive output uses a compact expandable view. For automation, `--detach`
returns after job creation and `--json` emits newline-delimited JSON. Use
`--verbose` when complete human-readable event replay is needed.

## Results and Continuations

A successful attached run against a local repository applies its result to the
originating checkout by default. Factory verifies that the checkout is clean
and still matches the job before changing it. Use `--no-apply` to retain the
result for `factory apply` or `factory export` instead.

Local jobs publish readable reports under `.factory/jobs/JOB_ID/`, outside the
managed worktree and result patch. `factory result JOB_ID` remains the durable
source for jobs created from another checkout or a remote Git URL.

`factory continue JOB_ID "feedback"` appends another iterate, review, and
remediation round to the same durable job, worktree, and parent Codex thread.

## Providers

Provider choice changes model transport, not the agent harness.

| Provider | Transport |
| --- | --- |
| OpenAI | Direct Responses API through Codex |
| Anthropic | Rust transport adapter for Messages |
| DeepSeek | Rust transport adapter for Chat Completions |
| Z.AI | Rust transport adapter for Chat Completions |

Use `factory configure` for guided onboarding, `factory provider` to switch
providers, `factory model` to switch models, and `factory configure --show` to
inspect active settings without displaying key values. Provider-specific notes
are available for [DeepSeek](docs/providers/deepseek.md) and
[Z.AI](docs/providers/zai-glm-5.2.md).

## Runtime Profiles

Docker is the default execution backend. The baseline stack contains
PostgreSQL for durable lifecycle state, Qdrant for long-term memory,
`factoryd`, and `factory-worker`. A selected non-Responses provider starts only
its Rust transport adapter.

Linux operators may optionally schedule per-job execution Pods through
Kubernetes while keeping the durable control plane in Compose. Shared
`ReadWriteMany` storage enables multi-node scheduling. Kata can be selected as
an operator-installed RuntimeClass on nodes that provide it; Factory does not
bundle or install Kata. See [Installation](INSTALLATION.md) and the
[Kubernetes execution decision](docs/adr/0003-single-host-k3s-execution-profile.md)
before selecting this profile.

## Documentation

- [Installation and operation](INSTALLATION.md)
- [Contributing](CONTRIBUTING.md)
- [Product contract](PRODUCT.md)
- [Codex kernel and Factory lifecycle decision](docs/adr/0001-codex-kernel-factory-extension.md)
- [Remote execution environment decision](docs/adr/0002-codex-remote-execution-environments.md)
- [Optional Kubernetes execution decision](docs/adr/0003-single-host-k3s-execution-profile.md)
- [Planned CleanRL GPU evidence](benchmarks/cleanrl-gpu/README.md) (reproducible scaffold; no results yet)

Factory-owned Rust lives under `factory-harness/factory/`. The preserved Codex
kernel remains upstream-shaped under `factory-harness/codex-rs/`. Contributors
should read [CONTRIBUTING.md](CONTRIBUTING.md) before changing either boundary.
