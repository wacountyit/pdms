# PDMS: notes for Claude and other contributors

PDMS is the county's policy and document management system. The code, crates,
binary and Docker images are named `pdms` too. It is a Rust 2024 workspace:
- Axum serves server-rendered Askama templates with a little HTMX;
- PostgreSQL holds the data, through SQLx;
- a separate worker process runs jobs.

The README ("Layout") describes each crate.

## Start here

1. **[docs/ROADMAP.md §0](docs/ROADMAP.md#0-where-things-stand)**
   is the status and the plan of record: what is done, what is next, and
   in what order.
2. **[docs/DECISIONS.md](docs/DECISIONS.md)** is the decision log. Check it
   before changing behaviour that someone decided on purpose.
3. **Designs and plans** live in `docs/superpowers/`:
   - `specs/` holds design specs, one per piece of work. The project owner
     approves each one.
   - `plans/` holds step-by-step implementation plans.
4. **What is in progress right now:** `gh pr list`, plus these ROADMAP
   sections:
   - §0.1a: who is working on what;
   - §0.3: the status of each sub-project;
   - §0.5: what can start next;
   - §0.6: which work touches which files;
   - §0.7: questions waiting for the owner.

   Don't rely on a copy of these lists anywhere else; copies go stale.
   Two people work on PDMS in turns, so read "Working in turns" below
   before you start.
   - **SharePoint storage is built** (plans P1 to P4, done 2026-10-23). The
     live test on a real SharePoint site started 2026-10-26: Test passes and
     the first upload works. The four small setup-flow problems it found are
     fixed (ROADMAP §0.1); the live test continues (§0.5 item 1)
     ([docs/SHAREPOINT_SETUP.md](docs/SHAREPOINT_SETUP.md)). The
     [plan index](docs/superpowers/plans/2026-10-14-pdms-sharepoint-entra-index.md)
     records what each plan produced.
   - **Settings in Administration (S1) is done** (2026-10-23). Sign-in,
     email, Microsoft 365, SharePoint, the Entra sync, OCR and virus
     scanning are set up on admin pages; the installer is for the first
     installation. The settings registry is `pdms-core::settings::SETTINGS`;
     a value in the configuration file wins and shows read-only (D-111).
   - **UI redesign R2 is done** (2026-10-22). Its
     [spec](docs/superpowers/specs/2026-10-22-ui-redesign-r2-design.md) §5.2
     is the pattern every new admin page follows.

## Working in turns

Two people work on PDMS, often one after the other, each with their own
Claude session. These rules keep either one from overwriting or breaking the
other's work. The owner (GitHub `@wc-sstevens`) follows them too.

**Everyone works directly on `main`** (the owner's decision, 2026-10-23).
PDMS is not in production yet, so there are no feature branches or pull
requests: you commit to `main` and push. CI runs on every push to `main`.
Taking turns, claiming work, and pushing often are what keep the two of you
from colliding.

- **Before your first session:** you need Write access to `wacountyit/pdms`
  (ask the owner), and `gh auth login`. Commit under your own git identity;
  never copy one from an earlier plan.
- **At the start of every session:**
  1. `git checkout main && git pull`.
  2. Read ROADMAP §0.1a (who has what, and where they stopped), §0.5 (what
     can start next) and §0.7 (questions waiting for the owner).
- **Claim work before you start it:**
  1. Take the first unclaimed item in ROADMAP §0.5 whose prerequisites are
     met, or the one the owner gives you.
  2. Add your row to §0.1a, commit it on its own (`docs: claim <item>`)
     and push it straight away, so the other person sees it. If the push is
     rejected because `main` has moved, pull, read §0.1a again, and choose
     again if someone else took your item.

  Before starting work that shares a row in §0.6 with claimed work, agree
  the order with the owner.
- **Commit and push in small steps.** Each commit should leave `main`
  building and its tests passing: run the checks under "Commands" before you
  push. Start commit messages with the item (`P4:`, `R3:`). Pull before you
  push; if someone else pushed in between, `git pull --rebase`, run the
  checks again, then push. Never force-push `main`.
- **Never leave work only on your machine.** Push at the end of every
  session. If something is unfinished, push it in a state that builds and
  passes, for example with the new part not yet switched on, and say so in
  §0.1a.
- **Keep your status current.** Whenever a part of your work changes state,
  and at the end of every session, update your row in ROADMAP §0.1a, in the
  same commit as the work or on its own (`docs: status <item>`). Mark each
  part **Not started**, **In progress** or **Done**, with the date.
- **When you stop partway,** put a short "Where I stopped" note in your
  §0.1a row: what is done, the next step, the open questions, and anything
  half-done.
- **Taking over paused work:** agree it with the owner first, change "Who"
  in §0.1a, and carry on from the "Where I stopped" note. Only one person
  works on an item at a time.
- **When you finish:**
  1. Run the checks under "Commands".
  2. Update the docs in the same commit or push: the status in ROADMAP §0.3,
     §0.5 and §0.6 if they changed, and the other docs listed under "Good to
     know".
  3. Mark the part **Done** in §0.1a. When the whole item is done, add it
     to ROADMAP §0.1 and remove its row from §0.1a.
- **Questions for the owner:**
  - If a question blocks you, ask the owner straight away: through the
    person you work for, or in a GitHub issue that mentions `@wc-sstevens`.
  - If it can wait, add it to ROADMAP §0.7.
  - Don't guess at a product decision. Once you have the answer, record it
    where it belongs and remove the question from §0.7.
- **Private material stays out of the repository:** the security findings
  and the notes from earlier builds. Ask the owner for what you need.

## Rules that always apply

- **This repository is public.**
  - Never commit secrets, internal hostnames or IP addresses, a real county
    seal, or real people's data.
  - A private security review exists. **Never write vulnerability details
    into code comments, commit messages or issues.** The project owner
    holds the findings; ask them.
  - Keep commit messages neutral and product-focused.
- **Design before build.** Anything beyond a small fix gets a spec in
  `docs/superpowers/specs/`, approved by the project owner, then a plan, then
  the build, committed to `main` in small steps (ROADMAP §0.4).
- **Numbers.** Migrations (`migrations/NNNN_*.sql`) and decisions (`D-NNN`)
  take the next number after the highest one on `main`; pull first. Never
  edit a migration once it has been pushed. Migrations have no license
  header (D-080).
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
  *ahead* still serves. But `pdms migrate` from an older checkout then
  fails, because the database knows a migration that checkout doesn't. Pull
  before you migrate, and use your own dev database.
- **The test server** is upgraded by the project owner: back up first
  (`deploy/backup/pdms-backup.sh`), then `git pull && ./install.sh`. See
  ADMIN_GUIDE "Operations".
- **Keep the docs in step with the code**, in the same commit or push:
  - ROADMAP §0.3 status;
  - DECISIONS;
  - ADMIN_GUIDE, whenever an admin would do something differently;
  - README and ARCHITECTURE, when behaviour or structure changes.
- **Writing style:** docs are for county IT administrators. Use plain
  language and concrete steps, and say why a rule exists.
