# syntax=docker/dockerfile:1.7

# Both inputs are immutable multi-architecture indexes. Override either input
# only with another repository@sha256:<digest> reference.
ARG FACTORY_BASE_IMAGE=ghcr.io/fpolica91/software-factory@sha256:ba18675ad5972cf4d2fd833128bb6ff471c76c5ab96f073aff9415ce98d0e39b
ARG NVIDIA_PYTORCH_BASE_IMAGE=nvcr.io/nvidia/pytorch@sha256:ace9a848c0ae543317e3c4763b6b4248961c47902625abfe3c77a0fb931c50fb
ARG UV_BASE_IMAGE=ghcr.io/astral-sh/uv@sha256:9a23023be68b2ed09750ae636228e903a54a05ea56ed03a934d00fe9fbeded4b

FROM ${FACTORY_BASE_IMAGE} AS factory-runtime
FROM ${UV_BASE_IMAGE} AS uv-runtime
FROM ${NVIDIA_PYTORCH_BASE_IMAGE} AS factory-gpu

LABEL org.opencontainers.image.source="https://github.com/fpolica91/software-factory" \
    org.opencontainers.image.title="Software Factory GPU execution image" \
    org.opencontainers.image.description="Optional NVIDIA PyTorch execution image for Factory jobs"

# Keep the runtime addition small. PyTorch, CUDA, compilers, and Python are
# supplied by the pinned NVIDIA image; venv provides repo-local isolation.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        python3-venv \
        util-linux \
    && rm -rf /var/lib/apt/lists/*

COPY --from=factory-runtime /usr/local/bin/factory /usr/local/bin/factory
COPY --from=factory-runtime /usr/local/bin/factory-worker /usr/local/bin/factory-worker
COPY --from=factory-runtime /usr/local/bin/factoryd /usr/local/bin/factoryd
COPY --from=factory-runtime /usr/local/bin/factory-provider-bridge /usr/local/bin/factory-provider-bridge
COPY --from=factory-runtime /usr/local/bin/codex /usr/local/bin/codex
COPY --from=factory-runtime /usr/local/bin/factory-worker-entrypoint /usr/local/bin/factory-worker-entrypoint
COPY --from=uv-runtime /uv /uvx /usr/local/bin/

RUN command -v git >/dev/null \
    && command -v curl >/dev/null \
    && command -v uv >/dev/null \
    && command -v setpriv >/dev/null \
    && python -m venv --help >/dev/null \
    && factory --help >/dev/null \
    && codex --version >/dev/null \
    && mkdir -p \
        /var/lib/software-factory/codex \
        /var/lib/software-factory/coordinator \
        /var/lib/software-factory/provider \
        /factory-artifacts/local/jobs \
        /factory-artifacts/coordinator/jobs \
        /workspace/project \
        /workspaces

ENV CODEX_HOME=/var/lib/software-factory/codex \
    FACTORY_WORKSPACE_ROOT=/workspaces \
    FACTORY_ARTIFACT_ROOT=/factory-artifacts

WORKDIR /workspace/project
CMD ["factory", "--help"]
