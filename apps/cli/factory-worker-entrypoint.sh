#!/bin/sh
set -eu

factory_uid="${FACTORY_RUN_AS_UID:-}"
factory_gid="${FACTORY_RUN_AS_GID:-}"
factory_workspace_ownership_mode="${FACTORY_WORKSPACE_OWNERSHIP_MODE:-manage}"
factory_codex_home="${CODEX_HOME:-/var/lib/software-factory/codex}"
factory_kubeconfig_source=/run/factory/k3s.yaml
factory_kubeconfig_target="$factory_codex_home/kubeconfig/k3s.yaml"

case "$factory_workspace_ownership_mode" in
  manage | preserve) ;;
  *) echo 'FACTORY_WORKSPACE_OWNERSHIP_MODE must be manage or preserve' >&2; exit 2 ;;
esac

if [ -z "$factory_uid" ] && [ -z "$factory_gid" ]; then
  exec "$@"
fi
if [ -z "$factory_uid" ] || [ -z "$factory_gid" ]; then
  echo 'FACTORY_RUN_AS_UID and FACTORY_RUN_AS_GID must be set together' >&2
  exit 2
fi
case "$factory_uid:$factory_gid" in
  *[!0-9:]* | :* | *:) echo 'Factory worker UID/GID must be numeric' >&2; exit 2 ;;
esac

if [ "$(id -u)" -eq 0 ]; then
  mkdir -p \
    /var/lib/software-factory/codex \
    /var/lib/software-factory/provider \
    /workspaces \
    /factory-artifacts/local/jobs \
    /factory-artifacts/coordinator/jobs
  if [ -f "$factory_kubeconfig_source" ]; then
    mkdir -p "$factory_codex_home/kubeconfig"
    cp "$factory_kubeconfig_source" "$factory_kubeconfig_target"
    chmod 600 "$factory_kubeconfig_target"
    chown "$factory_uid:$factory_gid" \
      "$factory_codex_home/kubeconfig" \
      "$factory_kubeconfig_target"
    export KUBECONFIG=$factory_kubeconfig_target
  fi
  chown -R "$factory_uid:$factory_gid" /var/lib/software-factory/codex
  if [ "${FACTORY_PROVIDER_STATE_WRITABLE:-}" = 1 ]; then
    chown -R "$factory_uid:$factory_gid" /var/lib/software-factory/provider
  fi
  if [ "$factory_workspace_ownership_mode" = manage ]; then
    chown -R "$factory_uid:$factory_gid" /workspaces
  fi
  chown "$factory_uid:$factory_gid" \
    /factory-artifacts \
    /factory-artifacts/local \
    /factory-artifacts/local/jobs \
    /factory-artifacts/coordinator \
    /factory-artifacts/coordinator/jobs
  export HOME=/var/lib/software-factory/codex
  export XDG_CACHE_HOME=/var/lib/software-factory/codex/.cache
  if [ -S /var/run/docker.sock ]; then
    docker_gid=$(stat -c '%g' /var/run/docker.sock)
    case "$docker_gid" in
      *[!0-9]* | '') echo 'Docker socket GID must be numeric' >&2; exit 2 ;;
    esac
    if [ "$docker_gid" = "$factory_gid" ]; then
      exec setpriv --reuid "$factory_uid" --regid "$factory_gid" --clear-groups "$@"
    fi
    exec setpriv --reuid "$factory_uid" --regid "$factory_gid" --groups "$docker_gid" "$@"
  fi
  exec setpriv --reuid "$factory_uid" --regid "$factory_gid" --clear-groups "$@"
fi

exec "$@"
