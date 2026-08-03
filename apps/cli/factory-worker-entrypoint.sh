#!/bin/sh
set -eu

factory_uid="${FACTORY_RUN_AS_UID:-}"
factory_gid="${FACTORY_RUN_AS_GID:-}"

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
  chown -R "$factory_uid:$factory_gid" /var/lib/software-factory/codex
  if [ "${FACTORY_PROVIDER_STATE_WRITABLE:-}" = 1 ]; then
    chown -R "$factory_uid:$factory_gid" /var/lib/software-factory/provider
  fi
  chown -R "$factory_uid:$factory_gid" /workspaces
  chown "$factory_uid:$factory_gid" \
    /factory-artifacts \
    /factory-artifacts/local \
    /factory-artifacts/local/jobs \
    /factory-artifacts/coordinator \
    /factory-artifacts/coordinator/jobs
  export HOME=/var/lib/software-factory/codex
  export XDG_CACHE_HOME=/var/lib/software-factory/codex/.cache
  exec setpriv --reuid "$factory_uid" --regid "$factory_gid" --clear-groups "$@"
fi

exec "$@"
