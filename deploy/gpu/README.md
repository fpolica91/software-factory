# GPU Execution Image

This optional image gives Factory execution Pods the native Factory/Codex
runtime plus NVIDIA's PyTorch and CUDA stack. It does not replace or enlarge the
default control-plane image. The pinned NVIDIA 25.08 index contains both
`linux/amd64` (A100) and `linux/arm64` (GB10) images; its CUDA 13.0 and Python
3.12 versions are intentional because that exact base has been exercised on
the GB10 host.

## Build and Publish

Accept the NVIDIA NGC terms and authenticate to `nvcr.io` if required. Build a
single local architecture for inspection:

```sh
docker buildx build --load \
  --platform linux/amd64 \
  --file deploy/gpu/Dockerfile \
  --tag software-factory-gpu:test .
```

Use `linux/arm64` for the corresponding GB10-only build.

Publish each architecture on its native GPU host. This avoids QEMU failures in
large NVIDIA base-image package scripts. On the A100 host, push the amd64 image:

```sh
docker buildx build --push \
  --platform linux/amd64 \
  --file deploy/gpu/Dockerfile \
  --tag ghcr.io/OWNER/software-factory-gpu:25.08-amd64 .
```

On the GB10 host, push the arm64 image:

```sh
docker buildx build --push \
  --platform linux/arm64 \
  --file deploy/gpu/Dockerfile \
  --tag ghcr.io/OWNER/software-factory-gpu:25.08-arm64 .
```

Resolve both architecture-specific digests, then create and verify the public
multi-architecture index from any authenticated host:

```sh
docker buildx imagetools create \
  --tag ghcr.io/OWNER/software-factory-gpu:25.08 \
  ghcr.io/OWNER/software-factory-gpu@sha256:AMD64_DIGEST \
  ghcr.io/OWNER/software-factory-gpu@sha256:ARM64_DIGEST
docker buildx imagetools inspect ghcr.io/OWNER/software-factory-gpu:25.08
```

Emulated cross-builds remain possible, but they are not the recommended
publication path for this image.

Set only the Kubernetes execution image to the resulting immutable digest:

```dotenv
FACTORY_EXECUTION_ENVIRONMENT_BACKEND=kubernetes
FACTORY_KUBERNETES_IMAGE=ghcr.io/OWNER/software-factory-gpu@sha256:DIGEST
FACTORY_KUBERNETES_GPU_RESOURCE=nvidia.com/gpu
FACTORY_KUBERNETES_GPU_COUNT=1
```

The cluster must advertise `nvidia.com/gpu` through its NVIDIA device plugin.
Use `FACTORY_KUBERNETES_NODE_NAME` when a run must target the A100 or GB10 host.
Do not set `FACTORY_IMAGE` to this image; control-plane services should retain
the normal Factory image.

## Compatibility Boundaries

The pin is Ubuntu 24.04, Python 3.12, NVIDIA PyTorch
`2.8.0a0+34c6371`, and CUDA 13.0. It requires an R580.65.06-or-newer NVIDIA
driver. It is deliberately not upgraded with each monthly NGC release: change
the digest only after the CUDA check below passes on both target hosts. Expect
more than 10 GiB of compressed base layers per architecture. See NVIDIA's
[25.08 release notes](https://docs.nvidia.com/deeplearning/frameworks/pytorch-release-notes/rel-25-08.html)
for the upstream software contract.

## Validate on Each Architecture

Before a Factory run, test the published digest directly on each GPU host:

```sh
docker run --rm --gpus all IMAGE@sha256:DIGEST python -c \
  'import torch; assert torch.cuda.is_available(); x=torch.ones(2, device="cuda"); print(torch.__version__, torch.cuda.get_device_name(), (x @ x).item())'
```

Inside a job, keep dependencies in the repository and retain NVIDIA's PyTorch
build:

```sh
python -m venv --system-site-packages .venv
. .venv/bin/activate
python -m pip install -r requirements.txt
```

The image also pins the multi-architecture `uv` 0.9.26 binary. Projects that
require an older Python than the base image can create a workspace-local
interpreter without changing the image:

```sh
uv python install 3.10
uv venv --python 3.10 .venv
```

Factory execution Pods can run under any numeric non-root UID. `HOME`, the XDG
directories, the uv cache, and uv's managed Python installation and binary
directories point into the Pod's private `/tmp`; the active UID creates them
on demand. Do not persist those caches as job output. The `.venv` above remains
in the mounted workspace and is therefore available to later job stages.

That environment does not inherit NVIDIA's Python 3.12 PyTorch build. Install
a CUDA-enabled PyTorch wheel compatible with both the project and the host,
then repeat the CUDA assertion before training.

The base sets `/etc/pip/constraint.txt`. Packages that depend on a different
`torch` build can still replace or conflict with the CUDA-enabled build, so pin
project dependencies and verify `torch.cuda.is_available()` after installation.
CleanRL, project source, credentials, datasets, and benchmark output belong in
the mounted workspace or external stores, never in this image.
