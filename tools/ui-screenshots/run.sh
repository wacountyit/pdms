#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
#
# Runs shoot.py in Microsoft's Playwright image against a local PADM instance.
# The image ships the browsers but not the Python package, so the package is
# installed once into .pylib/ next to this script (git-ignored). Runs as your
# user, so out/ and .pylib/ stay yours. Arguments are passed to shoot.py as
# page-name filters. See README.md.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
image=mcr.microsoft.com/playwright/python:v1.49.0-jammy

[[ -f "$here/ids.json" ]] || { echo "missing $here/ids.json (see README.md, step 2)" >&2; exit 2; }

docker run --rm --network host \
  --user "$(id -u):$(id -g)" -e HOME=/tmp \
  -v "$here:/w" -e PYTHONPATH=/w/.pylib \
  -e PADM_SHOTS_BASE -e PADM_SHOTS_USER -e PADM_SHOTS_PASSWORD -e PADM_SHOTS_THEME \
  "$image" sh -c '
    [ -d /w/.pylib/playwright ] || pip install --quiet --no-cache-dir --target /w/.pylib playwright==1.49.0
    exec python /w/shoot.py "$@"' sh "$@"
