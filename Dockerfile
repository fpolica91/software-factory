# syntax=docker/dockerfile:1.7

ARG RUST_VERSION=1.95.0
ARG NODE_VERSION=22.22.2
ARG CARGO_BUILD_JOBS=2

FROM rust:${RUST_VERSION}-bookworm AS rust-builder
ARG CARGO_BUILD_JOBS
WORKDIR /build
COPY factory-harness/codex-rs/ factory-harness/codex-rs/
COPY factory-harness/factory/ factory-harness/factory/
RUN test -f factory-harness/codex-rs/secrets/Cargo.toml
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/build/factory-harness/factory/target,sharing=locked \
    cargo build \
      --locked \
      --release \
      --manifest-path factory-harness/factory/Cargo.toml \
      --workspace \
      --bins \
      --jobs "${CARGO_BUILD_JOBS}" \
    && mkdir -p /out \
    && cp factory-harness/factory/target/release/factory-runtime /out/ \
    && cp factory-harness/factory/target/release/factoryd /out/ \
    && cp factory-harness/factory/target/release/factory-provider-bridge /out/ \
    && strip --strip-unneeded \
      /out/factory-runtime \
      /out/factoryd \
      /out/factory-provider-bridge

FROM node:${NODE_VERSION}-bookworm-slim AS node-builder
WORKDIR /build

COPY harness-client/package.json harness-client/package-lock.json harness-client/tsconfig.json harness-client/
COPY harness-client/scripts/ harness-client/scripts/
COPY harness-client/src/ harness-client/src/
COPY factory-harness/factory/protocol/schema/ factory-harness/factory/protocol/schema/
COPY factory-harness/codex-rs/app-server-protocol/schema/ factory-harness/codex-rs/app-server-protocol/schema/
RUN npm ci --prefix harness-client && npm run build --prefix harness-client

COPY integrations/package.json integrations/package-lock.json integrations/tsconfig.json integrations/
COPY integrations/src/ integrations/src/
RUN npm ci --prefix integrations && npm run build --prefix integrations

COPY workflows/package.json workflows/package-lock.json workflows/tsconfig.json workflows/
COPY workflows/src/ workflows/src/
RUN npm ci --prefix workflows \
    && npm run build --prefix workflows \
    && npm prune --omit=dev --prefix workflows

FROM node:${NODE_VERSION}-bookworm-slim AS provider-builder
WORKDIR /provider-bridge
COPY factory-harness/factory/providers/bridge/package.json \
    factory-harness/factory/providers/bridge/package-lock.json ./
RUN npm ci --omit=dev \
    && rm -rf node_modules/@oven
COPY factory-harness/factory/providers/bridge/src/ src/

FROM node:${NODE_VERSION}-bookworm-slim AS factory
LABEL org.opencontainers.image.source="https://github.com/fpolica91/software-factory" \
    org.opencontainers.image.title="Software Factory" \
    org.opencontainers.image.description="Durable autonomous software work on the native Codex harness"
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust-builder /out/factory-runtime /usr/local/bin/factory-runtime
COPY --from=rust-builder /out/factoryd /usr/local/bin/factoryd
COPY --from=rust-builder /out/factory-provider-bridge /usr/local/bin/factory-provider-bridge

COPY --from=provider-builder /provider-bridge /usr/local/lib/software-factory/provider-bridge
COPY --from=node-builder /build/harness-client/package.json /opt/software-factory/harness-client/package.json
COPY --from=node-builder /build/harness-client/dist /opt/software-factory/harness-client/dist
COPY --from=node-builder /build/workflows/package.json /opt/software-factory/workflows/package.json
COPY --from=node-builder /build/workflows/dist /opt/software-factory/workflows/dist
COPY --from=node-builder /build/workflows/node_modules /opt/software-factory/workflows/node_modules
COPY --from=node-builder /build/integrations/package.json /opt/software-factory/integrations/package.json
COPY --from=node-builder /build/integrations/dist /opt/software-factory/integrations/dist
COPY apps/cli/factory-worker-entrypoint.sh /usr/local/bin/factory-worker-entrypoint

RUN chmod 0755 /usr/local/bin/factory-worker-entrypoint \
    && chmod 0644 \
      /opt/software-factory/harness-client/package.json \
      /opt/software-factory/integrations/package.json \
      /opt/software-factory/workflows/package.json \
      /usr/local/lib/software-factory/provider-bridge/package.json \
      /usr/local/lib/software-factory/provider-bridge/package-lock.json \
    && chmod -R a+rX /usr/local/lib/software-factory/provider-bridge/src

RUN mkdir -p \
    /var/lib/software-factory/codex \
    /var/lib/software-factory/coordinator \
    /var/lib/software-factory/provider \
    /workspaces

ENV FACTORY_RUNTIME_PATH=/usr/local/bin/factory-runtime \
    FACTORY_PROVIDER_RESOURCE_DIR=/usr/local/lib/software-factory/provider-bridge \
    FACTORY_CODEX_HOME=/var/lib/software-factory/codex \
    FACTORY_WORKSPACE_ROOT=/workspaces \
    CODEX_ANALYTICS_ENABLED=false \
    DO_NOT_TRACK=1 \
    NEXT_TELEMETRY_DISABLED=1 \
    OTEL_SDK_DISABLED=true

WORKDIR /opt/software-factory/workflows
CMD ["factoryd", "--help"]
