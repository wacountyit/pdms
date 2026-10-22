# PADM: notes for Claude and other contributors

PADM is the county's policy and document management system. The code, crates,
binary and Docker images are still named `pdms`. It is a Rust 2024 workspace:
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
   Two people work on PADM in turns, so read "Working in turns" below
   before you start.
   - **Working on the UI redesign, round 2 (R2)?** Start with the
     [R2 brief](docs/superpowers/specs/2026-10-21-ui-redesign-r2-brief.md).
   - **Working on SharePoint storage (plans P3, P4)?** Start with the
     [plan index](docs/superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md),
     which holds their shared contracts.

## Working in turns

Two people work on PADM, often one after the other, each with their own
Claude session. These rules keep either one from overwriting or breaking the
other's work. The owner (GitHub `@wc-sstevens`) follows them too.

- **Before your first session:** you need Write access to `wacountyit/pdms`
  (ask the owner), and `gh auth login`. Commit under your own git identity;
  never copy one from an earlier plan.
- **At the start of every session:**
  1. `git checkout main && git pull`.
  2. Read ROADMAP §0.1a (who has what), §0.5 (what can start next) and §0.7
     (questions waiting for the owner).
  3. Run `gh pr list`. Draft PRs are work in flight.
  4. If you are resuming your own branch, read the "Where I stopped" note in
     its PR, rebase the branch on `main`, and push it with
     `git push --force-with-lease`.
- **Claim work before you start it:**
  1. Take the first unclaimed item in ROADMAP §0.5 whose prerequisites are
     met, or the one the owner gives you.
  2. Add your row to §0.1a and push that one change straight to `main`
     (`docs: claim <item>`). If the push is rejected because `main` has
     moved, pull, read §0.1a again, and choose again if someone else took
     your item. Status changes (claims and states in §0.1a, and moving a
     finished item to §0.1) and questions added to §0.7 are the only
     changes pushed to `main` without a PR, and a PR never edits §0.1a.
  3. Create your branch and open a **draft** PR straight away. Name the
     branch after the item (`r2-admin-pages`, `p3-sharepoint-storage`), and
     start the PR title with it (`R2:`, `P3:`, `SP3:`).

  Before starting work that shares a row in §0.6 with claimed work, agree
  the order with the owner.

  **Work you did before claiming it**, or that is still only on your
  machine: claim it now with its real state, move it onto a branch rebased
  on `main`, push it, and open the draft PR.
- **Never leave work only on your machine.** Push your branch at the end of
  every session, even when the work is unfinished or failing.
- **Keep your status current.** Whenever a part of your work changes state,
  and at the end of every session, update your row in ROADMAP §0.1a and
  push it straight to `main` (`docs: status <item>`). Mark each part
  **Not started**, **In progress**, **In review** or **Done**, with its PR
  number and the date. This is how the other person sees what is being
  done. If the push is rejected, pull and push again.
- **When you stop partway,** fill in "Where I stopped" in the PR description
  (the PR template has the heading). Say what is done, what the next step
  is, the open questions, and anything half-done or failing. Update your
  §0.1a row too.
- **Taking over paused work:** agree it with the owner first. Then work on
  the same branch, change "Who" in §0.1a, and add a line under "Where I
  stopped". Only one person works on a branch at a time.
- **When you finish:**
  1. Rebase on `main` and push with `git push --force-with-lease`. That is
     expected on your own PR branch; never force-push `main` or someone
     else's branch. If `main` has taken a migration or decision number you
     were using, renumber yours.
  2. Run the checks under "Commands".
  3. Update the docs in the same PR: the status in ROADMAP §0.3, §0.5 and
     §0.6 if they changed, and the other docs listed under "Good to know".
  4. Mark the PR ready for review, and mark that part **In review** in
     §0.1a. The owner merges it.
  5. After the merge, delete the branch on GitHub and locally, so only
     `main` and work in progress remain. Mark the part **Done** in §0.1a.
     When the whole item is done, add it to ROADMAP §0.1 and remove its
     row from §0.1a.
- **Questions for the owner:**
  - If a question blocks you, ask the owner straight away: through the
    person you work for, or in a comment on your PR that mentions
    `@wc-sstevens` (`gh pr comment`).
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
