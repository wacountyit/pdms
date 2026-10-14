#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
# Runs once, when the Postgres data volume is first initialized.
set -eu
read_secret() {
  value=$(cat "/run/secrets/$1") || { echo "cannot read secret $1" >&2; exit 1; }
  [ -n "$value" ] || { echo "secret $1 is empty" >&2; exit 1; }
  printf '%s' "$value"
}
owner_password=$(read_secret pg_owner_password)
app_password=$(read_secret pg_app_password)
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname postgres \
  -v owner_password="$owner_password" \
  -v app_password="$app_password" \
  -f /pdms/00_roles.sql
