#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
# PDMS restore from a pdms-backup.sh directory. See docs/BACKUP.md.
#
#   deploy/backup/pdms-restore.sh BACKUP_DIR        (run from the install directory)
#
# Replaces the current database and file store with the backup's, restores
# config/ and secrets/ when they are missing, starts PDMS, and runs
# verify-integrity. Destructive: you are asked to confirm unless --yes is given.
set -euo pipefail

cd "$(dirname "$0")/../.."
[[ -f docker-compose.yml ]] || { echo "run from the PDMS install directory" >&2; exit 1; }
src=${1:?usage: pdms-restore.sh BACKUP_DIR [--yes]}
src=$(cd "$src" && pwd)
yes=${2:-}
compose() { docker compose "$@"; }

echo "Checking the backup's checksums..."
( cd "$src" && sha256sum -c --quiet SHA256SUMS )
cat "$src/INFO"

if [[ "$yes" != "--yes" ]]; then
  read -r -p "This replaces the PDMS database and files in $(pwd). Type RESTORE to continue: " answer
  [[ "$answer" == "RESTORE" ]] || { echo "Cancelled."; exit 1; }
fi

# Configuration and secrets: restored only where missing (a rebuilt server).
if [[ ! -s secrets/pdms_secret_key || ! -f config/pdms.toml ]]; then
  echo "Restoring config/ and secrets/..."
  tar -xf "$src/config-secrets.tar"
fi

echo "Stopping the app and worker..."
compose stop app worker >/dev/null 2>&1 || true
compose up -d postgres
for _ in $(seq 1 60); do
  compose exec -T postgres pg_isready -U postgres -d postgres >/dev/null 2>&1 && break
  sleep 2
done

echo "Restoring the database..."
compose exec -T postgres psql -U postgres -d postgres -v ON_ERROR_STOP=1 <<'SQL'
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'pdms' AND pid <> pg_backend_pid();
DROP DATABASE IF EXISTS pdms;
CREATE DATABASE pdms OWNER pdms_owner ENCODING 'UTF8' TEMPLATE template0;
REVOKE ALL ON DATABASE pdms FROM PUBLIC;
GRANT CONNECT ON DATABASE pdms TO pdms_app;
SQL
compose exec -T postgres pg_restore -U postgres -d pdms --exit-on-error < "$src/pdms.dump"

if [[ -f "$src/pdmsdata.tar" ]]; then
  echo "Restoring the file store..."
  project=$(compose config --format json 2>/dev/null | sed -n 's/^ *"name": *"\([^"]*\)".*/\1/p' | head -1)
  project=${project:-pdms}
  compose create app >/dev/null 2>&1 || true
  volume=$(docker volume ls -q --filter "label=com.docker.compose.project=${project}" \
                                 --filter "label=com.docker.compose.volume=pdmsdata")
  [[ -n "$volume" ]] || { echo "cannot find the pdmsdata volume" >&2; exit 1; }
  docker run --rm -v "$volume:/data" -v "$src:/backup:ro" postgres:16-alpine sh -c \
    'find /data -mindepth 1 -delete && tar -C /data -xpf /backup/pdmsdata.tar'
fi

echo "Starting PDMS..."
compose up -d
echo "Verifying integrity (every file is re-hashed; this can take a while)..."
compose run --rm app verify-integrity
