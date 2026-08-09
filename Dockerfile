# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.95.0
ARG CARGO_BUILD_JOBS=2

FROM rust:${RUST_VERSION}-bookworm AS rust-builder
ARG CARGO_BUILD_JOBS
WORKDIR /build
COPY factory-harness/codex-rs/ factory-harness/codex-rs/
COPY factory-harness/factory/ factory-harness/factory/
RUN test -f factory-harness/codex-rs/secrets/Cargo.toml
# Codex crates embed non-Rust assets, so hash every vendored build input. Only
# refresh their mtimes when that content changes; Factory-only edits keep the
# upstream Cargo cache reusable.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/factory-harness/factory/target,sharing=locked \
    codex_input_list=/tmp/codex-rs-inputs.list \
    && codex_input_manifest=/tmp/codex-rs-inputs.sha256 \
    && codex_cache_stamp=/build/factory-harness/factory/target/.codex-rs-inputs.sha256 \
    && find factory-harness/codex-rs -path '*/target' -prune -o -type f -print0 \
      | LC_ALL=C sort -z > "$codex_input_list" \
    && xargs -0 -r sha256sum < "$codex_input_list" > "$codex_input_manifest" \
    && find factory-harness/codex-rs -path '*/target' -prune -o \
      -type l -printf 'symlink\t%p\t%l\n' \
      | LC_ALL=C sort >> "$codex_input_manifest" \
    && codex_input_digest="$(sha256sum "$codex_input_manifest" | cut -d ' ' -f 1)" \
    && if ! grep -Fqx -- "$codex_input_digest" "$codex_cache_stamp" 2>/dev/null; then \
      xargs -0 -r touch < "$codex_input_list"; \
    fi \
    && find factory-harness/factory -path '*/target' -prune -o -type f -name '*.rs' -exec touch {} + \
    && cargo build \
      --locked \
      --release \
      --manifest-path factory-harness/factory/Cargo.toml \
      --workspace \
      --bin factory \
      --bin factory-worker \
      --bin factoryd \
      --bin factory-provider-bridge \
      --jobs "${CARGO_BUILD_JOBS}" \
    && cargo build \
      --locked \
      --release \
      --manifest-path factory-harness/codex-rs/Cargo.toml \
      --package codex-cli \
      --bin codex \
      --target-dir /build/factory-harness/factory/target \
      --jobs "${CARGO_BUILD_JOBS}" \
    && printf '%s\n' "$codex_input_digest" > "$codex_cache_stamp" \
    && mkdir -p /out \
    && cp factory-harness/factory/target/release/factory /out/ \
    && cp factory-harness/factory/target/release/factory-worker /out/ \
    && cp factory-harness/factory/target/release/factoryd /out/ \
    && cp factory-harness/factory/target/release/factory-provider-bridge /out/ \
    && cp factory-harness/factory/target/release/codex /out/ \
    && strip --strip-unneeded \
      /out/factory \
      /out/factory-worker \
      /out/factoryd \
      /out/factory-provider-bridge \
      /out/codex

FROM debian:bookworm-slim AS factory
LABEL org.opencontainers.image.source="https://github.com/fpolica91/software-factory" \
    org.opencontainers.image.title="Software Factory" \
    org.opencontainers.image.description="Durable autonomous software work on the native Codex harness"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git util-linux \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust-builder /out/factory /usr/local/bin/factory
COPY --from=rust-builder /out/factory-worker /usr/local/bin/factory-worker
COPY --from=rust-builder /out/factoryd /usr/local/bin/factoryd
COPY --from=rust-builder /out/factory-provider-bridge /usr/local/bin/factory-provider-bridge
COPY --from=rust-builder /out/codex /usr/local/bin/codex

COPY apps/cli/factory-worker-entrypoint.sh /usr/local/bin/factory-worker-entrypoint

RUN chmod 0755 /usr/local/bin/factory-worker-entrypoint

RUN mkdir -p \
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
