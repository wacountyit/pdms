#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
# PDMS backup: a consistent copy of the database, the file store, and the
# configuration and secrets, with checksums. See docs/BACKUP.md.
#
#   deploy/backup/pdms-backup.sh [DESTINATION]     (run from the install directory)
#
# DESTINATION defaults to ./backups/<UTC timestamp>. Point your backup system
# (for example a backup product's pre-script or a scheduled job) at the directory this
# creates. The system keeps running; nothing is stopped.
#
# Order matters: the database is dumped first, then the file store is copied.
# Stored files are never modified or deleted by PDMS, so every file the dump
# refers to is already on disk when the copy starts.
set -euo pipefail

cd "$(dirname "$0")/../.."
[[ -f docker-compose.yml ]] || { echo "run from the PDMS install directory" >&2; exit 1; }
stamp=$(date -u +%Y%m%dT%H%M%SZ)
dest=${1:-backups/$stamp}
mkdir -p "$dest"
dest=$(cd "$dest" && pwd)
umask 077
compose() { docker compose "$@"; }
project=$(compose config --format json 2>/dev/null | sed -n 's/^ *"name": *"\([^"]*\)".*/\1/p' | head -1)
project=${project:-pdms}

echo "PDMS backup -> $dest"

# 1. Database (custom format: compressed, restorable table by table).
echo "  database..."
compose exec -T postgres pg_dump -U postgres -d pdms -Fc --no-password > "$dest/pdms.dump"
compose exec -T postgres pg_dumpall -U postgres --roles-only --no-password > "$dest/roles.sql"
compose exec -T postgres psql -U postgres -d pdms -At -c \
  "SELECT 'schema_version=' || max(version) FROM _sqlx_migrations;
   SELECT 'audit_last_seq=' || coalesce(max(seq), 0) FROM audit_log;
   SELECT 'files=' || count(*) || ' bytes=' || coalesce(sum(size_bytes), 0) FROM blobs;" > "$dest/INFO"

# 2. File store (local backend). With the S3 backend, protect the bucket with
#    versioning and replication instead (see docs/BACKUP.md).
backend=$(sed -n 's/^ *backend *= *"\(.*\)".*/\1/p' config/pdms.toml | head -1)
if [[ "${backend:-local}" == "local" ]]; then
  echo "  file store..."
  volume=$(docker volume ls -q --filter "label=com.docker.compose.project=$project" \
                                 --filter "label=com.docker.compose.volume=pdmsdata")
  [[ -n "$volume" ]] || { echo "cannot find the pdmsdata volume" >&2; exit 1; }
  docker run --rm -v "$volume:/data:ro" -v "$dest:/backup" postgres:16-alpine sh -c \
    "tar -C /data -cf /backup/pdmsdata.tar --exclude=./spool . && chown $(id -u):$(id -g) /backup/pdmsdata.tar && chmod 600 /backup/pdmsdata.tar"
else
  echo "  file store: backend is $backend; back up the bucket separately"
fi

# 3. Configuration, secrets, .env, and any compose override (sensitive: store encrypted).
echo "  configuration and secrets..."
keep=(config secrets)
for f in .env docker-compose.override.yml; do [[ -f $f ]] && keep+=("$f"); done
tar -cf "$dest/config-secrets.tar" "${keep[@]}"

# 4. Checksums.
files=(pdms.dump roles.sql INFO config-secrets.tar)
[[ -f "$dest/pdmsdata.tar" ]] && files+=(pdmsdata.tar)
( cd "$dest" && sha256sum "${files[@]}" > SHA256SUMS )
echo "  $(tr '\n' ' ' < "$dest/INFO")"
echo "Done. Verify with: (cd $dest && sha256sum -c SHA256SUMS)"
