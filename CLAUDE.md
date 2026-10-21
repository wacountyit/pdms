# PADM: notes for Claude and other contributors

PADM is the county's policy and document management system. The code, crates,
binary and Docker images are still named `pdms`. It is a Rust 2024 workspace:
- Axum serves server-rendered Askama templates with a little HTMX;
- PostgreSQL holds the data, through SQLx;
- a separate worker process runs jobs.

The README ("Layout") describes each crate.

## Start here

1. **[docs/ROADMAP.md §0](docs/ROADMAP.md#0-where-things-stand-updated-2026-10-21)**
   is the status and the plan of record: what is done, what is next, and
   in what order.
2. **[docs/DECISIONS.md](docs/DECISIONS.md)** is the decision log. Check it
   before changing behaviour that someone decided on purpose.
3. **Designs and plans** live in `docs/superpowers/`:
   - `specs/` holds design specs, one per piece of work. The project owner
     approves each one.
   - `plans/` holds step-by-step implementation plans.
4. **What is in progress right now:** ROADMAP §0.1a and §0.3, plus
   `gh pr list`. Don't rely on a copy of that list anywhere else; it goes
   stale.
   - **Working on the UI redesign, round 2 (R2)?** Start with the
     [R2 brief](docs/superpowers/specs/2026-10-21-ui-redesign-r2-brief.md).
   - **Working on SharePoint storage (plans P3, P4)?** Start with the
     [plan index](docs/superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md),
     which holds their shared contracts.

## Rules that always apply

- **This repository is public.**
  - Never commit secrets, internal hostnames or IP addresses, a real county
    seal, or real people's data.
  - A private security review exists. **Never write vulnerability details
    into code comments, commit messages, PRs or issues.** The project owner
    holds the findings; ask them.
  - Keep commit messages neutral and product-focused.
- **Design before build.** Anything beyond a small fix gets a spec in
  `docs/superpowers/specs/`, approved by the project owner, then a plan, then
  a build on a branch with a PR to `main` (ROADMAP §0.4). The owner merges.
- **Numbers are shared across branches.** Migrations (`migrations/NNNN_*.sql`)
  and decisions (`D-NNN`) take the next number after the highest one on
  `main` **and** in open PRs. Never edit a migration once it is on `main`.
  Migrations have no license header (D-080).
- **License headers.** Every other new source file starts with
  `SPDX-License-Identifier: AGPL-3.0-or-later` and
  `Copyright (C) 2026 PDMS contributors`, as comments in the file's syntax.
  Test files put those two lines first, then the `#![allow(...)]` block the
  other tests use.
- **No panics in app code.** `unwrap`, `expect`, `panic` and `dbg` are denied
  outside tests (workspace lints).
- **Web pages:**
  - **Content Security Policy:** no `style="…"` attributes, no inline
    `<script>`, no `on…=` handlers, and no CDNs or remote fonts.
    `crates/pdms-web/tests/csp_guard.rs` checks templates for the first
    three. The CSP header itself blocks the rest in the browser.
  - **Accessibility:** WCAG 2.1 AA, and one `<h1>` per page.
  - **Look:** the visual system is the UI redesign's (R1 spec §4: tokens in
    `static/app.css`, the accent from `/theme.css`). Don't hard-code colors.
- **Permissions and audit:**
  - Everything shown goes through the existing visibility and grant rules
    (D-085, D-090), and nothing widens access.
  - Every change of state writes its audit event in the same database
    transaction as the change.
- **SQLx offline metadata.** After changing any `sqlx::query!` or migration,
  run `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/`.
  CI builds offline and fails if it is stale.

## Commands

- **Setup:** README "Develop" covers the dev database container, environment
  variables, `migrate`, `seed-demo` and `serve --with-worker`. The demo
  accounts and their password are listed there too.
- **Tests:** README "Tests". The fastest local setup reuses the dev database
  server through `PDMS_TEST_DATABASE_URL`, and each test still gets a fresh
  database.
- **SQLx CLI**, needed for migrations and the offline metadata:
  `cargo install sqlx-cli --no-default-features --features postgres,rustls --locked`.
  - **Compile-time query checks:** set
    `DATABASE_URL=postgres://pdms_owner:devowner@127.0.0.1:55432/pdms`, with
    the dev database migrated.
  - **Without a database:** set `SQLX_OFFLINE=true`.
- **Before handing over a change**, run:

  ```sh
  cargo fmt --all
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace
  cargo sqlx prepare --workspace --check -- --all-targets
  ```

  `cargo sqlx prepare --check` is needed only if queries or migrations
  changed.
- **UI screenshots** of every page, with layout checks:
  [tools/ui-screenshots/README.md](tools/ui-screenshots/README.md).

## Good to know

- **The app checks migrations at startup.** It refuses to start if the binary
  has migrations the database lacks; run `pdms migrate`. A database that is
  *ahead* still serves. But `pdms migrate` from an older branch then fails,
  because the database knows a migration that branch doesn't. Use your own dev
  database, or recreate it when you switch between branches with different
  migrations.
- **The test server** is upgraded by the project owner: back up first
  (`deploy/backup/pdms-backup.sh`), then `git pull && ./install.sh`. See
  ADMIN_GUIDE "Operations".
- **Keep the docs in step with the code**, in the same branch:
  - ROADMAP §0.3 status;
  - DECISIONS;
  - ADMIN_GUIDE, whenever an admin would do something differently;
  - README and ARCHITECTURE, when behaviour or structure changes.
- **Writing style:** docs are for county IT administrators. Use plain
  language and concrete steps, and say why a rule exists.
