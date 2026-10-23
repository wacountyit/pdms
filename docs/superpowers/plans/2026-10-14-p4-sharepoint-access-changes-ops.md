# P4: SharePoint folder access, change detection, and operations. Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Pre-flight done (2026-10-23, WACountyIT).** This plan was written on 2026-10-15, before P2, P3, R1 and R2 were built. It was checked against `main` at `2915b94`. Most of what it consumes exists as assumed (`GraphError::Auth`, `actions::send`, `Notice`, `sync_cursors`, the mock Graph anchors in `sp.rs`, the directory functions, `Worker::handle`/`every`, `documents::access::can`). Everyone works directly on `main` now: ignore any branch or pull-request wording, and start commit messages with `P4:`. These corrections apply; the text below already carries the numeric ones.
> - **Migration `0014`** (`0014_sharepoint_access.sql`; "Applied 14/…"). `main` ends at `0013_sharepoint_files.sql` (P3) and P2's is `0010_entra_sync.sql`.
> - **Decisions D-104 to D-110** (the plan's D-091 to D-097, in the same order). `main` ends at D-103.
> - **All tasks, test file headers:** put the two SPDX lines **first**, then the `#![allow(` block (CLAUDE.md; P3's `tests/sharepoint.rs`).
> - **Task 1:** don't add a new `ENTRA_CURSOR` constant; make `entra_sync::CURSOR` (`"entra.users"`) `pub(crate)` and reuse it.
> - **Task 2:** don't redefine `certificate()` or `sp_core`; import `mock_graph::certificate` and copy P3's `core_with`/`sp_core` pattern (or move them to a shared helper).
> - **Task 3:** in `directory/sync.rs` there are no `before`/`after` sets: queue the folder-access run when `want_depts != have_depts` (the early return at `sync.rs:72`). For the Entra sync, don't hook `run_job` and `sync_one`: queue it inside `entra_sync::set_idp_department` (on its `conn`) whenever it changes a membership. That also covers `pdms sync-directory` and mapping edits in `entra_values.rs`.
> - **Task 4:** reuse P3's `sharepoint::files::flag_changed` (make it `pub(crate)` from `sharepoint`) instead of writing a second `sharepoint.changed_outside` event, so the action has one shape. A file already flagged by a download must still reach administrators: the change check flags it (sets `changed_by`/`changed_at` and notifies) when `changed_by IS NULL` (owner answer 3 below).
> - **Task 5:** Graph failures in restore, accept and re-check go through `sharepoint::unavailable(…)` (`CoreError::Unavailable`), so the page shows a message instead of an error page (D-102).
> - **Task 6:** `integrity.rs` computes `files_checked` as `blobs - sharepoint_files_skipped`: count local files separately; remove P3's `sharepoint_files_skipped` audit context and report field once SharePoint files are checked; add `sharepoint_file` to the `Problem.kind` doc comment.
> - **Task 7 (pages):** follow R2's admin page pattern (R2 spec §5.2; `admin/entra.html`, `admin/deleted.html`): `{% block heading %}` with `header.page-head`, `<h1>{{ l.title }}</h1>`, `.page-lede`, `.page-actions` ("Check for changes now" the only primary button); sections as `section.panel` with `.panel-head`; run status as `dl.facts`; tables in `.table-wrap`; `p.empty` for empty lists; "not turned on" as `flash-info role="status"`, flagged files as `flash-error role="alert"`. The handler calls `.admin_tab("sharepoint")`; the menu item goes in the *Integrations* group of `Layout::admin_groups()` (`views.rs`), not `base.html`. The Entra page section is a `section.panel`. The document-page notice goes in `templates/documents/show.html` inside `header.doc-head`, after `.doc-title-row` and before `nav.on-this-page`, as a `p.flash`. Add `/admin/sharepoint` to `every_admin_page_uses_the_admin_pattern` (`tests/shell.rs`) and to `tools/ui-screenshots/shoot.py`.
> - **Task 8 (installer and Compose):** the Outlook anchors are gone (D-087): add `PDMS_GRAPH__CERTIFICATE_FILE` after `PDMS_EMAIL__SMTP_PASSWORD_FILE`, the `graph_certificate` secret after `smtp_password`, and to both secrets lists; in `install.sh` drop `outlook` from the `--configure` lists, add the new variables after the existing `PDMS_INSTALL_*` help lines, extend the `for f in oidc_client_secret smtp_password` loop, and insert the step before the "Removed: Outlook calendar sync" comment. Today's installer also shows the Caddy ports and the Entra redirect URI; keep both. How `[storage] backend` is set, and the certificate file name and thumbprint, follow owner answers 1 and 2 below. ADMIN_GUIDE's manual Compose edit for the certificate (Entra sync step 3) must get an "undo this before `git pull`" note, because the installer now does it.
> - **Task 9 (docs):** ADMIN_GUIDE already has "SharePoint storage" in §4 (from P3): update it (site grant, `delta_interval_minutes`, the integrity and known-gap bullets, the `[storage] backend` row) rather than adding a section in §12, and rewrite Entra step 3. ARCHITECTURE: add a §12 row `9 (0014_sharepoint_access, P4)` in the current style. DECISIONS: add D-104 to D-110, and edit the status lines of D-082, D-084 and D-089 by hand (the plan's `sed` no longer fits, and would wrongly change D-083). ROADMAP: §0.3 row 1, §0.1, §0.5, §0.6, and the §4 orphans row (as CLAUDE.md describes); the plan's `1b`/`2a` rows don't exist. Index: `0014`, D-104 to D-110. After P4, `tests/sharepoint.rs` has 27 tests plus 3 ignored live tests.
>
> **The owner's answers (2026-10-23).** They go into DECISIONS in Task 9: answer 1 replaces this plan's `config/pdms.env` decision (D-108), and the others are recorded with the decision they touch.
> 1. **Switching SharePoint storage on (Task 8):** the installer's existing `[storage]` block (today written by the ClamAV step) owns all of `[storage]`, including `backend`. No `config/pdms.env`, no Compose `env_file`, no Compose 2.24 requirement. A hand-written `[storage] backend` outside the managed block is detected and left alone, with a warning.
> 2. **Certificate (Task 8):** keep P2's `secrets/graph_certificate` and its public half `secrets/graph_certificate.crt` (no `config/pdms-service.cer`). Show both thumbprints: SHA-256 (what Administration → Entra ID sync shows) and SHA-1 (what the Entra admin center lists).
> 3. **"Changed outside PDMS" (Task 4):** one audit shape, P3's (`flag_changed`: category system, outcome failure, context `source`, `item_id`, `detail`), for the download check and the change check. A change first found by a download also notifies administrators.
> 4. **Installer steps (Task 8):** `--configure sharepoint` stays separate from the Entra sync. Both use the same certificate; the SharePoint step generates it only if it is missing.
> 5. **Restricted documents on the status page (Task 7):** for a document the administrator cannot open, the page shows "A Restricted document" and the version number only: no ID, title or path. "Restore original" still works.
> 6. **Document-page notice (Task 7):** a status message, not an alert, for everyone who can see the document: "A file of this document was changed outside PDMS. Downloads of it are paused until an administrator checks it." Administrators also see a link to Administration → SharePoint.

**Goal:** Finish SharePoint storage so it can run in the county:

- staff can browse their own department's folder read-only;
- edits, uploads, and deletions made directly in SharePoint are caught within minutes and resolved by an administrator;
- `verify-integrity` checks SharePoint files;
- administrators get a status page;
- the installer configures everything;
- the county has a step-by-step setup guide.

**Architecture:** Four new modules in `pdms-core::sharepoint`:

- `access`: per-person read grants on `Departments/<department>/`.
- `changes`: the Graph delta feed compared with `sharepoint_files`.
- `resolve`: restore the original, accept as a new version, re-check, dismiss.
- `status`: run records, administrator notices, and status-panel queries.

They are wired into the job worker, the directory functions that change memberships, P2's Entra sync job, and a new `/admin/sharepoint` page. A new `ManageSharePoint` capability gates them. Docker Compose and `install.sh` gain the Graph certificate secret and the SharePoint settings.

**Tech Stack:** Rust 2024, Axum 0.8, SQLx 0.9 (`query!` macros, `.sqlx/` offline data), Askama 0.16, Microsoft Graph v1.0 through P2's `GraphClient`, Bash (installer), Docker Compose ≥ 2.24.

**Spec:** [docs/superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md](../specs/2026-10-14-sharepoint-storage-entra-sync-design.md): §7.4 (department folder access), §7.6 (change detection and resolution), §7.7 (integrity), §9–§11 (configuration, errors, security), §12 (P4 tests), §13 (rollout), Appendix A (county setup). **Index:** [2026-10-14-pdms-sharepoint-entra-index.md](2026-10-14-pdms-sharepoint-entra-index.md) ("Produced by P4"). **Depends on:** P1, P2, and P3 merged. This plan uses their names as written in [P2](2026-10-14-p2-graph-client-entra-sync.md) and [P3](2026-10-14-p3-sharepoint-storage.md); the deviations from the index are listed at the end.

## Global Constraints

- **Job kinds:** `sharepoint.reconcile_access` (`sharepoint::RECONCILE_JOB`) and `sharepoint.delta` (`sharepoint::DELTA_JOB`).
- **Run records:** these go in P2's `sync_cursors` under the names `sharepoint.delta` (with the delta link) and `sharepoint.reconcile` (no link). P2's `entra.users` row is read, never written.
- **Folder access (spec §7.4):**
  - Only `Departments/<department>/` folders get per-person grants.
  - The readers are the department's members whose account is active and whose `users.source = 'entra'`. Local break-glass accounts never get grants.
  - Every grant is exactly `POST /drives/{drive}/items/{folder}/invite` with the body `{"recipients":[{"email":<email>}],"roles":["read"],"requireSignIn":true,"sendInvitation":false}`.
  - PDMS removes only grants it recorded in `sharepoint_folder_access`. Any other permission on the folder is reported and never changed.
  - `County-wide/` and `Restricted/` are set up by hand (Appendix A).
- **Change detection (spec §7.6):**
  - A tracked file counts as changed when its `size` or `file.hashes.quickXorHash` differs from what `sharepoint_files` recorded at upload.
  - Who made the change is recorded and shown, never trusted.
  - A change counts as PDMS's own only when `lastModifiedBy.application.id` equals `[graph] client_id`. That only suppresses needless mirror repairs.
- **Untracked files:** a file in the library that no `sharepoint_files` row points at is shown to administrators only after **10 minutes**, because an upload in progress has no row yet. PDMS never deletes such files.
- **Audit actions:** `sharepoint.access_granted`, `sharepoint.access_revoked`, `sharepoint.changed_outside`, `sharepoint.change_resolved`, `sharepoint.unexpected_dismissed`. `pdms_audit::append` stays the **last** statement before `tx.commit()` (D-004).
- **Capability:** `Capability::ManageSharePoint`, held by the org-wide `admin` role only. "Accept as new version" also needs `edit` on the document, through the normal `documents::add_version` check.
- **Notices to administrators** (in-app, plus email when email is set up):
  - Changes found: subject "SharePoint: N file(s) changed outside PDMS", linking to `/admin/sharepoint`.
  - Graph sign-in failure (HTTP 401/403 → `GraphError::Auth`): subject "PDMS cannot sign in to Microsoft Graph", at most **once an hour** (dedupe key `graph-auth:YYYYMMDDHH`).
  - Neither notice names a document.
- **Restores are verified:** "Restore original" picks the SharePoint version whose content matches the version's recorded SHA-256 (`document_versions.blob_sha256`). It never trusts a version label.
- **Storage choice:** `[storage] backend` is set through `config/pdms.env` (`PDMS_STORAGE__BACKEND=sharepoint`), loaded by Compose `env_file`, because the ClamAV block already owns the `[storage]` table.
- **Graph certificate:** Compose always mounts the secret `graph_certificate` and sets `PDMS_GRAPH__CERTIFICATE_FILE: /run/secrets/graph_certificate`. P2's `_FILE` loader reads it, and an empty file means "not configured".
- **Migration:** `migrations/0014_sharepoint_access.sql`. Never edit 0001–0013. Migrations carry no SPDX header (D-080).
- **New Rust files** start with `// SPDX-License-Identifier: AGPL-3.0-or-later` and `// Copyright (C) 2026 PDMS contributors`. New integration-test files use the `#![allow(` header pattern of `crates/pdms-core/tests/authz_sql.rs`.
- **Lints:** `unwrap`, `expect`, `panic`, and `dbg` are denied outside tests; `unsafe` is forbidden. Every task must pass `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- **Offline query data:** after any `sqlx::query!` or migration change, run `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/`.
- **Dev environment:** use "Shared development setup" in the index. `DATABASE_URL` must point at the dev database with migrations through 0014 applied before building.

## Review Focus

Five inputs the spec implies but its happy path doesn't exercise. Each has a test in its owning task.

1. **A person whose address SharePoint cannot resolve** (no mailbox, a typo in Entra). Their grant fails, but everyone else in the department still gets access, and the failure shows on the status panel. Task 2: `one_unresolvable_person_does_not_block_the_rest`.
2. **A file PDMS is uploading appears in the change feed before its database row commits.** It must not be reported as untracked. Task 4: `an_upload_in_progress_is_not_reported`.
3. **The Graph certificate expires or the site grant is removed.** Administrators are told once an hour, not every five minutes. The status panel shows the error, and the next run after the fix succeeds. Task 1: `graph_sign_in_failures_reach_admins_once_an_hour`; Task 2: `folder_access_reports_sign_in_failures`.
4. **The change-feed token is rejected after a long outage (HTTP 410).** PDMS rereads everything without flagging files that did not change. Task 4: `an_expired_change_token_rereads_without_false_alarms`.
5. **The administrator resolving a change cannot open the document** (Restricted). The status panel hides the title and path. "Restore original" still works, and "Accept as new version" is refused for lack of `edit`. Task 5: `accept_needs_edit_rights_restore_does_not`; Task 7: `sharepoint_panel_is_for_administrators`.

## Files

| File | Responsibility |
|---|---|
| `migrations/0014_sharepoint_access.sql` (create) | `sharepoint_folder_access`, `sharepoint_foreign_grants`, `sharepoint_unexpected_items`, `sharepoint_files.changed_by/changed_at` |
| `crates/pdms-authz/src/capability.rs` (modify) | `Capability::ManageSharePoint` |
| `crates/pdms-core/src/notifications/dispatch.rs` (modify) | `Visibility::Anyone` for notices that name no document |
| `crates/pdms-core/src/sharepoint/status.rs` (create) | `RunError`, run records, administrator notices, panel queries, document flags |
| `crates/pdms-core/src/sharepoint/access.rs` (create) | department-folder grants (`RECONCILE_JOB`) |
| `crates/pdms-core/src/sharepoint/changes.rs` (create) | change detection (`DELTA_JOB`), `DriveItem`, `observe` |
| `crates/pdms-core/src/sharepoint/resolve.rs` (create) | restore, accept, re-check, dismiss |
| `crates/pdms-core/src/sharepoint/mod.rs` (modify) | module declarations, re-exports, `register_ops_jobs` |
| `crates/pdms-core/src/directory/admin.rs`, `directory/sync.rs`, `directory/entra_sync.rs` (modify) | queue a folder-access run when memberships change |
| `crates/pdms-core/src/maintenance.rs` (modify) | register P4's jobs |
| `crates/pdms-core/src/integrity.rs`, `crates/pdms/src/main.rs` (modify) | SharePoint files in `verify-integrity`, `--rehash-sharepoint` |
| `crates/pdms-core/tests/mock_graph/sp.rs` (modify: P3's SharePoint part of P2's mock Graph) | change feed, versions, permissions, recycle bin, people editing SharePoint, refused sign-in |
| `crates/pdms-core/tests/sharepoint_ops.rs` (create) | P4's core integration tests |
| `crates/pdms-core/tests/entra_sync.rs` (modify, from P2) | the sync queues a folder-access run |
| `crates/pdms-core/tests/sharepoint.rs` (modify, from P3) | P3's integrity test renamed; shared live-test setup and two opt-in live tests |
| `crates/pdms-web/src/routes/sharepoint_admin.rs`, `templates/admin/sharepoint.html` (create) | Administration → SharePoint |
| `crates/pdms-web/src/{lib.rs, views.rs, routes/mod.rs, routes/documents.rs}`, `templates/{base.html, documents/show.html, admin/entra.html}` (modify) | routes, navigation, document-page notice, link from the Entra page |
| `crates/pdms-web/tests/sharepoint_admin.rs` (create) | web tests |
| `install.sh`, `docker-compose.yml`, `.gitignore` (modify) | `--configure sharepoint`, the certificate secret, `config/pdms.env` |
| `docs/SHAREPOINT_SETUP.md` (create); `docs/ADMIN_GUIDE.md`, `docs/BACKUP.md`, `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`, `docs/ROADMAP.md`, the plan index (modify) | county runbook and records |

---

### Task 1: Migration 0014, the `ManageSharePoint` capability, and run records with administrator notices

**Files:**
- Create: `migrations/0014_sharepoint_access.sql`
- Modify: `crates/pdms-authz/src/capability.rs`, `crates/pdms-authz/src/tests.rs`
- Modify: `crates/pdms-core/src/notifications/dispatch.rs`
- Create: `crates/pdms-core/src/sharepoint/status.rs`
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Test: `crates/pdms-core/tests/sharepoint_ops.rs` (create)

**Interfaces:**
- Consumes:
  - P2: `pdms_core::graph::GraphError`, including `Auth(String)`, and `sync_cursors (name, delta_link, last_run, last_error, last_summary)`.
  - P3: `pdms_core::sharepoint::{SharePoint, Resolved}`, with `SharePoint::graph(&self) -> &GraphClient` and `SharePoint::resolved(&self) -> CoreResult<&Resolved>`; and the `sharepoint_files` table.
  - Existing: `notifications::actions::{send, Notice}`.
- Produces:
  - `Capability::ManageSharePoint`;
  - `pdms_core::sharepoint::status::{RunError, DELTA_CURSOR, RECONCILE_CURSOR, ENTRA_CURSOR, report_failure, record_ok, record_error, notify_admins, parts}`:
    - `pub enum RunError { Graph(GraphError), Core(CoreError) }` with `is_auth()`, `Display`, `From` for `GraphError`/`CoreError`/`sqlx::Error`/`AuditError`, and `From<RunError> for CoreError`;
    - `pub async fn report_failure(core: &Core, name: &str, err: &RunError) -> CoreResult<()>`;
    - `pub(crate) async fn record_ok(conn: &mut PgConnection, name: &str, delta_link: Option<&str>) -> CoreResult<()>`;
    - `pub(crate) async fn record_error(db: &PgPool, name: &str, message: &str) -> CoreResult<()>`;
    - `pub(crate) async fn notify_admins(conn: &mut PgConnection, core: &Core, notice: Notice) -> CoreResult<usize>`;
    - `pub(crate) async fn parts(core: &Core) -> CoreResult<(&GraphClient, &Resolved)>`;
  - `notifications::dispatch::Visibility::Anyone`.

> **As built (2026-10-23):** `status::parts` and `status::record_ok` are added in Task 2 instead, where they are first used (an unused function fails `clippy -D warnings`). `ENTRA_CURSOR` reuses `entra_sync::CURSOR`, now `pub(crate)`. The test file puts the SPDX lines first.

- [x] **Step 1: Confirm the P2/P3 names this plan uses.** Run:

```bash
grep -n "pub fn graph\|pub async fn resolved\|pub struct Resolved\|pub fn area_id" crates/pdms-core/src/sharepoint/mod.rs
grep -n "pub async fn department_folder_id\|pub async fn enqueue_mirror\|pub const MIRROR_SWEEP_JOB\|pub fn register_jobs" -r crates/pdms-core/src/sharepoint/
grep -n "Auth(String)\|ResyncRequired" crates/pdms-core/src/graph/*.rs
grep -n "CREATE TABLE sharepoint_files" -A16 migrations/0013_sharepoint_files.sql
grep -n "pub async fn run_job\|pub async fn sync_one" crates/pdms-core/src/directory/entra_sync.rs
grep -n "pub struct SpState\|pub const DRIVE\|pub(super) fn route\|pub(super) fn blob\|fn json(&self" crates/pdms-core/tests/mock_graph/sp.rs
```

Expected: every name is found, and `sharepoint_files` has `item_id`, `list_item_id`, `name`, `quick_xor_hash`, `size`, `last_known_path`, `state`, and `checked_at`. If a name differs, use the real name everywhere in this plan's code, and record it in the index in Task 9.

- [x] **Step 2: Write the migration.** Create `migrations/0014_sharepoint_access.sql`:

```sql
-- SharePoint folder access, changes made outside PDMS, and the status panel
-- (spec §7.4, §7.6; plan P4).

-- Read access PDMS granted, one person at a time, on Departments/<department>/.
CREATE TABLE sharepoint_folder_access (
    department_id uuid NOT NULL REFERENCES departments (id),
    user_id       uuid NOT NULL REFERENCES users (id),
    permission_id text NOT NULL,                 -- Graph permission id, for removal
    granted_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (department_id, user_id)
);

-- Permissions on a department folder that PDMS did not grant: reported, never changed.
CREATE TABLE sharepoint_foreign_grants (
    department_id uuid NOT NULL REFERENCES departments (id),
    permission_id text NOT NULL,
    granted_to    text NOT NULL,
    roles         text NOT NULL,
    seen_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (department_id, permission_id)
);

-- Who changed a tracked file outside PDMS, and when it was noticed.
ALTER TABLE sharepoint_files
    ADD COLUMN changed_by text,
    ADD COLUMN changed_at timestamptz;

-- Files in the library that no document version points at.
CREATE TABLE sharepoint_unexpected_items (
    item_id      text PRIMARY KEY,
    name         text NOT NULL,
    path         text NOT NULL,
    created_by   text NOT NULL,
    by_pdms      boolean NOT NULL,               -- left behind by a PDMS save that did not finish
    first_seen   timestamptz NOT NULL DEFAULT now(),
    last_seen    timestamptz NOT NULL DEFAULT now(),
    dismissed_by uuid REFERENCES users (id),
    dismissed_at timestamptz
);

GRANT SELECT, INSERT, DELETE ON sharepoint_folder_access, sharepoint_foreign_grants TO pdms_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON sharepoint_unexpected_items TO pdms_app;
GRANT SELECT, INSERT, UPDATE ON sharepoint_files TO pdms_app;
```

Apply it: `sqlx migrate run --source migrations`
Expected: `Applied 14/migrate sharepoint access`.

- [x] **Step 3: Write the failing capability test.** Append to `crates/pdms-authz/src/tests.rs`:

```rust
#[test]
fn sharepoint_management_is_for_org_admins_only() {
    let admin = with_org_role(user(ALICE, &[]), "admin");
    assert!(has_capability(&admin, Capability::ManageSharePoint));
    for key in ["auditor", "records_officer", "appointments_clerk", "legal_counsel"] {
        let p = with_org_role(user(BOB, &[]), key);
        assert!(!has_capability(&p, Capability::ManageSharePoint), "{key}");
    }
    let mut scoped = user(CAROL, &[OPS]);
    scoped.roles.push(RoleHolding {
        role_id: ROLES[0],
        role_key: "admin".into(),
        department_id: Some(OPS),
    });
    assert!(!has_capability(&scoped, Capability::ManageSharePoint));
}
```

Run: `cargo test -p pdms-authz sharepoint_management_is_for_org_admins_only`
Expected: compile error `no variant named ManageSharePoint`.

- [x] **Step 4: Add the capability.** In `crates/pdms-authz/src/capability.rs`, add to `enum Capability` after `ManageIntegrations`:

```rust
    /// SharePoint status, department-folder access, and resolving files that
    /// were changed directly in SharePoint.
    ManageSharePoint,
```

In `granted_by`, extend the first arm so it reads:

```rust
            Capability::ManageDirectory
            | Capability::ManageSettings
            | Capability::ManageDocumentSchema
            | Capability::ManageTags
            | Capability::RestoreDeletedDocuments
            | Capability::ManageSharePoint => &[ADMIN],
```

Run: `cargo test -p pdms-authz`
Expected: `test result: ok.` with 0 failed.

- [x] **Step 5: Let notices skip the document check.** In `crates/pdms-core/src/notifications/dispatch.rs`, add a variant to `pub(crate) enum Visibility`:

```rust
    /// Not about a document or a position: the caller chose the recipients,
    /// and the notice names no document (for example SharePoint status for
    /// administrators).
    Anyone,
```

In `Visibility::allows`, add the arm `Visibility::Anyone => true,`. In `Visibility::denied_reason`, add `Visibility::Anyone => "recipient is not active",`.

- [x] **Step 6: Write the failing status test.** Create `crates/pdms-core/tests/sharepoint_ops.rs`:

```rust
#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! SharePoint folder access, change detection, resolution, and integrity (P4),
//! against P3's in-process mock of Microsoft Graph.

mod common;

use common::*;
use pdms_core::graph::GraphError;
use pdms_core::sharepoint::status;
use pdms_core::{Core, CoreError, directory};
use uuid::Uuid;

/// A person synced from Entra (only they get SharePoint folder access).
async fn entra_user(core: &Core, email: &str, dept: Option<Uuid>) -> Uuid {
    let id = directory::create_user(&core.db, &system(), email, email, dept).await.unwrap();
    sqlx::query("UPDATE users SET source = 'entra' WHERE id = $1")
        .bind(id)
        .execute(&core.db)
        .await
        .unwrap();
    id
}

/// An Entra person with the org-wide administrator role.
async fn admin(core: &Core, email: &str, dept: Option<Uuid>) -> Uuid {
    let id = entra_user(core, email, dept).await;
    let role = directory::role_id_by_key(&core.db, "admin").await.unwrap();
    directory::assign_role(&core.db, &system(), id, role, None).await.unwrap();
    id
}

async fn in_app_notices(core: &Core) -> Vec<(Uuid, String)> {
    sqlx::query_as(
        "SELECT dl.user_id, n.subject FROM notification_deliveries dl
           JOIN notifications n ON n.id = dl.notification_id
          WHERE dl.channel = 'in_app' ORDER BY dl.created_at",
    )
    .fetch_all(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn graph_sign_in_failures_reach_admins_once_an_hour() {
    let (core, _db) = core().await;
    let boss = admin(&core, "admin@county.example", None).await;
    entra_user(&core, "clerk@county.example", None).await;
    let denied = status::RunError::Graph(GraphError::Auth(
        "InvalidAuthenticationToken: AADSTS700027 certificate expired".into(),
    ));
    status::report_failure(&core, status::DELTA_CURSOR, &denied).await.unwrap();
    status::report_failure(&core, status::RECONCILE_CURSOR, &denied).await.unwrap();
    assert_eq!(
        in_app_notices(&core).await,
        vec![(boss, "PDMS cannot sign in to Microsoft Graph".to_owned())],
        "one notice an hour, only to administrators"
    );
    let err: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM sync_cursors WHERE name = 'sharepoint.delta'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert!(err.unwrap().contains("AADSTS700027"));
    // Other failures are recorded but tell nobody.
    let other = status::RunError::Core(CoreError::internal("disk full"));
    status::report_failure(&core, status::DELTA_CURSOR, &other).await.unwrap();
    assert_eq!(in_app_notices(&core).await.len(), 1);
}
```

Run: `cargo test -p pdms-core --test sharepoint_ops`
Expected: compile error `could not find status in sharepoint`.

- [x] **Step 7: Implement `status.rs`.** Create `crates/pdms-core/src/sharepoint/status.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! SharePoint background work: run records for the status panel (in P2's
//! `sync_cursors`), notices to administrators, and the queries behind
//! Administration → SharePoint.

use std::collections::BTreeSet;
use std::fmt;

use chrono::Utc;
use pdms_audit::{AuditEvent, Category};
use pdms_authz::role_keys;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{Resolved, SharePoint};
use crate::Core;
use crate::context::Ctx;
use crate::error::{CoreError, CoreResult};
use crate::graph::{GraphClient, GraphError};
use crate::notifications::actions::{self, Notice};
use crate::notifications::dispatch::Visibility;

/// `sync_cursors` names.
pub const DELTA_CURSOR: &str = "sharepoint.delta";
pub const RECONCILE_CURSOR: &str = "sharepoint.reconcile";
/// P2's Entra sync cursor (read only, for the panel).
pub const ENTRA_CURSOR: &str = "entra.users";

/// A failed SharePoint background run. Graph errors are kept as they are, so
/// sign-in failures can be told apart.
#[derive(Debug)]
pub enum RunError {
    Graph(GraphError),
    Core(CoreError),
}

impl RunError {
    /// Microsoft Graph refused PDMS's sign-in or access (HTTP 401/403).
    pub fn is_auth(&self) -> bool {
        match self {
            RunError::Graph(GraphError::Auth(_)) => true,
            // P3's lazy library lookup (`SharePoint::resolved`) hands Graph
            // errors back as `CoreError::Internal(<GraphError's text>)`.
            RunError::Core(CoreError::Internal(m)) => {
                m.starts_with(&GraphError::Auth(String::new()).to_string())
            }
            _ => false,
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Graph(e) => write!(f, "{e}"),
            RunError::Core(e) => write!(f, "{e}"),
        }
    }
}

impl From<GraphError> for RunError {
    fn from(e: GraphError) -> Self {
        RunError::Graph(e)
    }
}

impl From<CoreError> for RunError {
    fn from(e: CoreError) -> Self {
        RunError::Core(e)
    }
}

impl From<sqlx::Error> for RunError {
    fn from(e: sqlx::Error) -> Self {
        RunError::Core(e.into())
    }
}

impl From<pdms_audit::AuditError> for RunError {
    fn from(e: pdms_audit::AuditError) -> Self {
        RunError::Core(e.into())
    }
}

impl From<RunError> for CoreError {
    fn from(e: RunError) -> Self {
        match e {
            RunError::Graph(g) => g.into(),
            RunError::Core(c) => c,
        }
    }
}

/// The Graph client and the resolved library, when SharePoint storage is on.
pub(crate) async fn parts(core: &Core) -> CoreResult<(&GraphClient, &Resolved)> {
    let sp: &SharePoint = core
        .sharepoint
        .as_ref()
        .ok_or_else(|| CoreError::Conflict("SharePoint storage is not turned on.".into()))?;
    let resolved = sp.resolved().await?;
    Ok((sp.graph(), resolved))
}

/// Record a successful run; `delta_link` replaces the stored link when given.
pub(crate) async fn record_ok(
    conn: &mut PgConnection,
    name: &str,
    delta_link: Option<&str>,
) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO sync_cursors (name, delta_link, last_run, last_error) VALUES ($1, $2, now(), NULL)
         ON CONFLICT (name) DO UPDATE
            SET delta_link = coalesce(EXCLUDED.delta_link, sync_cursors.delta_link),
                last_run = now(), last_error = NULL",
        name,
        delta_link
    )
    .execute(conn)
    .await?;
    Ok(())
}

/// Record a failed run (the stored delta link is kept).
pub(crate) async fn record_error(db: &PgPool, name: &str, message: &str) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO sync_cursors (name, last_run, last_error) VALUES ($1, now(), $2)
         ON CONFLICT (name) DO UPDATE SET last_run = now(), last_error = EXCLUDED.last_error",
        name,
        message
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Send `notice` to every org-wide administrator (inactive ones are skipped).
pub(crate) async fn notify_admins(
    conn: &mut PgConnection,
    core: &Core,
    notice: Notice,
) -> CoreResult<usize> {
    let admins: BTreeSet<Uuid> = sqlx::query_scalar!(
        "SELECT a.user_id FROM role_assignments a JOIN roles r ON r.id = a.role_id
          WHERE r.key = $1 AND a.department_id IS NULL",
        role_keys::ADMIN
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let ctx = Ctx::system("sharepoint");
    let about = AuditEvent::new(Category::System, "notification.created", ctx.actor.clone(), "system");
    actions::send(conn, core, &ctx, notice, admins, &Visibility::Anyone, about).await
}

/// Record a failed run for the status panel. A Graph sign-in failure also
/// tells administrators, at most once an hour.
pub async fn report_failure(core: &Core, name: &str, err: &RunError) -> CoreResult<()> {
    let message: String = err.to_string().chars().take(500).collect();
    tracing::warn!(job = name, error = %message, "SharePoint background run failed");
    record_error(&core.db, name, &message).await?;
    if err.is_auth() {
        let mut tx = core.db.begin().await?;
        notify_admins(
            &mut tx,
            core,
            Notice {
                subject: "PDMS cannot sign in to Microsoft Graph".into(),
                body: format!(
                    "SharePoint storage and Entra sync are paused until this is fixed: {message}. \
                     Check the PDMS Service certificate and app registration (docs/SHAREPOINT_SETUP.md)."
                ),
                link: "/admin/sharepoint".into(),
                document_id: None,
                appointment_id: None,
                dedupe_key: format!("graph-auth:{}", Utc::now().format("%Y%m%d%H")),
            },
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}
```

In `crates/pdms-core/src/sharepoint/mod.rs`, add `pub mod status;` below the existing module declarations.

- [x] **Step 8: Run the tests and confirm they pass.**

```bash
cargo test -p pdms-core --test sharepoint_ops
cargo test -p pdms-core --test notifications
```

Expected: `test graph_sign_in_failures_reach_admins_once_an_hour ... ok`, and both binaries report `test result: ok.`

- [x] **Step 9: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add migrations/0014_sharepoint_access.sql crates/pdms-authz crates/pdms-core/src/notifications/dispatch.rs \
  crates/pdms-core/src/sharepoint crates/pdms-core/tests/sharepoint_ops.rs .sqlx
git commit -m "P4: migration 0014, ManageSharePoint, run records and admin notices"
```

---
### Task 2: Department staff get read access on their folder

> **From Task 1:** add `status::parts` and `status::record_ok` (their code is in Task 1, Step 7) to `status.rs` here, with the `use super::{Resolved, SharePoint};` and `GraphClient` imports they need.

**Files:**
- Modify: `crates/pdms-core/tests/mock_graph/sp.rs` (P3's SharePoint part of P2's mock Graph)
- Create: `crates/pdms-core/src/sharepoint/access.rs`
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Test: `crates/pdms-core/tests/sharepoint_ops.rs`

**Interfaces:**
- Consumes:
  - Task 1: `status::{parts, record_ok, record_error, report_failure, RunError, RECONCILE_CURSOR}`.
  - P3: `pdms_core::sharepoint::{department_folder_id, Area}`.
  - P2: `GraphClient::{post, get, delete}`.
- Produces:
  - `pdms_core::sharepoint::access::{RECONCILE_JOB, ReconcileReport, reconcile, enqueue_reconcile, reconcile_slot, plan}`:
    - `pub async fn reconcile(core: &Core) -> CoreResult<ReconcileReport>` (a no-op without SharePoint);
    - `pub async fn enqueue_reconcile<'e>(exec: impl PgExecutor<'e>) -> CoreResult<()>`;
    - `pub fn reconcile_slot(now: i64) -> (String, i64)`;
    - `pub fn plan(desired: &BTreeMap<Uuid, String>, granted: &BTreeMap<Uuid, String>) -> (Vec<(Uuid, String)>, Vec<(Uuid, String)>)`;
    - `pub struct ReconcileReport { departments, granted, revoked, foreign: u32, errors: Vec<String> }`;
  - re-export `pdms_core::sharepoint::RECONCILE_JOB`;
  - mock helpers `MockGraph::{human_edit, human_rename, human_delete, restore_from_recycle_bin, human_upload, app_upload, add_group_permission, readers, content_of, invites, expire_delta, deny}`.

> **As built (2026-10-23):**
> - Migration `0014` grants the app SELECT, INSERT and DELETE on `sharepoint_folder_access`, not UPDATE, so a new grant deletes any earlier row and inserts the new one in the same transaction instead of `ON CONFLICT … DO UPDATE`.
> - The run also covers departments that already have a `Departments/` folder (`sharepoint_department_folders`), so access added by hand is still reported once nobody in the department has access any more. The plan's query skipped them.
> - The tests import `mock_graph::certificate` instead of defining their own.

- [x] **Step 1: Extend the mock Graph with what SharePoint does beyond P3's needs.** P3's SharePoint endpoints live in `crates/pdms-core/tests/mock_graph/sp.rs`, a child of P2's mock. The mock needs a change feed, version history, permissions, a recycle bin, people editing SharePoint directly, and refused sign-in. Edit `sp.rs`:

(a) Change the import from `super` to `use super::{CLIENT, MockGraph, MockState, graph_error, json_resp};`.

(b) Change `SpItem`'s derive to `#[derive(Clone, Debug, Default)]` and add these fields after `pub list_item: String,`:

```rust
    /// Earlier contents, oldest first: SharePoint's version history. `content` is current.
    pub history: Vec<Vec<u8>>,
    /// Who created / last changed it: `None` is PDMS's app, `Some(name)` a person.
    pub created_by: Option<String>,
    pub modified_by: Option<String>,
    /// Direct permissions: (permission id, user email, SharePoint group name).
    pub perms: Vec<(String, Option<String>, Option<String>)>,
```

In `SpState::ensure_root`, add `..Default::default()` as the last field of the `SpItem { … }` literal.

(c) Add these fields to `pub struct SpState` (it derives `Default`):

```rust
    /// Deleted items (the site recycle bin), by id.
    recycle: BTreeMap<String, SpItem>,
    /// The change feed: (sequence number, item id).
    changes: Vec<(u64, String)>,
    seq: u64,
    /// The next delta request that carries a token gets HTTP 410.
    expire_delta: bool,
    /// Every sharing invitation, as "invite <email> requireSignIn=… sendInvitation=… roles=…".
    invites: Vec<String>,
```

(d) In `impl SpState`, replace `json` and `insert`, and add `touch` and `parent_path`:

```rust
    fn touch(&mut self, id: &str) {
        self.seq += 1;
        let seq = self.seq;
        self.changes.push((seq, id.to_owned()));
    }

    /// Graph's `parentReference.path`: `/drives/{id}/root:` plus the folders above.
    fn parent_path(&self, i: &SpItem) -> String {
        let mut names = Vec::new();
        let mut cur = i.parent.clone();
        while let Some(p) = cur {
            match self.items.get(&p) {
                Some(it) if it.id != "root" => {
                    names.push(it.name.clone());
                    cur = it.parent.clone();
                }
                _ => break,
            }
        }
        names.reverse();
        let mut s = format!("/drives/{DRIVE}/root:");
        for n in names {
            s.push('/');
            s.push_str(&n);
        }
        s
    }

    fn json(&self, i: &SpItem) -> Value {
        let mut v = json!({
            "id": i.id, "name": i.name, "size": i.content.len(),
            "parentReference": {"id": i.parent, "driveId": DRIVE, "path": self.parent_path(i)},
            "listItem": {"id": i.list_item},
            "createdBy": identity(i.created_by.as_deref()),
            "lastModifiedBy": identity(i.modified_by.as_deref()),
        });
        if i.id == "root" {
            v["root"] = json!({});
        }
        if i.folder {
            v["folder"] = json!({"childCount": 0});
        } else {
            let mut h = QuickXorHash::new();
            h.update(&i.content);
            v["file"] = json!({"hashes": {"quickXorHash": h.finish_base64()}});
        }
        v
    }

    fn insert(&mut self, parent: &str, name: String, folder: bool, content: Vec<u8>) -> Value {
        let id = self.id("sp-item");
        let list_item = self.id("sp-li");
        let item = SpItem {
            id: id.clone(),
            name,
            parent: Some(parent.to_owned()),
            folder,
            content,
            fields: Map::new(),
            list_item,
            ..Default::default()
        };
        let out = self.json(&item);
        self.touch(&id);
        self.items.insert(id, item);
        out
    }
```

(e) In `route`, replace the `("PATCH", ["sites", SITE, "lists", LIST, "items", li, "fields"])` arm with:

```rust
        ("PATCH", ["sites", SITE, "lists", LIST, "items", li, "fields"]) => {
            let v: Value = serde_json::from_slice(body).unwrap();
            match sp.items.values_mut().find(|i| i.list_item == *li) {
                Some(item) => {
                    for (k, val) in v.as_object().unwrap() {
                        item.fields.insert(k.clone(), val.clone());
                    }
                    item.modified_by = None;
                    let (id, fields) = (item.id.clone(), item.fields.clone());
                    sp.touch(&id);
                    json_resp(200, Value::Object(fields))
                }
                None => graph_error(404, "itemNotFound"),
            }
        }
```

Replace the `("DELETE", ["drives", DRIVE, "items", id])` arm with:

```rust
        ("DELETE", ["drives", DRIVE, "items", id]) => {
            if let Some(mut item) = sp.items.remove(*id) {
                item.modified_by = None;
                sp.recycle.insert((*id).to_owned(), item);
                sp.touch(id);
            }
            StatusCode::NO_CONTENT.into_response()
        }
```

Add these arms immediately before the final `_ => return None,`:

```rust
        ("GET", ["drives", DRIVE, "root"]) => {
            let root = sp.items["root"].clone();
            json_resp(200, sp.json(&root))
        }
        ("GET", ["drives", DRIVE, "root", "delta"]) => delta(sp, base, None),
        ("GET", ["drives", DRIVE, "root", "delta", token]) => delta(sp, base, token.parse().ok()),
        ("GET", ["drives", DRIVE, "items", id, "versions"]) => versions(sp, id),
        ("GET", ["drives", DRIVE, "items", id, "versions", v, "content"]) => {
            if sp.items.contains_key(*id) {
                let mut r = StatusCode::FOUND.into_response();
                r.headers_mut()
                    .insert(header::LOCATION, format!("{base}/blob/{id}~{v}").parse().unwrap());
                r
            } else {
                graph_error(404, "itemNotFound")
            }
        }
        ("POST", ["drives", DRIVE, "items", id, "versions", v, "restoreVersion"]) => {
            restore_version(sp, id, v)
        }
        ("POST", ["drives", DRIVE, "items", id, "invite"]) => invite(sp, id, body),
        ("GET", ["drives", DRIVE, "items", id, "permissions"]) => match sp.items.get(*id) {
            Some(i) => {
                let v: Vec<Value> = i.perms.iter().map(perm_json).collect();
                json_resp(200, json!({"value": v}))
            }
            None => graph_error(404, "itemNotFound"),
        },
        ("DELETE", ["drives", DRIVE, "items", id, "permissions", p]) => match sp.items.get_mut(*id) {
            Some(i) if i.perms.iter().any(|x| x.0 == *p) => {
                i.perms.retain(|x| x.0 != *p);
                StatusCode::NO_CONTENT.into_response()
            }
            _ => graph_error(404, "itemNotFound"),
        },
```

(f) Replace `blob` so `/blob/<item>~<version>` serves an earlier version:

```rust
/// A pre-authenticated download `/blob/<item>` or `/blob/<item>~<version>`;
/// `None` if not a SharePoint item.
pub(super) fn blob(s: &MockState, item: &str) -> Option<Response> {
    let (id, version) = match item.split_once('~') {
        Some((i, v)) => (i, Some(v)),
        None => (item, None),
    };
    let i = s.sp.items.get(id)?;
    let content = match version {
        Some(v) => version_content(i, v)?,
        None => i.content.clone(),
    };
    Some((StatusCode::OK, content).into_response())
}
```

In `move_item`, replace its last four lines (from `let item = sp.items.get_mut(id).unwrap();` to the end) with:

```rust
    let item = sp.items.get_mut(id).unwrap();
    item.parent = Some(parent);
    item.name = name;
    item.modified_by = None;
    let item = item.clone();
    sp.touch(id);
    json_resp(200, sp.json(&item))
```

(g) Add these functions at the end of `sp.rs`:

```rust
/// Graph's identity set for a change: PDMS's app (the mock's client id), or a person.
fn identity(person: Option<&str>) -> Value {
    match person {
        None => json!({
            "application": {"id": CLIENT, "displayName": "PDMS Service"},
            "user": {"displayName": "SharePoint App"}
        }),
        Some(n) => json!({"user": {
            "displayName": n,
            "email": format!("{}@county.example", n.to_lowercase().replace(' ', "."))
        }}),
    }
}

fn perm_json(p: &(String, Option<String>, Option<String>)) -> Value {
    match (&p.1, &p.2) {
        (Some(e), _) => json!({"id": p.0, "roles": ["read"], "grantedToV2": {"user": {"email": e, "displayName": e}}}),
        (None, g) => json!({"id": p.0, "roles": ["write"], "grantedToV2": {"siteGroup": {"displayName": g}}}),
    }
}

/// The change feed. The mock's delta link carries its token in the path.
fn delta(sp: &mut SpState, base: &str, token: Option<u64>) -> Response {
    if token.is_some() && sp.expire_delta {
        sp.expire_delta = false;
        return graph_error(410, "resyncChangesApplyDifferences");
    }
    let ids: Vec<String> = match token {
        None => sp.items.keys().filter(|k| k.as_str() != "root").cloned().collect(),
        Some(t) => {
            let mut v: Vec<String> = Vec::new();
            for (_, id) in sp.changes.iter().filter(|(s, _)| *s > t) {
                v.retain(|x| x != id);
                v.push(id.clone());
            }
            v
        }
    };
    let value: Vec<Value> = ids
        .iter()
        .filter_map(|id| match sp.items.get(id) {
            Some(i) => {
                // The real feed leaves out paths.
                let mut v = sp.json(i);
                if let Some(p) = v["parentReference"].as_object_mut() {
                    p.remove("path");
                }
                Some(v)
            }
            None => sp.recycle.get(id).map(|i| {
                json!({"id": i.id, "deleted": {"state": "deleted"},
                       "lastModifiedBy": identity(i.modified_by.as_deref())})
            }),
        })
        .collect();
    let link = format!("{base}/v1.0/drives/{DRIVE}/root/delta/{}", sp.seq);
    json_resp(200, json!({"value": value, "@odata.deltaLink": link}))
}

fn versions(sp: &SpState, id: &str) -> Response {
    let Some(i) = sp.items.get(id) else {
        return graph_error(404, "itemNotFound");
    };
    let mut all: Vec<&Vec<u8>> = i.history.iter().collect();
    all.push(&i.content);
    // Newest first, as Graph lists them; version "N.0" is the Nth content.
    let value: Vec<Value> = all
        .iter()
        .enumerate()
        .rev()
        .map(|(n, c)| json!({"id": format!("{}.0", n + 1), "size": c.len()}))
        .collect();
    json_resp(200, json!({"value": value}))
}

/// The bytes of version "N.0" (1 is the oldest).
fn version_content(i: &SpItem, v: &str) -> Option<Vec<u8>> {
    let n: usize = v.split('.').next()?.parse().ok()?;
    if n == i.history.len() + 1 {
        Some(i.content.clone())
    } else {
        i.history.get(n.checked_sub(1)?).cloned()
    }
}

fn restore_version(sp: &mut SpState, id: &str, v: &str) -> Response {
    let Some(content) = sp.items.get(id).and_then(|i| version_content(i, v)) else {
        return graph_error(404, "itemNotFound");
    };
    let item = sp.items.get_mut(id).unwrap();
    let old = std::mem::replace(&mut item.content, content);
    item.history.push(old);
    item.modified_by = None;
    sp.touch(id);
    StatusCode::NO_CONTENT.into_response()
}

fn invite(sp: &mut SpState, id: &str, body: &[u8]) -> Response {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let email = v["recipients"][0]["email"].as_str().unwrap_or("").to_lowercase();
    sp.invites.push(format!(
        "invite {email} requireSignIn={} sendInvitation={} roles={}",
        v["requireSignIn"], v["sendInvitation"], v["roles"]
    ));
    if !sp.items.contains_key(id) {
        return graph_error(404, "itemNotFound");
    }
    if email.is_empty() || email.ends_with("@nobody.example") {
        return graph_error(400, "invalidRequest");
    }
    let fresh = sp.id("sp-perm");
    let item = sp.items.get_mut(id).unwrap();
    let existing = item
        .perms
        .iter()
        .find(|p| p.1.as_deref() == Some(email.as_str()))
        .map(|p| p.0.clone());
    let pid = match existing {
        Some(p) => p,
        None => {
            item.perms.push((fresh.clone(), Some(email.clone()), None));
            fresh
        }
    };
    json_resp(200, json!({"value": [perm_json(&(pid, Some(email), None))]}))
}
```

(h) Add these methods to the `impl MockGraph` block in `sp.rs`:

```rust
    /// Someone edits a file in SharePoint: a new version, by that person.
    pub fn human_edit(&self, id: &str, bytes: &[u8], who: &str) {
        let mut s = self.state.lock().unwrap();
        let item = s.sp.items.get_mut(id).unwrap();
        let old = std::mem::replace(&mut item.content, bytes.to_vec());
        item.history.push(old);
        item.modified_by = Some(who.to_owned());
        s.sp.touch(id);
    }

    /// Someone renames a file or folder in SharePoint.
    pub fn human_rename(&self, id: &str, name: &str, who: &str) {
        let mut s = self.state.lock().unwrap();
        let item = s.sp.items.get_mut(id).unwrap();
        item.name = name.to_owned();
        item.modified_by = Some(who.to_owned());
        s.sp.touch(id);
    }

    /// Someone deletes an item; it goes to the recycle bin.
    pub fn human_delete(&self, id: &str, who: &str) {
        let mut s = self.state.lock().unwrap();
        let mut item = s.sp.items.remove(id).unwrap();
        item.modified_by = Some(who.to_owned());
        s.sp.recycle.insert(id.to_owned(), item);
        s.sp.touch(id);
    }

    /// A site owner restores an item from the recycle bin (same id).
    pub fn restore_from_recycle_bin(&self, id: &str) {
        let mut s = self.state.lock().unwrap();
        let item = s.sp.recycle.remove(id).unwrap();
        s.sp.items.insert(id.to_owned(), item);
        s.sp.touch(id);
    }

    /// Someone uploads their own file into a folder path. Returns its id.
    pub fn human_upload(&self, folder: &str, name: &str, bytes: &[u8], who: &str) -> String {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        let parent = s.sp.at(folder).unwrap().id.clone();
        let v = s.sp.insert(&parent, name.to_owned(), false, bytes.to_vec());
        let id = v["id"].as_str().unwrap().to_owned();
        let item = s.sp.items.get_mut(&id).unwrap();
        item.created_by = Some(who.to_owned());
        item.modified_by = Some(who.to_owned());
        id
    }

    /// A file PDMS's app put there whose save never committed. Returns its id.
    pub fn app_upload(&self, folder: &str, name: &str, bytes: &[u8]) -> String {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        let parent = s.sp.at(folder).unwrap().id.clone();
        let v = s.sp.insert(&parent, name.to_owned(), false, bytes.to_vec());
        v["id"].as_str().unwrap().to_owned()
    }

    /// A permission someone added by hand (a SharePoint group with edit rights).
    pub fn add_group_permission(&self, id: &str, group: &str) {
        let mut s = self.state.lock().unwrap();
        let pid = s.sp.id("sp-perm");
        s.sp.items.get_mut(id).unwrap().perms.push((pid, None, Some(group.to_owned())));
    }

    /// Emails with a direct permission on an item, sorted.
    pub fn readers(&self, id: &str) -> Vec<String> {
        let s = self.state.lock().unwrap();
        let mut v: Vec<String> = s.sp.items[id].perms.iter().filter_map(|p| p.1.clone()).collect();
        v.sort();
        v
    }

    pub fn content_of(&self, id: &str) -> Vec<u8> {
        self.state.lock().unwrap().sp.items[id].content.clone()
    }

    pub fn invites(&self) -> Vec<String> {
        self.state.lock().unwrap().sp.invites.clone()
    }

    pub fn expire_delta(&self) {
        self.state.lock().unwrap().sp.expire_delta = true;
    }

    /// Refuse PDMS's sign-in (every API call gets HTTP 401) until called with `false`.
    pub fn deny(&self, on: bool) {
        self.state.lock().unwrap().min_token = if on { usize::MAX } else { 0 };
    }
```

Run: `cargo test -p pdms-core --test sharepoint && cargo test -p pdms-core --test graph`
Expected: P2's and P3's tests still pass (`test result: ok.`). The mock still answers everything it answered before.

- [x] **Step 2: Write the failing tests.** In `crates/pdms-core/tests/sharepoint_ops.rs`:

- below `mod common;`, add `mod mock_graph;`;
- replace the `use` block with:

```rust
use std::sync::OnceLock;

use common::*;
use mock_graph::MockGraph;
use pdms_authz::{Permission, Scope, Subject};
use pdms_core::config::StorageBackend;
use pdms_core::graph::{GraphError, cert};
use pdms_core::model::UserStatus;
use pdms_core::sharepoint::{access, status};
use pdms_core::{Core, CoreError, acl, directory};
use uuid::Uuid;
```

Then add:

```rust
fn certificate() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| cert::generate("PDMS test", 30).unwrap().pem)
}

/// A `Core` that stores document files in the mock SharePoint (as in P3's tests).
async fn sp_core(sp: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut c = (*core.config).clone();
    c.graph = sp.config(certificate());
    c.storage.backend = StorageBackend::SharePoint;
    c.sharepoint.site_url = mock_graph::SITE_URL.into();
    c.sharepoint.upload_chunk_bytes = 327_680;
    (Core::new(core.db.clone(), c).unwrap(), db)
}

/// A department whose members may create and edit documents.
async fn department(core: &Core, code: &str, name: &str) -> Uuid {
    let dept = directory::create_department(&core.db, &system(), code, name).await.unwrap();
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(&core.db, &system(), Subject::Department(dept), Scope::Department(dept), p, None, "test")
            .await
            .unwrap();
    }
    dept
}

async fn last_error(core: &Core, name: &str) -> Option<String> {
    sqlx::query_scalar("SELECT last_error FROM sync_cursors WHERE name = $1")
        .bind(name)
        .fetch_optional(&core.db)
        .await
        .unwrap()
        .flatten()
}

#[tokio::test]
async fn department_members_get_read_access_on_their_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let bob = entra_user(&core, "bob@county.example", Some(roads)).await;
    // Local break-glass accounts never get SharePoint access.
    directory::create_user(&core.db, &system(), "breakglass@county.example", "breakglass@county.example", Some(roads))
        .await
        .unwrap();

    let r = access::reconcile(&core).await.unwrap();
    assert_eq!((r.granted, r.revoked, r.errors.len()), (2, 0, 0), "{r:?}");
    let folder = sp.item("Departments/Road and Bridge").unwrap().id;
    assert_eq!(sp.readers(&folder), vec!["alice@county.example", "bob@county.example"]);
    assert!(sp.invites().iter().any(|q| {
        q == r#"invite alice@county.example requireSignIn=true sendInvitation=false roles=["read"]"#
    }));
    assert_eq!(last_error(&core, status::RECONCILE_CURSOR).await, None);

    // A second run changes nothing.
    let again = access::reconcile(&core).await.unwrap();
    assert_eq!((again.granted, again.revoked), (0, 0));

    // Bob leaves the department; Alice leaves the county.
    directory::remove_department_member(&core.db, &system(), bob, roads).await.unwrap();
    directory::set_user_status(&core.db, &system(), alice, UserStatus::Departed).await.unwrap();
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.revoked, 2);
    assert!(sp.readers(&folder).is_empty());

    // Access someone added by hand is reported, never removed. What the folder
    // got from the site when its inheritance broke is not reported.
    let area = sp.item("Departments").unwrap().id;
    sp.add_group_permission(&area, "PDMS Test Members");
    sp.add_group_permission(&folder, "PDMS Test Members");
    sp.add_group_permission(&folder, "Road and Bridge Editors");
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.foreign, 1);
    let foreign: Vec<String> = sqlx::query_scalar("SELECT granted_to FROM sharepoint_foreign_grants")
        .fetch_all(&core.db)
        .await
        .unwrap();
    assert_eq!(foreign, vec!["Road and Bridge Editors"]);
    let acts = audit_actions(&core).await;
    for a in ["sharepoint.access_granted", "sharepoint.access_revoked"] {
        assert!(acts.contains(&a.to_owned()), "{a}");
    }
}

#[tokio::test]
async fn one_unresolvable_person_does_not_block_the_rest() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    entra_user(&core, "alice@county.example", Some(roads)).await;
    entra_user(&core, "ghost@nobody.example", Some(roads)).await;
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.granted, 1);
    assert_eq!(r.errors.len(), 1);
    assert!(r.errors[0].contains("ghost@nobody.example"), "{r:?}");
    assert!(last_error(&core, status::RECONCILE_CURSOR).await.unwrap().contains("ghost@nobody.example"));
}

#[tokio::test]
async fn folder_access_reports_sign_in_failures() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let boss = admin(&core, "admin@county.example", None).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    entra_user(&core, "alice@county.example", Some(roads)).await;
    sp.deny(true);
    assert!(access::reconcile(&core).await.is_err());
    assert_eq!(
        in_app_notices(&core).await,
        vec![(boss, "PDMS cannot sign in to Microsoft Graph".to_owned())]
    );
    sp.deny(false);
    let r = access::reconcile(&core).await.unwrap();
    assert_eq!(r.granted, 1);
    assert_eq!(last_error(&core, status::RECONCILE_CURSOR).await, None);
}

#[test]
fn plan_adds_and_removes_by_person() {
    let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let desired = [(a, "a@x".to_owned()), (b, "b@x".to_owned())].into_iter().collect();
    let granted = [(b, "perm-b".to_owned()), (c, "perm-c".to_owned())].into_iter().collect();
    let (add, remove) = access::plan(&desired, &granted);
    assert_eq!(add, vec![(a, "a@x".to_owned())]);
    assert_eq!(remove, vec![(c, "perm-c".to_owned())]);
}

#[test]
fn changes_in_one_minute_share_one_run_just_after_it() {
    assert_eq!(access::reconcile_slot(120), ("sharepoint.reconcile_access:2".to_owned(), 185));
    assert_eq!(access::reconcile_slot(179).0, "sharepoint.reconcile_access:2");
    assert_eq!(access::reconcile_slot(180).0, "sharepoint.reconcile_access:3");
}
```

Run: `cargo test -p pdms-core --test sharepoint_ops`
Expected: compile error `could not find access in sharepoint`.

- [x] **Step 3: Implement `access.rs`.** Create `crates/pdms-core/src/sharepoint/access.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Read access on `Departments/<department>/` folders (spec §7.4, D-084).
//! PDMS grants each active member of a department who came from Entra read
//! access on that department's folder, one person at a time, and removes it
//! when they leave or are deactivated. Permissions PDMS did not grant are
//! reported, never changed.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use pdms_audit::{AuditEvent, Category};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgExecutor;
use uuid::Uuid;

use super::Area;
use super::status::{self, RECONCILE_CURSOR, RunError};
use crate::Core;
use crate::context::Ctx;
use crate::error::CoreResult;
use crate::graph::{GraphClient, GraphError};
use crate::jobs::{self, NewJob};

pub const RECONCILE_JOB: &str = "sharepoint.reconcile_access";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub departments: u32,
    pub granted: u32,
    pub revoked: u32,
    pub foreign: u32,
    pub errors: Vec<String>,
}

/// Every change in one minute shares one run, scheduled 5 seconds after
/// that minute ends, so the run sees all of them. Returns (key, run at).
pub fn reconcile_slot(now: i64) -> (String, i64) {
    let bucket = now.div_euclid(60);
    (format!("{RECONCILE_JOB}:{bucket}"), (bucket + 1) * 60 + 5)
}

/// Queue a folder-access run (cheap; harmless without SharePoint).
pub async fn enqueue_reconcile<'e>(exec: impl PgExecutor<'e>) -> CoreResult<()> {
    let (key, at) = reconcile_slot(Utc::now().timestamp());
    let run_at = DateTime::from_timestamp(at, 0).unwrap_or_else(Utc::now);
    jobs::enqueue(
        exec,
        NewJob::new(RECONCILE_JOB, json!({}))
            .idempotency_key(key)
            .run_at(run_at)
            .max_attempts(6),
    )
    .await?;
    Ok(())
}

/// Who to add (user, email) and who to remove (user, permission id) on one folder.
pub fn plan(
    desired: &BTreeMap<Uuid, String>,
    granted: &BTreeMap<Uuid, String>,
) -> (Vec<(Uuid, String)>, Vec<(Uuid, String)>) {
    let add = desired
        .iter()
        .filter(|(u, _)| !granted.contains_key(u))
        .map(|(u, e)| (*u, e.clone()))
        .collect();
    let remove = granted
        .iter()
        .filter(|(u, _)| !desired.contains_key(u))
        .map(|(u, p)| (*u, p.clone()))
        .collect();
    (add, remove)
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    value: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphPermission {
    id: String,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    inherited_from: Option<Value>,
    #[serde(default)]
    granted_to_v2: Option<Value>,
    #[serde(default)]
    link: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
}

impl GraphPermission {
    fn grantee(&self) -> String {
        let g = self.granted_to_v2.as_ref();
        for k in ["user", "siteGroup", "group", "siteUser", "application"] {
            let name = g
                .and_then(|g| g.get(k))
                .and_then(|x| x.get("displayName").or_else(|| x.get("email")))
                .and_then(Value::as_str);
            if let Some(n) = name {
                return n.to_owned();
            }
        }
        if self.link.is_some() { "a sharing link".into() } else { "unknown".into() }
    }

    /// Who and with which roles, to compare a folder's access with the site's.
    fn key(&self) -> String {
        format!("{} {}", self.grantee(), self.roles.join(","))
    }
}

/// Bring every department folder's readers in line with PDMS (one run at a
/// time). A no-op when SharePoint storage is off.
pub async fn reconcile(core: &Core) -> CoreResult<ReconcileReport> {
    if core.sharepoint.is_none() {
        return Ok(ReconcileReport::default());
    }
    match run(core).await {
        Ok(r) => Ok(r),
        Err(e) => {
            status::report_failure(core, RECONCILE_CURSOR, &e).await?;
            Err(e.into())
        }
    }
}

async fn run(core: &Core) -> Result<ReconcileReport, RunError> {
    let (graph, resolved) = status::parts(core).await?;
    let drive = resolved.drive_id.clone();
    let mut lock = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pdms.sharepoint_reconcile'))")
        .execute(&mut *lock)
        .await?;
    let departments = sqlx::query!(
        r#"SELECT d.id, d.name FROM departments d
            WHERE EXISTS (SELECT 1 FROM department_members m JOIN users u ON u.id = m.user_id
                           WHERE m.department_id = d.id AND u.status = 'active' AND u.source = 'entra')
               OR EXISTS (SELECT 1 FROM sharepoint_folder_access a WHERE a.department_id = d.id)
            ORDER BY d.name"#
    )
    .fetch_all(&core.db)
    .await?;
    let mut report = ReconcileReport::default();
    // What department folders inherit from the site (its own groups). The
    // first invite on a folder breaks inheritance, and SharePoint copies these
    // onto it as direct permissions; they are not "other access".
    let area: Page<GraphPermission> =
        graph.get(&format!("/drives/{drive}/items/{}/permissions", resolved.departments_id)).await?;
    let inherited: BTreeSet<String> = area.value.iter().map(GraphPermission::key).collect();
    for d in departments {
        report.departments += 1;
        if let Err(e) = department(core, graph, &drive, &inherited, d.id, &d.name, &mut report).await {
            if e.is_auth() {
                return Err(e);
            }
            report.errors.push(format!("{}: {e}", d.name));
        }
    }
    match report.errors.first() {
        None => {
            let mut conn = core.db.acquire().await?;
            status::record_ok(&mut conn, RECONCILE_CURSOR, None).await?;
        }
        Some(first) => {
            let message = format!("{} problem(s); first: {first}", report.errors.len());
            status::record_error(&core.db, RECONCILE_CURSOR, &message).await?;
        }
    }
    lock.commit().await?;
    Ok(report)
}

async fn department(
    core: &Core,
    graph: &GraphClient,
    drive: &str,
    inherited: &BTreeSet<String>,
    department_id: Uuid,
    name: &str,
    report: &mut ReconcileReport,
) -> Result<(), RunError> {
    let folder = super::department_folder_id(core, Area::Departments, department_id).await?;
    let desired: BTreeMap<Uuid, String> = sqlx::query!(
        "SELECT u.id, u.email FROM department_members m JOIN users u ON u.id = m.user_id
          WHERE m.department_id = $1 AND u.status = 'active' AND u.source = 'entra'",
        department_id
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r.email))
    .collect();
    let granted: BTreeMap<Uuid, String> = sqlx::query!(
        "SELECT user_id, permission_id FROM sharepoint_folder_access WHERE department_id = $1",
        department_id
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| (r.user_id, r.permission_id))
    .collect();
    let (add, remove) = plan(&desired, &granted);
    let ctx = Ctx::system("sharepoint-access");
    let place = format!("Departments/{name}");

    for (user_id, email) in add {
        let body = json!({
            "recipients": [{"email": email}],
            "roles": ["read"],
            "requireSignIn": true,
            "sendInvitation": false,
        });
        let resp: Page<GraphPermission> =
            match graph.post(&format!("/drives/{drive}/items/{folder}/invite"), &body).await {
                Ok(r) => r,
                Err(e @ GraphError::Auth(_)) => return Err(e.into()),
                Err(e) => {
                    report.errors.push(format!("{name}: could not grant {email}: {e}"));
                    continue;
                }
            };
        let Some(p) = resp.value.into_iter().find(|p| p.error.is_none()) else {
            report.errors.push(format!("{name}: could not grant {email}"));
            continue;
        };
        let mut tx = core.db.begin().await?;
        sqlx::query!(
            "INSERT INTO sharepoint_folder_access (department_id, user_id, permission_id) VALUES ($1, $2, $3)
             ON CONFLICT (department_id, user_id) DO UPDATE SET permission_id = EXCLUDED.permission_id, granted_at = now()",
            department_id,
            user_id,
            p.id
        )
        .execute(&mut *tx)
        .await?;
        pdms_audit::append(
            &mut tx,
            vec![
                AuditEvent::new(Category::Permission, "sharepoint.access_granted", ctx.actor.clone(), "user")
                    .object(user_id, Some(&email))
                    .department(Some(department_id))
                    .context("folder", place.as_str())
                    .context("permission_id", p.id.as_str()),
            ],
        )
        .await?;
        tx.commit().await?;
        report.granted += 1;
    }

    for (user_id, permission) in remove {
        match graph.delete(&format!("/drives/{drive}/items/{folder}/permissions/{permission}")).await {
            Ok(()) | Err(GraphError::NotFound) => {}
            Err(e) => return Err(e.into()),
        }
        let mut tx = core.db.begin().await?;
        sqlx::query!(
            "DELETE FROM sharepoint_folder_access WHERE department_id = $1 AND user_id = $2",
            department_id,
            user_id
        )
        .execute(&mut *tx)
        .await?;
        pdms_audit::append(
            &mut tx,
            vec![
                AuditEvent::new(Category::Permission, "sharepoint.access_revoked", ctx.actor.clone(), "user")
                    .object(user_id, None)
                    .department(Some(department_id))
                    .context("folder", place.as_str())
                    .context("permission_id", permission.as_str()),
            ],
        )
        .await?;
        tx.commit().await?;
        report.revoked += 1;
    }

    // Report access PDMS did not grant.
    let mine: BTreeSet<String> = sqlx::query_scalar!(
        "SELECT permission_id FROM sharepoint_folder_access WHERE department_id = $1",
        department_id
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .collect();
    let all: Page<GraphPermission> = graph.get(&format!("/drives/{drive}/items/{folder}/permissions")).await?;
    let foreign: Vec<GraphPermission> = all
        .value
        .into_iter()
        .filter(|p| p.inherited_from.is_none() && !mine.contains(&p.id) && !inherited.contains(&p.key()))
        .collect();
    let mut tx = core.db.begin().await?;
    sqlx::query!("DELETE FROM sharepoint_foreign_grants WHERE department_id = $1", department_id)
        .execute(&mut *tx)
        .await?;
    for p in &foreign {
        sqlx::query!(
            "INSERT INTO sharepoint_foreign_grants (department_id, permission_id, granted_to, roles)
             VALUES ($1, $2, $3, $4)",
            department_id,
            p.id,
            p.grantee(),
            p.roles.join(", ")
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    report.foreign += u32::try_from(foreign.len()).unwrap_or(u32::MAX);
    Ok(())
}
```

In `sharepoint/mod.rs`, add `pub mod access;` next to `pub mod status;`, and `pub use access::RECONCILE_JOB;` with the other `pub use` lines.

- [x] **Step 4: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint_ops`
Expected: `department_members_get_read_access_on_their_folder`, `one_unresolvable_person_does_not_block_the_rest`, `folder_access_reports_sign_in_failures`, `plan_adds_and_removes_by_person`, `changes_in_one_minute_share_one_run_just_after_it`, and the Task 1 test all `... ok`.

- [x] **Step 5: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/sharepoint crates/pdms-core/tests/mock_graph crates/pdms-core/tests/sharepoint_ops.rs .sqlx
git commit -m "sharepoint: grant department staff read access on their folder"
```

---
### Task 3: Folder access follows membership changes, the Entra sync, and a nightly run

**Files:**
- Modify: `crates/pdms-core/src/directory/admin.rs` (`insert_membership`, `remove_department_member`, `set_user_status`)
- Modify: `crates/pdms-core/src/directory/sync.rs` (`reconcile_idp_memberships`)
- Modify: `crates/pdms-core/src/directory/entra_sync.rs` (`run_job`, `sync_one`; P2)
- Create: `crates/pdms-core/src/sharepoint/ops.rs`
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`, `crates/pdms-core/src/maintenance.rs`
- Test: `crates/pdms-core/tests/sharepoint_ops.rs`, `crates/pdms-core/tests/entra_sync.rs`

**Interfaces:**
- Consumes:
  - Task 2: `access::{enqueue_reconcile, reconcile, RECONCILE_JOB}`.
  - P2: `entra_sync::{run_job, sync_one, SyncReport.changed_departments}`.
  - P3: `sharepoint::register_jobs(worker, core)`, which ends `maintenance::worker()`.
- Produces: `pub fn pdms_core::sharepoint::register_ops_jobs(worker: Worker, core: &Core) -> Worker`. The `RECONCILE_JOB` handler is **always** registered: directory changes queue the job whether or not SharePoint is on, and a job kind without a handler would go dead. The nightly schedule is added only with SharePoint storage. Task 4 adds the change check here.

> **As built (2026-10-23):** as the pre-flight says, the Entra sync queues the run inside `entra_sync::set_idp_department` whenever a membership changes (which also covers `pdms sync-directory` and mapping edits), not in `run_job` and `sync_one`. `directory/sync.rs` queues it when `want_depts != have_depts`. The Entra test maps "Road and Bridge" to a department first, because a sync that changes no membership queues nothing.

- [x] **Step 1: Write the failing tests.** Append to `crates/pdms-core/tests/sharepoint_ops.rs`:

```rust
async fn queued_runs(core: &Core) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'sharepoint.reconcile_access' AND status = 'pending'",
    )
    .fetch_one(&core.db)
    .await
    .unwrap()
}

#[tokio::test]
async fn membership_changes_queue_one_folder_access_run() {
    let (core, _db) = core().await;
    let roads = directory::create_department(&core.db, &system(), "RB", "Road and Bridge").await.unwrap();
    let parks = directory::create_department(&core.db, &system(), "PK", "Parks").await.unwrap();
    assert_eq!(queued_runs(&core).await, 0, "a new, empty department needs no run");
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    directory::add_department_member(&core.db, &system(), alice, parks, false).await.unwrap();
    directory::remove_department_member(&core.db, &system(), alice, roads).await.unwrap();
    directory::set_user_status(&core.db, &system(), alice, UserStatus::Departed).await.unwrap();
    let n = queued_runs(&core).await;
    assert!((1..=2).contains(&n), "changes in the same minute share one run; got {n}");
    let first: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT min(run_at) FROM jobs WHERE kind = 'sharepoint.reconcile_access'")
            .fetch_one(&core.db)
            .await
            .unwrap();
    assert!(first > chrono::Utc::now(), "runs after the minute of the change ends");
    // Without SharePoint storage the run does nothing.
    assert_eq!(access::reconcile(&core).await.unwrap(), access::ReconcileReport::default());
}
```

Append to P2's `crates/pdms-core/tests/entra_sync.rs`:

```rust
#[tokio::test]
async fn a_successful_sync_queues_a_folder_access_run() {
    let m = MockGraph::start().await;
    m.add_user(member(OID_A, "Alice Ops", "alice@county.example", Some("Road and Bridge")));
    let (core, _db) = synced(&m).await;
    entra_sync::run_job(&core, &json!({})).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'sharepoint.reconcile_access'")
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert!(n >= 1, "{n}");
}
```

Run: `cargo test -p pdms-core --test sharepoint_ops membership_changes && cargo test -p pdms-core --test entra_sync a_successful_sync`
Expected: both FAIL. No `sharepoint.reconcile_access` jobs are queued (`assertion failed … got 0`).

- [x] **Step 2: Queue the run where memberships change.** All inserts go inside the existing transaction, before the `pdms_audit::append`.

In `crates/pdms-core/src/directory/admin.rs`:

- `insert_membership`: after the `INSERT INTO department_members … ON CONFLICT … DO UPDATE SET is_primary = …` statement's `.execute(&mut *conn).await?;`, add:

```rust
    crate::sharepoint::access::enqueue_reconcile(&mut *conn).await?;
```

- `remove_department_member`: after the `DELETE FROM department_members … RETURNING source …` statement (the one ending `.ok_or(CoreError::NotFound)?;`), add:

```rust
    crate::sharepoint::access::enqueue_reconcile(&mut *tx).await?;
```

- `set_user_status`: after the `UPDATE users SET status = $2, …` statement's `.execute(&mut *tx).await?;`, add the same line as for `remove_department_member`.

In `crates/pdms-core/src/directory/sync.rs`, `reconcile_idp_memberships`: directly after

```rust
    if before == after {
        return Ok(None);
    }
```

add:

```rust
    if before.0 != after.0 {
        crate::sharepoint::access::enqueue_reconcile(&mut *conn).await?;
    }
```

In `crates/pdms-core/src/directory/entra_sync.rs` (P2):

- `run_job`, in the `Ok(r) => { … }` arm, directly before its `Ok(())`:

```rust
            crate::sharepoint::access::enqueue_reconcile(&core.db)
                .await
                .map_err(|e| e.to_string())?;
```

- `sync_one`, directly before its final `Ok(id)`:

```rust
    if !report.changed_departments.is_empty() {
        crate::sharepoint::access::enqueue_reconcile(&core.db).await?;
    }
```

(People that `apply` departs go through `set_user_status`, which already queues the run.)

- [x] **Step 3: Register the job.** Create `crates/pdms-core/src/sharepoint/ops.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Worker jobs for SharePoint operations (P4): department-folder access and,
//! from Task 4, the change check.

use std::time::Duration;

use super::access::{self, RECONCILE_JOB};
use crate::Core;
use crate::jobs::Worker;

/// Register P4's jobs. The folder-access handler is always registered,
/// because directory changes queue it whether or not SharePoint is on (it
/// is a no-op then); the schedules only with SharePoint storage.
pub fn register_ops_jobs(worker: Worker, core: &Core) -> Worker {
    let c1 = core.clone();
    let worker = worker.handle(RECONCILE_JOB, move |_| {
        let core = c1.clone();
        async move {
            let r = access::reconcile(&core).await.map_err(|e| e.to_string())?;
            if r.granted + r.revoked > 0 || !r.errors.is_empty() {
                tracing::info!(?r, "SharePoint folder access updated");
            }
            Ok(())
        }
    });
    if core.sharepoint.is_none() {
        return worker;
    }
    worker.every(RECONCILE_JOB, Duration::from_secs(24 * 3600))
}
```

In `sharepoint/mod.rs`, add `mod ops;` with the other module lines and `pub use ops::register_ops_jobs;` with the `pub use` lines.

In `crates/pdms-core/src/maintenance.rs`, replace the last line of `worker()`, `crate::sharepoint::register_jobs(worker, core)` (added by P3), with:

```rust
    crate::sharepoint::register_ops_jobs(crate::sharepoint::register_jobs(worker, core), core)
```

- [x] **Step 4: Run the tests and confirm they pass.**

```bash
cargo test -p pdms-core --test sharepoint_ops
cargo test -p pdms-core --test entra_sync
cargo test -p pdms-core --test directory
```

Expected: all `test result: ok.` with 0 failed.

- [x] **Step 5: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src crates/pdms-core/tests/sharepoint_ops.rs crates/pdms-core/tests/entra_sync.rs .sqlx
git commit -m "sharepoint: update folder access after membership changes, the Entra sync, and nightly"
```

---
### Task 4: Catch changes made directly in SharePoint

**Files:**
- Create: `crates/pdms-core/src/sharepoint/changes.rs`
- Modify: `crates/pdms-core/src/sharepoint/status.rs` (`document_flags`, `unexpected_files`)
- Modify: `crates/pdms-core/src/sharepoint/ops.rs`, `crates/pdms-core/src/sharepoint/mod.rs`
- Test: `crates/pdms-core/src/sharepoint/changes.rs` (unit), `crates/pdms-core/tests/sharepoint_ops.rs`

**Interfaces:**
- Consumes:
  - Task 1: `status::{parts, record_ok, report_failure, notify_admins, RunError, DELTA_CURSOR}`.
  - P2: `GraphClient::{get_delta, get}`, `GraphError::{ResyncRequired, NotFound}`, `DeltaPage { items, delta_link }`.
  - P3: `sharepoint::{enqueue_mirror, MIRROR_SWEEP_JOB, Resolved}`, the tables `sharepoint_document_folders` and `sharepoint_department_folders`.
- Produces:
  - `pdms_core::sharepoint::changes::{DELTA_JOB, DeltaReport, DriveItem, Tracked, Observation, observe, display_path, detect_changes, enqueue_now}`:
    - `pub async fn detect_changes(core: &Core) -> CoreResult<DeltaReport>`, a no-op without SharePoint;
    - `pub async fn enqueue_now(db: &PgPool) -> CoreResult<()>`;
    - `DriveItem::quick_xor_hash(&self) -> Option<&str>`;
    - `DeltaReport { items, modified, missing, cleared, unexpected, mirrors: u32, resynced: bool }`;
  - `status::{document_flags(db, document_id) -> CoreResult<Vec<(i32, String)>>, unexpected_files(db) -> CoreResult<Vec<UnexpectedFile>>, UnexpectedFile}`;
  - re-export `pdms_core::sharepoint::DELTA_JOB`.

> **As built (2026-10-23), following the owner's answer 3:**
> - One audit shape: `files::changed_outside_event(document_id, item_id, source, detail)` (P3's: category system, outcome failure) is used by the download check (`source = "download_check"`) and by the change check (`"change_check"`, with "modified/deleted in SharePoint by …, version, path" in `detail`). The event names no title.
> - `files::flag_changed` now takes `&Core` (the download stream carries a `Core`), flags a file only the first time (`state <> 'changed_outside'`), sets `changed_at`, and in the same transaction sends administrators the same notice as the change check (`status::outside_changes_notice`). Repeated downloads of a changed file are no longer audited each time.
> - `Tracked` gains `changed_by`, and `observe` returns a new `Observation::Attribute` for a file a download already flagged: the change check records who changed it, with no second event or notice.
> - Extra tests: the unit test `a_file_a_download_flagged_gets_its_author_without_a_second_alarm`, and `a_change_found_by_a_download_reaches_admins_once` in `tests/sharepoint_ops.rs`.

- [x] **Step 1: Write the failing integration tests.** Append to `crates/pdms-core/tests/sharepoint_ops.rs`, and extend its imports with:

```rust
use pdms_authz::Visibility;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::sharepoint::changes;
```

```rust
/// A document with one file in the mock SharePoint: (document, file version id, SharePoint item id).
async fn stored_document(core: &Core, owner: Uuid, dept: Uuid, title: &str, bytes: &[u8]) -> (Uuid, Uuid, String) {
    let t = types::type_by_key(&core.db, "contract").await.unwrap().id;
    let id = documents::create(
        core,
        &system(),
        NewDocument {
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: t,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file: Some(upload(core, "agreement.pdf", bytes).await),
            note: String::new(),
            visibility: Visibility::Department,
        },
    )
    .await
    .unwrap();
    let (version, item): (Uuid, String) =
        sqlx::query_as("SELECT document_version_id, item_id FROM sharepoint_files WHERE document_id = $1")
            .bind(id)
            .fetch_one(&core.db)
            .await
            .unwrap();
    (id, version, item)
}

async fn file_state(core: &Core, version: Uuid) -> (String, Option<String>) {
    sqlx::query_as("SELECT state, changed_by FROM sharepoint_files WHERE document_version_id = $1")
        .bind(version)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn outside_changes_are_flagged_and_admins_told() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let boss = admin(&core, "admin@county.example", None).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (fuel, fuel_v1, fuel_item) = stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    let (_lease, lease_v1, lease_item) = stored_document(&core, alice, roads, "Office Lease", &pdf("lease")).await;

    // The first run reads everything; PDMS's own files and folders are not changes.
    let first = changes::detect_changes(&core).await.unwrap();
    assert_eq!((first.modified, first.missing, first.unexpected), (0, 0, 0), "{first:?}");

    sp.human_edit(&fuel_item, &pdf("fuel, edited by hand"), "Pat Clerk");
    sp.human_delete(&lease_item, "Pat Clerk");
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!((r.modified, r.missing), (1, 1), "{r:?}");
    assert_eq!(
        file_state(&core, fuel_v1).await,
        ("changed_outside".to_owned(), Some("Pat Clerk <pat.clerk@county.example>".to_owned()))
    );
    assert_eq!(file_state(&core, lease_v1).await.0, "missing");
    assert!(audit_actions(&core).await.contains(&"sharepoint.changed_outside".to_owned()));
    assert_eq!(
        in_app_notices(&core).await,
        vec![(boss, "SharePoint: 2 file(s) changed outside PDMS".to_owned())]
    );
    assert_eq!(
        status::document_flags(&core.db, fuel).await.unwrap(),
        vec![(1, "changed_outside".to_owned())]
    );

    // Nothing new happened: no new flags, no new notice.
    let quiet = changes::detect_changes(&core).await.unwrap();
    assert_eq!((quiet.modified, quiet.missing), (0, 0));
    assert_eq!(in_app_notices(&core).await.len(), 1);
    assert_eq!(last_error(&core, status::DELTA_CURSOR).await, None);
}

#[tokio::test]
async fn untracked_files_are_listed_after_ten_minutes_and_never_deleted() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();

    let stray = sp.human_upload("Departments/Road and Bridge", "notes.docx", b"hand-made notes", "Pat Clerk");
    sp.app_upload("Departments/Road and Bridge", "unfinished.pdf", &pdf("orphan"));
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!(r.unexpected, 2, "{r:?}");
    assert!(status::unexpected_files(&core.db).await.unwrap().is_empty(), "not shown for ten minutes");

    sqlx::query("UPDATE sharepoint_unexpected_items SET first_seen = now() - interval '11 minutes'")
        .execute(&core.db)
        .await
        .unwrap();
    let mut seen: Vec<(String, String, bool)> = status::unexpected_files(&core.db)
        .await
        .unwrap()
        .into_iter()
        .map(|u| (u.path, u.created_by, u.by_pdms))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("Departments/Road and Bridge/notes.docx".into(), "Pat Clerk <pat.clerk@county.example>".into(), false),
            ("Departments/Road and Bridge/unfinished.pdf".into(), "PDMS".into(), true),
        ]
    );
    assert!(sp.item("Departments/Road and Bridge/notes.docx").is_some(), "PDMS never deletes it");

    // Deleted in SharePoint: forgotten.
    sp.human_delete(&stray, "Pat Clerk");
    changes::detect_changes(&core).await.unwrap();
    assert_eq!(status::unexpected_files(&core.db).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_upload_in_progress_is_not_reported() {
    let sp = MockGraph::start().await;
    let (core, db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (_doc, _v, item) = stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    // The feed sees the file before its upload's transaction commits.
    sqlx::query("UPDATE sharepoint_files SET item_id = 'not-yet-' || item_id WHERE item_id = $1")
        .bind(&item)
        .execute(&db.owner)
        .await
        .unwrap();
    assert_eq!(changes::detect_changes(&core).await.unwrap().unexpected, 1);
    // The transaction commits.
    sqlx::query("UPDATE sharepoint_files SET item_id = $1 WHERE item_id = 'not-yet-' || $1")
        .bind(&item)
        .execute(&db.owner)
        .await
        .unwrap();
    changes::detect_changes(&core).await.unwrap();
    sqlx::query("UPDATE sharepoint_unexpected_items SET first_seen = now() - interval '11 minutes'")
        .execute(&core.db)
        .await
        .unwrap();
    assert!(status::unexpected_files(&core.db).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_expired_change_token_rereads_without_false_alarms() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    sp.expire_delta();
    let r = changes::detect_changes(&core).await.unwrap();
    assert!(r.resynced);
    assert_eq!((r.modified, r.missing, r.unexpected), (0, 0, 0), "{r:?}");
    assert_eq!(last_error(&core, status::DELTA_CURSOR).await, None);
}

#[tokio::test]
async fn renamed_files_and_folders_get_pdms_layout_back() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (doc, _v, item) = stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    sp.human_rename(&item, "renamed by hand.pdf", "Pat Clerk");
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!((r.mirrors, r.modified), (1, 0), "a rename is not a content change: {r:?}");
    let folder: String = sqlx::query_scalar("SELECT item_id FROM sharepoint_document_folders WHERE document_id = $1")
        .bind(doc)
        .fetch_one(&core.db)
        .await
        .unwrap();
    sp.human_rename(&folder, "Fuel (old)", "Pat Clerk");
    assert_eq!(changes::detect_changes(&core).await.unwrap().mirrors, 1);
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'sharepoint.mirror_document' AND status = 'pending'
           AND payload->>'document_id' = $1",
    )
    .bind(doc.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(pending, 1, "one pending mirror job for the document");

    // An area folder in the feed is not a change unless it was renamed.
    let area = sp.item("Departments").unwrap().id;
    sp.human_rename(&area, "Departments", "Pat Clerk");
    assert_eq!(changes::detect_changes(&core).await.unwrap().mirrors, 0);
    sp.human_rename(&area, "Departments (old)", "Pat Clerk");
    assert_eq!(changes::detect_changes(&core).await.unwrap().mirrors, 1, "a renamed area folder queues a sweep");
}
```

Run: `cargo test -p pdms-core --test sharepoint_ops`
Expected: compile error `could not find changes in sharepoint`.

- [x] **Step 2: Implement `changes.rs` with its unit tests.** Create `crates/pdms-core/src/sharepoint/changes.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Changes made directly in SharePoint (spec §7.6, D-089). Every few minutes
//! PDMS reads the library's change feed (Graph delta) and compares each file
//! it tracks with what it recorded at upload: size and QuickXorHash. A file
//! that differs, or is gone, is flagged, audited, and reported to
//! administrators. Who made the change is recorded but never trusted: the
//! hashes decide. Files PDMS does not track are listed for review and never
//! deleted.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::Utc;
use pdms_audit::{AuditEvent, Category};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::status::{self, DELTA_CURSOR, RunError};
use super::{Area, MIRROR_SWEEP_JOB};
use crate::Core;
use crate::context::Ctx;
use crate::error::CoreResult;
use crate::graph::GraphError;
use crate::jobs::{self, NewJob};
use crate::notifications::actions::Notice;

pub const DELTA_JOB: &str = "sharepoint.delta";

/// Properties read from the feed and from single items.
const SELECT: &str = "id,name,size,file,folder,deleted,root,parentReference,lastModifiedBy,createdBy";

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveItem {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub file: Option<FileFacet>,
    #[serde(default)]
    pub folder: Option<Value>,
    #[serde(default)]
    pub deleted: Option<Value>,
    #[serde(default)]
    pub root: Option<Value>,
    #[serde(default)]
    pub parent_reference: Option<ParentReference>,
    #[serde(default)]
    pub last_modified_by: Option<IdentitySet>,
    #[serde(default)]
    pub created_by: Option<IdentitySet>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct FileFacet {
    #[serde(default)]
    pub hashes: Option<Hashes>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hashes {
    #[serde(default)]
    pub quick_xor_hash: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ParentReference {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct IdentitySet {
    #[serde(default)]
    pub application: Option<Identity>,
    #[serde(default)]
    pub user: Option<Identity>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl IdentitySet {
    /// Written by PDMS's own app registration.
    pub fn is_app(&self, client_id: &str) -> bool {
        self.application
            .as_ref()
            .and_then(|a| a.id.as_deref())
            .is_some_and(|id| id.eq_ignore_ascii_case(client_id))
    }

    /// A readable name for the audit log and the status panel.
    pub fn describe(&self) -> String {
        let user = self.user.as_ref();
        let name = user.and_then(|u| u.display_name.clone());
        let email = user.and_then(|u| u.email.clone());
        match (name, email) {
            (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
            (Some(n), _) => n,
            (None, Some(e)) => e,
            (None, None) => self
                .application
                .as_ref()
                .and_then(|a| a.display_name.clone())
                .unwrap_or_else(|| "unknown".into()),
        }
    }
}

impl DriveItem {
    pub fn quick_xor_hash(&self) -> Option<&str> {
        self.file.as_ref()?.hashes.as_ref()?.quick_xor_hash.as_deref()
    }

    fn modified_by_pdms(&self, client_id: &str) -> bool {
        self.last_modified_by.as_ref().is_some_and(|w| w.is_app(client_id))
    }

    fn modifier(&self) -> String {
        self.last_modified_by
            .as_ref()
            .map_or_else(|| "unknown".into(), IdentitySet::describe)
    }
}

/// What PDMS recorded about a file it put in SharePoint.
#[derive(Clone, Debug)]
pub struct Tracked {
    pub version_id: Uuid,
    pub document_id: Uuid,
    pub quick_xor_hash: String,
    pub size: i64,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// A tracked file now differs from PDMS's record, or is gone.
    Flag { version_id: Uuid, document_id: Uuid, by: String, missing: bool },
    /// A flagged file matches PDMS's record again.
    Cleared { version_id: Uuid, document_id: Uuid },
    /// A person renamed, moved, or relabelled a tracked file without changing
    /// its content: put PDMS's name, place, and columns back.
    Touched { document_id: Uuid },
    /// A file PDMS does not track.
    Unexpected { item_id: String, name: String, path: String, by: String, by_pdms: bool },
    /// A file PDMS does not track is gone.
    Forget { item_id: String },
    /// A person renamed or moved a folder.
    FolderChanged { item_id: String },
    Ignore,
}

/// What one change-feed entry means. Pure: the caller supplies PDMS's record.
pub fn observe(item: &DriveItem, tracked: Option<&Tracked>, client_id: &str) -> Observation {
    if item.root.is_some() {
        return Observation::Ignore;
    }
    if item.deleted.is_some() {
        return match tracked {
            Some(t) if t.state == "missing" => Observation::Ignore,
            Some(t) => Observation::Flag {
                version_id: t.version_id,
                document_id: t.document_id,
                by: item.modifier(),
                missing: true,
            },
            None => Observation::Forget { item_id: item.id.clone() },
        };
    }
    if item.folder.is_some() {
        return if item.modified_by_pdms(client_id) {
            Observation::Ignore
        } else {
            Observation::FolderChanged { item_id: item.id.clone() }
        };
    }
    let Some(t) = tracked else {
        let by_pdms = item.created_by.as_ref().is_some_and(|w| w.is_app(client_id));
        return Observation::Unexpected {
            item_id: item.id.clone(),
            name: item.name.clone().unwrap_or_default(),
            path: String::new(),
            by: if by_pdms {
                "PDMS".into()
            } else {
                item.created_by.as_ref().map_or_else(|| "unknown".into(), IdentitySet::describe)
            },
            by_pdms,
        };
    };
    let same = item.size == Some(t.size) && item.quick_xor_hash() == Some(t.quick_xor_hash.as_str());
    match (same, t.state.as_str()) {
        (false, "changed_outside") => Observation::Ignore,
        (false, _) => Observation::Flag {
            version_id: t.version_id,
            document_id: t.document_id,
            by: item.modifier(),
            missing: false,
        },
        (true, "ok") if item.modified_by_pdms(client_id) => Observation::Ignore,
        (true, "ok") => Observation::Touched { document_id: t.document_id },
        (true, _) => Observation::Cleared { version_id: t.version_id, document_id: t.document_id },
    }
}

/// `/drives/{id}/root:/Departments/X` (or `/drive/root:/…`) and a name → `Departments/X/name`.
pub fn display_path(parent_path: Option<&str>, name: &str) -> String {
    let folder = parent_path
        .and_then(|p| p.split_once("root:").map(|(_, rest)| rest.trim_matches('/')))
        .unwrap_or("");
    if folder.is_empty() { name.to_owned() } else { format!("{folder}/{name}") }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeltaReport {
    pub items: u32,
    pub modified: u32,
    pub missing: u32,
    pub cleared: u32,
    pub unexpected: u32,
    pub mirrors: u32,
    pub resynced: bool,
}

/// Ask the worker to check for changes now (the status panel's button).
pub async fn enqueue_now(db: &PgPool) -> CoreResult<()> {
    let key = format!("{DELTA_JOB}:manual:{}", Utc::now().timestamp().div_euclid(60));
    jobs::enqueue(db, NewJob::new(DELTA_JOB, json!({})).idempotency_key(key).max_attempts(3)).await?;
    Ok(())
}

/// Read the change feed since the last run and act on it (one run at a
/// time). A no-op when SharePoint storage is off.
pub async fn detect_changes(core: &Core) -> CoreResult<DeltaReport> {
    if core.sharepoint.is_none() {
        return Ok(DeltaReport::default());
    }
    match run(core).await {
        Ok(r) => Ok(r),
        Err(e) => {
            status::report_failure(core, DELTA_CURSOR, &e).await?;
            Err(e.into())
        }
    }
}

/// Documents laid out inside a folder: the document folder itself, or every
/// document folder in a department folder.
async fn documents_in_folder(db: &PgPool, item_id: &str) -> CoreResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT document_id AS "document_id!" FROM sharepoint_document_folders WHERE item_id = $1
           UNION
           SELECT f.document_id FROM sharepoint_document_folders f
             JOIN sharepoint_department_folders d ON d.item_id = f.parent_item_id
            WHERE d.item_id = $1"#,
        item_id
    )
    .fetch_all(db)
    .await?)
}

async fn run(core: &Core) -> Result<DeltaReport, RunError> {
    let (graph, resolved) = status::parts(core).await?;
    let drive = resolved.drive_id.clone();
    let client_id = core.config.graph.client_id.clone();
    let mut lock = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pdms.sharepoint_delta'))")
        .execute(&mut *lock)
        .await?;

    let fresh = format!("/drives/{drive}/root/delta?$select={SELECT}");
    let cursor: Option<String> =
        sqlx::query_scalar!("SELECT delta_link FROM sync_cursors WHERE name = $1", DELTA_CURSOR)
            .fetch_optional(&core.db)
            .await?
            .flatten();
    let mut report = DeltaReport::default();
    let page = match graph.get_delta::<DriveItem>(cursor.as_deref().unwrap_or(&fresh)).await {
        Err(GraphError::ResyncRequired) => {
            report.resynced = true;
            graph.get_delta::<DriveItem>(&fresh).await?
        }
        other => other?,
    };
    report.items = u32::try_from(page.items.len()).unwrap_or(u32::MAX);
    // An item can appear more than once; the last occurrence is current.
    let mut latest: BTreeMap<String, DriveItem> = BTreeMap::new();
    for it in page.items {
        latest.insert(it.id.clone(), it);
    }
    let ids: Vec<String> = latest.keys().cloned().collect();
    let tracked: HashMap<String, Tracked> = sqlx::query!(
        "SELECT item_id, document_version_id, document_id, quick_xor_hash, size, state
           FROM sharepoint_files WHERE item_id = ANY($1)",
        &ids
    )
    .fetch_all(&core.db)
    .await?
    .into_iter()
    .map(|r| {
        let t = Tracked {
            version_id: r.document_version_id,
            document_id: r.document_id,
            quick_xor_hash: r.quick_xor_hash,
            size: r.size,
            state: r.state,
        };
        (r.item_id, t)
    })
    .collect();

    // Read everything needed from Graph before opening the transaction.
    let mut observations = Vec::new();
    let mut folders = Vec::new();
    for (id, mut item) in latest {
        let t = tracked.get(&id);
        if t.is_some() && item.deleted.is_none() && item.quick_xor_hash().is_none() {
            // The feed may leave out hashes; read the file itself.
            match graph.get::<DriveItem>(&format!("/drives/{drive}/items/{id}?$select={SELECT}")).await {
                Ok(full) => item = full,
                Err(GraphError::NotFound) => item.deleted = Some(json!({})),
                Err(e) => return Err(e.into()),
            }
        }
        match observe(&item, t, &client_id) {
            Observation::Unexpected { item_id, name, by, by_pdms, .. } => {
                // The feed leaves out folder paths; read the item for its location.
                let url = format!("/drives/{drive}/items/{item_id}?$select=id,name,parentReference");
                let path = match graph.get::<DriveItem>(&url).await {
                    Ok(full) => display_path(full.parent_reference.as_ref().and_then(|p| p.path.as_deref()), &name),
                    Err(GraphError::NotFound) => continue,
                    Err(e) => return Err(e.into()),
                };
                observations.push(Observation::Unexpected { item_id, name, path, by, by_pdms });
            }
            Observation::FolderChanged { item_id } => folders.push((item_id, item.name.clone().unwrap_or_default())),
            o => observations.push(o),
        }
    }
    let mut mirror: BTreeSet<Uuid> = BTreeSet::new();
    let mut sweep = false;
    for (f, name) in folders {
        match Area::ALL.into_iter().find(|a| resolved.area_id(*a) == f) {
            // The feed can list an area folder whenever something inside it
            // changes, and people may have created it by hand (setup guide).
            // Only a rename matters.
            Some(area) => sweep |= name != area.folder_name(),
            None => mirror.extend(documents_in_folder(&core.db, &f).await?),
        }
    }

    let ctx = Ctx::system("sharepoint-changes");
    let mut events = Vec::new();
    let mut tx = core.db.begin().await?;
    for o in observations {
        match o {
            Observation::Flag { version_id, document_id, by, missing } => {
                let state = if missing { "missing" } else { "changed_outside" };
                let r = sqlx::query!(
                    r#"UPDATE sharepoint_files f
                          SET state = $2, changed_by = $3, changed_at = now(), checked_at = now()
                         FROM document_versions v, documents d
                        WHERE f.document_version_id = $1 AND v.id = f.document_version_id AND d.id = v.document_id
                    RETURNING v.version_no AS "version_no!", d.external_id AS "external_id!", d.title AS "title!",
                              d.department_id AS "department_id!", f.item_id AS "item_id!",
                              f.last_known_path AS "path!""#,
                    version_id,
                    state,
                    by.as_str()
                )
                .fetch_one(&mut *tx)
                .await?;
                events.push(
                    AuditEvent::new(Category::Content, "sharepoint.changed_outside", ctx.actor.clone(), "document")
                        .object(document_id, Some(&format!("{} {}", r.external_id, r.title)))
                        .document(document_id)
                        .department(Some(r.department_id))
                        .context("version", r.version_no)
                        .context("change", if missing { "deleted" } else { "modified" })
                        .context("changed_by", by.as_str())
                        .context("item_id", r.item_id)
                        .context("path", r.path),
                );
                if missing {
                    report.missing += 1;
                } else {
                    report.modified += 1;
                }
            }
            Observation::Cleared { version_id, document_id } => {
                let r = sqlx::query!(
                    r#"UPDATE sharepoint_files f
                          SET state = 'ok', changed_by = NULL, changed_at = NULL, checked_at = now()
                         FROM document_versions v, documents d
                        WHERE f.document_version_id = $1 AND v.id = f.document_version_id AND d.id = v.document_id
                    RETURNING v.version_no AS "version_no!", d.external_id AS "external_id!", d.title AS "title!",
                              d.department_id AS "department_id!""#,
                    version_id
                )
                .fetch_one(&mut *tx)
                .await?;
                events.push(
                    AuditEvent::new(Category::Content, "sharepoint.change_resolved", ctx.actor.clone(), "document")
                        .object(document_id, Some(&format!("{} {}", r.external_id, r.title)))
                        .document(document_id)
                        .department(Some(r.department_id))
                        .context("version", r.version_no)
                        .context("resolution", "matches_again"),
                );
                report.cleared += 1;
                mirror.insert(document_id);
            }
            Observation::Touched { document_id } => {
                mirror.insert(document_id);
            }
            Observation::Unexpected { item_id, name, path, by, by_pdms } => {
                sqlx::query!(
                    "INSERT INTO sharepoint_unexpected_items (item_id, name, path, created_by, by_pdms)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (item_id) DO UPDATE SET name = EXCLUDED.name, path = EXCLUDED.path, last_seen = now()",
                    item_id,
                    name,
                    path,
                    by,
                    by_pdms
                )
                .execute(&mut *tx)
                .await?;
                report.unexpected += 1;
            }
            Observation::Forget { item_id } => {
                sqlx::query!("DELETE FROM sharepoint_unexpected_items WHERE item_id = $1", item_id)
                    .execute(&mut *tx)
                    .await?;
            }
            Observation::FolderChanged { .. } | Observation::Ignore => {}
        }
    }
    for document_id in &mirror {
        super::enqueue_mirror(&mut *tx, *document_id).await?;
    }
    if sweep {
        let key = format!("{MIRROR_SWEEP_JOB}:changes:{}", Uuid::now_v7());
        jobs::enqueue(&mut *tx, NewJob::new(MIRROR_SWEEP_JOB, json!({})).idempotency_key(key).max_attempts(3))
            .await?;
    }
    report.mirrors = u32::try_from(mirror.len()).unwrap_or(u32::MAX) + u32::from(sweep);
    // Files that were untracked only because their upload had not committed yet.
    sqlx::query!("DELETE FROM sharepoint_unexpected_items u USING sharepoint_files f WHERE f.item_id = u.item_id")
        .execute(&mut *tx)
        .await?;
    status::record_ok(&mut tx, DELTA_CURSOR, Some(&page.delta_link)).await?;
    let flagged = report.modified + report.missing;
    if flagged > 0 {
        status::notify_admins(
            &mut tx,
            core,
            Notice {
                subject: format!("SharePoint: {flagged} file(s) changed outside PDMS"),
                body: "Someone edited or deleted files directly in the PDMS SharePoint site. \
                       Review them under Administration → SharePoint."
                    .into(),
                link: "/admin/sharepoint".into(),
                document_id: None,
                appointment_id: None,
                dedupe_key: format!("sharepoint-changes:{}", Uuid::now_v7()),
            },
        )
        .await?;
    }
    if !events.is_empty() {
        pdms_audit::append(&mut tx, events).await?;
    }
    tx.commit().await?;
    lock.commit().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    const APP: &str = "client-1";

    fn item(v: Value) -> DriveItem {
        serde_json::from_value(v).unwrap()
    }

    fn file(hash: &str, size: i64, by_app: bool) -> DriveItem {
        let by = if by_app {
            json!({"application": {"id": APP}})
        } else {
            json!({"user": {"displayName": "Pat Clerk", "email": "pat@county.example"}})
        };
        item(json!({"id": "f1", "name": "a.pdf", "size": size,
                    "file": {"hashes": {"quickXorHash": hash}},
                    "lastModifiedBy": by, "createdBy": by}))
    }

    fn tracked(state: &str) -> Tracked {
        Tracked {
            version_id: Uuid::from_u128(1),
            document_id: Uuid::from_u128(2),
            quick_xor_hash: "AAA=".into(),
            size: 10,
            state: state.into(),
        }
    }

    #[test]
    fn a_file_changed_by_a_person_is_flagged() {
        assert_eq!(
            observe(&file("BBB=", 12, false), Some(&tracked("ok")), APP),
            Observation::Flag {
                version_id: Uuid::from_u128(1),
                document_id: Uuid::from_u128(2),
                by: "Pat Clerk <pat@county.example>".into(),
                missing: false,
            }
        );
    }

    #[test]
    fn the_hash_decides_not_the_author() {
        assert!(matches!(
            observe(&file("BBB=", 10, true), Some(&tracked("ok")), APP),
            Observation::Flag { missing: false, .. }
        ));
        assert_eq!(observe(&file("AAA=", 10, true), Some(&tracked("ok")), APP), Observation::Ignore);
        assert_eq!(
            observe(&file("AAA=", 10, false), Some(&tracked("ok")), APP),
            Observation::Touched { document_id: Uuid::from_u128(2) }
        );
    }

    #[test]
    fn flags_are_raised_once_and_cleared_when_the_file_matches_again() {
        assert_eq!(
            observe(&file("BBB=", 12, false), Some(&tracked("changed_outside")), APP),
            Observation::Ignore
        );
        assert_eq!(
            observe(&file("AAA=", 10, false), Some(&tracked("changed_outside")), APP),
            Observation::Cleared { version_id: Uuid::from_u128(1), document_id: Uuid::from_u128(2) }
        );
    }

    #[test]
    fn deletions() {
        let gone = item(json!({"id": "f1", "deleted": {"state": "deleted"}}));
        assert!(matches!(observe(&gone, Some(&tracked("ok")), APP), Observation::Flag { missing: true, .. }));
        assert_eq!(observe(&gone, Some(&tracked("missing")), APP), Observation::Ignore);
        assert_eq!(observe(&gone, None, APP), Observation::Forget { item_id: "f1".into() });
    }

    #[test]
    fn untracked_files_and_folders() {
        assert!(matches!(
            observe(&file("AAA=", 10, false), None, APP),
            Observation::Unexpected { by_pdms: false, .. }
        ));
        assert!(matches!(
            observe(&file("AAA=", 10, true), None, APP),
            Observation::Unexpected { by_pdms: true, .. }
        ));
        let by_app = item(json!({"id": "d1", "name": "X", "folder": {}, "lastModifiedBy": {"application": {"id": APP}}}));
        let by_person = item(json!({"id": "d1", "name": "X", "folder": {}, "lastModifiedBy": {"user": {"displayName": "Pat"}}}));
        assert_eq!(observe(&by_app, None, APP), Observation::Ignore);
        assert_eq!(observe(&by_person, None, APP), Observation::FolderChanged { item_id: "d1".into() });
        let root = item(json!({"id": "root", "root": {}, "folder": {}}));
        assert_eq!(observe(&root, None, APP), Observation::Ignore);
    }

    #[test]
    fn paths_drop_the_drive_prefix() {
        assert_eq!(
            display_path(Some("/drives/b!x/root:/Departments/Road and Bridge"), "a.pdf"),
            "Departments/Road and Bridge/a.pdf"
        );
        assert_eq!(display_path(Some("/drive/root:"), "a.pdf"), "a.pdf");
        assert_eq!(display_path(None, "a.pdf"), "a.pdf");
    }
}
```

In `sharepoint/mod.rs`, add `pub mod changes;` and `pub use changes::DELTA_JOB;`.

- [x] **Step 3: Add the two status queries.** Append to `crates/pdms-core/src/sharepoint/status.rs` (and add `use chrono::DateTime;` and `use serde::Serialize;` to its imports):

```rust
/// Unresolved SharePoint flags on one document's files: (version number, state).
pub async fn document_flags(db: &PgPool, document_id: Uuid) -> CoreResult<Vec<(i32, String)>> {
    Ok(sqlx::query!(
        "SELECT v.version_no, f.state FROM sharepoint_files f
           JOIN document_versions v ON v.id = f.document_version_id
          WHERE f.document_id = $1 AND f.state <> 'ok' ORDER BY v.version_no",
        document_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| (r.version_no, r.state))
    .collect())
}

#[derive(Clone, Debug, Serialize)]
pub struct UnexpectedFile {
    pub item_id: String,
    pub name: String,
    pub path: String,
    pub created_by: String,
    pub by_pdms: bool,
    pub first_seen: DateTime<Utc>,
}

/// Files nobody tracks, seen more than ten minutes ago and not dismissed.
pub async fn unexpected_files(db: &PgPool) -> CoreResult<Vec<UnexpectedFile>> {
    Ok(sqlx::query_as!(
        UnexpectedFile,
        "SELECT item_id, name, path, created_by, by_pdms, first_seen FROM sharepoint_unexpected_items
          WHERE dismissed_at IS NULL AND first_seen < now() - interval '10 minutes'
          ORDER BY first_seen DESC LIMIT 500"
    )
    .fetch_all(db)
    .await?)
}
```

- [x] **Step 4: Schedule the check.** Replace `crates/pdms-core/src/sharepoint/ops.rs` from `use super::access::{self, RECONCILE_JOB};` to the end with:

```rust
use super::access::{self, RECONCILE_JOB};
use super::changes::{self, DELTA_JOB};
use crate::Core;
use crate::jobs::Worker;

/// Register P4's jobs. Both handlers are always registered, because
/// directory changes and the status panel queue them whether or not
/// SharePoint is on (they are no-ops then); the schedules only with
/// SharePoint storage.
pub fn register_ops_jobs(worker: Worker, core: &Core) -> Worker {
    let (c1, c2) = (core.clone(), core.clone());
    let worker = worker
        .handle(RECONCILE_JOB, move |_| {
            let core = c1.clone();
            async move {
                let r = access::reconcile(&core).await.map_err(|e| e.to_string())?;
                if r.granted + r.revoked > 0 || !r.errors.is_empty() {
                    tracing::info!(?r, "SharePoint folder access updated");
                }
                Ok(())
            }
        })
        .handle(DELTA_JOB, move |_| {
            let core = c2.clone();
            async move {
                let r = changes::detect_changes(&core).await.map_err(|e| e.to_string())?;
                if r.modified + r.missing + r.unexpected > 0 || r.resynced {
                    tracing::info!(?r, "SharePoint changes checked");
                }
                Ok(())
            }
        });
    if core.sharepoint.is_none() {
        return worker;
    }
    let every = Duration::from_secs(core.config.sharepoint.delta_interval_minutes.max(1) * 60);
    worker
        .every(RECONCILE_JOB, Duration::from_secs(24 * 3600))
        .every(DELTA_JOB, every)
}
```

- [x] **Step 5: Run the tests and confirm they pass.**

```bash
cargo test -p pdms-core --lib sharepoint::changes
cargo test -p pdms-core --test sharepoint_ops
```

Expected: six unit tests and every integration test `... ok`.

- [x] **Step 6: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/sharepoint crates/pdms-core/tests/sharepoint_ops.rs .sqlx
git commit -m "sharepoint: flag files changed outside PDMS and list untracked files"
```

---
### Task 5: Administrators resolve what the check found

**Files:**
- Create: `crates/pdms-core/src/sharepoint/resolve.rs`
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Test: `crates/pdms-core/tests/sharepoint_ops.rs`

**Interfaces:**
- Consumes:
  - Task 1: `status::parts`, `Capability::ManageSharePoint`.
  - Task 4: `changes::DriveItem`.
  - Existing: `documents::add_version(core, ctx, id, Upload, VersionKind, note, milestone_date)`, `pdms_storage::spool::{SpoolWriter, sanitize_file_name}`.
  - P2: `GraphClient::{get, post, download}`. Note that `post` returns `Value::Null` for an empty 204 body.
- Produces `pdms_core::sharepoint::resolve::{restore_original, accept_as_new_version, recheck, dismiss_unexpected}`:
  - `pub async fn restore_original(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<()>`;
  - `pub async fn accept_as_new_version(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<i32>` (the new version number);
  - `pub async fn recheck(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<bool>` (true when the file matches again);
  - `pub async fn dismiss_unexpected(core: &Core, ctx: &Ctx, item_id: &str) -> CoreResult<()>`.

> **As built (2026-10-23):** every Microsoft Graph call in `resolve.rs` maps its failure through `sharepoint::unavailable(MOVE_UNAVAILABLE, …)`, so an outage shows "…so the change was not made" instead of an error page (D-102); a SharePoint outage while `status::parts` resolves the library is mapped the same way. The tests reuse the `read_all` helper from Task 4 (it returns a `Result`). Extra test: `a_sharepoint_outage_while_resolving_changes_nothing`.

- [x] **Step 1: Write the failing tests.** Add `use pdms_core::sharepoint::resolve;` to the imports of `crates/pdms-core/tests/sharepoint_ops.rs`, and append:

```rust
async fn read_all(mut s: pdms_storage::ByteStream) -> Vec<u8> {
    use futures_util::StreamExt;
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c.unwrap());
    }
    out
}

/// A department, an administrator who is a member (so they may edit), the
/// document owner, and one document whose file someone edited in SharePoint.
async fn edited_in_sharepoint(sp: &MockGraph, core: &Core, admin_in_department: bool) -> (Uuid, Uuid, Uuid, String, Vec<u8>) {
    let roads = department(core, "RB", "Road and Bridge").await;
    let boss = admin(core, "admin@county.example", admin_in_department.then_some(roads)).await;
    let alice = entra_user(core, "alice@county.example", Some(roads)).await;
    let original = pdf("fuel at 2026 rates");
    let (doc, v1, item) = stored_document(core, alice, roads, "Fuel Supply", &original).await;
    changes::detect_changes(core).await.unwrap();
    sp.human_edit(&item, &pdf("fuel at 2027 rates"), "Pat Clerk");
    changes::detect_changes(core).await.unwrap();
    assert_eq!(file_state(core, v1).await.0, "changed_outside");
    (boss, doc, v1, item, original)
}

#[tokio::test]
async fn restore_original_puts_the_recorded_file_back() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (boss, doc, v1, item, original) = edited_in_sharepoint(&sp, &core, true).await;
    let owner: Uuid = sqlx::query_scalar("SELECT owner_id FROM documents WHERE id = $1")
        .bind(doc)
        .fetch_one(&core.db)
        .await
        .unwrap();
    // Only administrators resolve, not the document's owner.
    let owner_ctx = as_user(&core, owner).await;
    assert!(matches!(resolve::restore_original(&core, &owner_ctx, v1).await, Err(CoreError::Forbidden(_))));
    let ctx = as_user(&core, boss).await;
    resolve::restore_original(&core, &ctx, v1).await.unwrap();
    assert_eq!(sp.content_of(&item), original);
    assert_eq!(file_state(&core, v1).await, ("ok".to_owned(), None));
    assert!(audit_actions(&core).await.contains(&"sharepoint.change_resolved".to_owned()));
    // The restore was PDMS's own change: the next check finds nothing.
    let r = changes::detect_changes(&core).await.unwrap();
    assert_eq!((r.modified, r.missing, r.cleared), (0, 0, 0), "{r:?}");
}

#[tokio::test]
async fn accept_as_new_version_keeps_the_change_and_repairs_history() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (boss, doc, v1, item, original) = edited_in_sharepoint(&sp, &core, true).await;
    let ctx = as_user(&core, boss).await;
    let n = resolve::accept_as_new_version(&core, &ctx, v1).await.unwrap();
    assert_eq!(n, 2);
    let f = documents::open_file(&core, &ctx, doc, Some(2)).await.unwrap();
    assert_eq!(read_all(f.stream).await, pdf("fuel at 2027 rates"));
    assert_eq!(sp.content_of(&item), original, "version 1's file is back as it was");
    assert_eq!(file_state(&core, v1).await.0, "ok");
    let note: String = sqlx::query_scalar("SELECT note FROM document_versions WHERE document_id = $1 AND version_no = 2")
        .bind(doc)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert!(note.contains("Pat Clerk"), "{note}");
}

#[tokio::test]
async fn accept_needs_edit_rights_restore_does_not() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    // An administrator outside the department may view but not edit the document.
    let (boss, _doc, v1, _item, _original) = edited_in_sharepoint(&sp, &core, false).await;
    let ctx = as_user(&core, boss).await;
    assert!(matches!(
        resolve::accept_as_new_version(&core, &ctx, v1).await,
        Err(CoreError::Forbidden(_))
    ));
    assert_eq!(file_state(&core, v1).await.0, "changed_outside", "nothing changed");
    resolve::restore_original(&core, &ctx, v1).await.unwrap();
    assert_eq!(file_state(&core, v1).await.0, "ok");
}

#[tokio::test]
async fn a_deleted_file_is_cleared_after_a_recycle_bin_restore_and_recheck() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let boss = admin(&core, "admin@county.example", Some(roads)).await;
    let (_doc, v1, item) = stored_document(&core, boss, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    sp.human_delete(&item, "Pat Clerk");
    changes::detect_changes(&core).await.unwrap();
    let ctx = as_user(&core, boss).await;
    let err = resolve::restore_original(&core, &ctx, v1).await.unwrap_err();
    assert!(err.to_string().contains("recycle bin"), "{err}");
    assert!(!resolve::recheck(&core, &ctx, v1).await.unwrap());
    sp.restore_from_recycle_bin(&item);
    assert!(resolve::recheck(&core, &ctx, v1).await.unwrap());
    assert_eq!(file_state(&core, v1).await, ("ok".to_owned(), None));
}

#[tokio::test]
async fn dismissed_untracked_files_leave_the_list() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let boss = admin(&core, "admin@county.example", Some(roads)).await;
    stored_document(&core, boss, roads, "Fuel Supply", &pdf("fuel")).await;
    changes::detect_changes(&core).await.unwrap();
    let stray = sp.human_upload("Departments/Road and Bridge", "notes.docx", b"notes", "Pat Clerk");
    changes::detect_changes(&core).await.unwrap();
    sqlx::query("UPDATE sharepoint_unexpected_items SET first_seen = now() - interval '11 minutes'")
        .execute(&core.db)
        .await
        .unwrap();
    assert_eq!(status::unexpected_files(&core.db).await.unwrap().len(), 1);
    resolve::dismiss_unexpected(&core, &as_user(&core, boss).await, &stray).await.unwrap();
    assert!(status::unexpected_files(&core.db).await.unwrap().is_empty());
    assert!(audit_actions(&core).await.contains(&"sharepoint.unexpected_dismissed".to_owned()));
}
```

Run: `cargo test -p pdms-core --test sharepoint_ops`
Expected: compile error `could not find resolve in sharepoint`.

- [x] **Step 2: Implement `resolve.rs`.** Create `crates/pdms-core/src/sharepoint/resolve.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Resolving a file changed directly in SharePoint (spec §7.6). An
//! administrator either puts PDMS's recorded file back (picked from
//! SharePoint's version history by its SHA-256, never by label), or keeps the
//! change as a new document version through the normal upload checks and then
//! puts the earlier file back. Also: re-checking a file after a manual fix, and
//! dismissing files PDMS does not track.

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use pdms_audit::{AuditEvent, Category};
use pdms_authz::Capability;
use pdms_storage::ByteStream;
use pdms_storage::spool::{SpoolWriter, sanitize_file_name};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::changes::DriveItem;
use super::status;
use crate::Core;
use crate::context::Ctx;
use crate::documents::files::Upload;
use crate::documents::{self, VersionKind};
use crate::error::{CoreError, CoreResult};
use crate::graph::{GraphClient, GraphError};

const MISSING_HELP: &str = "The file was deleted in SharePoint. A site owner can restore it from the \
    site's recycle bin (Site contents → Recycle bin); then choose Re-check.";
const ALREADY_OK: &str = "This file already matches PDMS's record.";
const NO_ORIGINAL: &str = "No earlier SharePoint version of this file matches PDMS's record. Accept the \
    change as a new version, or restore the file by hand in SharePoint and choose Re-check.";

struct Flagged {
    version_id: Uuid,
    document_id: Uuid,
    version_no: i32,
    label: String,
    department_id: Uuid,
    drive_id: String,
    item_id: String,
    size: i64,
    quick_xor_hash: String,
    state: String,
    sha256: Vec<u8>,
    file_name: String,
    changed_by: Option<String>,
    changed_at: Option<DateTime<Utc>>,
}

async fn load(core: &Core, version_id: Uuid) -> CoreResult<Flagged> {
    let r = sqlx::query!(
        r#"SELECT f.document_id, v.version_no, d.external_id, d.title, d.department_id, f.drive_id,
                  f.item_id, f.size, f.quick_xor_hash, f.state, v.blob_sha256 AS "sha256!",
                  coalesce(v.file_name, f.name) AS "file_name!", f.changed_by, f.changed_at
             FROM sharepoint_files f
             JOIN document_versions v ON v.id = f.document_version_id
             JOIN documents d ON d.id = f.document_id
            WHERE f.document_version_id = $1"#,
        version_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    Ok(Flagged {
        version_id,
        document_id: r.document_id,
        version_no: r.version_no,
        label: format!("{} {}", r.external_id, r.title),
        department_id: r.department_id,
        drive_id: r.drive_id,
        item_id: r.item_id,
        size: r.size,
        quick_xor_hash: r.quick_xor_hash,
        state: r.state,
        sha256: r.sha256,
        file_name: r.file_name,
        changed_by: r.changed_by,
        changed_at: r.changed_at,
    })
}

fn event(ctx: &Ctx, f: &Flagged, action: &str) -> AuditEvent {
    AuditEvent::new(Category::Content, action, ctx.actor.clone(), "document")
        .object(f.document_id, Some(&f.label))
        .document(f.document_id)
        .department(Some(f.department_id))
        .context("version", f.version_no)
        .context("item_id", f.item_id.as_str())
}

/// The file as SharePoint has it now (`None` if it is gone).
async fn current(graph: &GraphClient, f: &Flagged) -> CoreResult<Option<DriveItem>> {
    let path = format!("/drives/{}/items/{}?$select=id,size,file", f.drive_id, f.item_id);
    match graph.get::<DriveItem>(&path).await {
        Ok(i) => Ok(Some(i)),
        Err(GraphError::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn matches_record(f: &Flagged, item: &DriveItem) -> bool {
    item.size == Some(f.size) && item.quick_xor_hash() == Some(f.quick_xor_hash.as_str())
}

async fn sha256_of(mut s: ByteStream) -> CoreResult<Vec<u8>> {
    let mut h = Sha256::new();
    while let Some(chunk) = s.next().await {
        h.update(&chunk.map_err(CoreError::internal)?);
    }
    Ok(h.finalize().to_vec())
}

#[derive(Deserialize)]
struct Versions {
    value: Vec<VersionInfo>,
}

#[derive(Deserialize)]
struct VersionInfo {
    id: String,
    #[serde(default)]
    size: Option<i64>,
}

/// The newest earlier SharePoint version whose content is PDMS's recorded file.
async fn original_version(graph: &GraphClient, f: &Flagged) -> CoreResult<Option<String>> {
    let list: Versions = graph
        .get(&format!("/drives/{}/items/{}/versions", f.drive_id, f.item_id))
        .await?;
    // The first entry is the current (changed) content.
    for v in list.value.iter().skip(1).take(20) {
        if v.size.is_some_and(|s| s != f.size) {
            continue;
        }
        let path = format!("/drives/{}/items/{}/versions/{}/content", f.drive_id, f.item_id, v.id);
        if sha256_of(graph.download(&path).await?).await? == f.sha256 {
            return Ok(Some(v.id.clone()));
        }
    }
    Ok(None)
}

/// Make PDMS's recorded content current again. `Ok(None)` if SharePoint no
/// longer has it.
async fn put_back(graph: &GraphClient, f: &Flagged) -> CoreResult<Option<String>> {
    let Some(vid) = original_version(graph, f).await? else {
        return Ok(None);
    };
    let path = format!("/drives/{}/items/{}/versions/{vid}/restoreVersion", f.drive_id, f.item_id);
    graph.post::<_, Value>(&path, &json!({})).await?;
    match current(graph, f).await? {
        Some(item) if matches_record(f, &item) => Ok(Some(vid)),
        _ => Err(CoreError::internal(
            "SharePoint restored the earlier version, but the file still differs from PDMS's record",
        )),
    }
}

async fn mark_ok(
    core: &Core,
    ctx: &Ctx,
    f: &Flagged,
    resolution: &str,
    sharepoint_version: Option<&str>,
    new_version: Option<i32>,
) -> CoreResult<()> {
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE sharepoint_files SET state = 'ok', changed_by = NULL, changed_at = NULL, checked_at = now()
          WHERE document_version_id = $1",
        f.version_id
    )
    .execute(&mut *tx)
    .await?;
    let mut e = event(ctx, f, "sharepoint.change_resolved").context("resolution", resolution);
    if let Some(v) = sharepoint_version {
        e = e.context("sharepoint_version", v);
    }
    if let Some(n) = new_version {
        e = e.context("new_version", n);
    }
    pdms_audit::append(&mut tx, vec![e]).await?;
    tx.commit().await?;
    Ok(())
}

/// Put PDMS's recorded file back (requires `ManageSharePoint`).
pub async fn restore_original(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<()> {
    ctx.require(Capability::ManageSharePoint)?;
    let (graph, _) = status::parts(core).await?;
    let f = load(core, version_id).await?;
    match f.state.as_str() {
        "changed_outside" => {}
        "missing" => return Err(CoreError::Conflict(MISSING_HELP.into())),
        _ => return Err(CoreError::Conflict(ALREADY_OK.into())),
    }
    let vid = put_back(graph, &f)
        .await?
        .ok_or_else(|| CoreError::Conflict(NO_ORIGINAL.into()))?;
    mark_ok(core, ctx, &f, "restored_original", Some(&vid), None).await
}

/// Keep the change as a new version of the document (requires
/// `ManageSharePoint`, and `edit` on the document through the normal upload
/// path), then put the earlier file back. Returns the new version number.
pub async fn accept_as_new_version(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<i32> {
    ctx.require(Capability::ManageSharePoint)?;
    let (graph, _) = status::parts(core).await?;
    let f = load(core, version_id).await?;
    match f.state.as_str() {
        "changed_outside" => {}
        "missing" => return Err(CoreError::Conflict(MISSING_HELP.into())),
        _ => return Err(CoreError::Conflict(ALREADY_OK.into())),
    }
    let mut w = SpoolWriter::create(&core.config.storage.spool_path, core.config.storage.max_upload_bytes)
        .await
        .map_err(CoreError::internal)?;
    let mut s = graph
        .download(&format!("/drives/{}/items/{}/content", f.drive_id, f.item_id))
        .await?;
    while let Some(chunk) = s.next().await {
        let chunk = chunk.map_err(CoreError::internal)?;
        w.write(&chunk)
            .await
            .map_err(|e| CoreError::validation(format!("The changed file could not be read: {e}")))?;
    }
    let upload = Upload {
        file: w.finish().await.map_err(CoreError::internal)?,
        file_name: sanitize_file_name(&f.file_name),
    };
    let when = f.changed_at.map(|t| format!(" on {}", t.format("%Y-%m-%d"))).unwrap_or_default();
    let note = format!(
        "Change made directly in SharePoint by {}{when}, accepted.",
        f.changed_by.as_deref().unwrap_or("someone")
    );
    let new_version =
        documents::add_version(core, ctx, f.document_id, upload, VersionKind::Revision, &note, None).await?;
    // The earlier file is part of the record too: put it back if SharePoint still has it.
    match put_back(graph, &f).await? {
        Some(vid) => mark_ok(core, ctx, &f, "accepted_as_new_version", Some(&vid), Some(new_version)).await?,
        None => {
            let mut tx = core.db.begin().await?;
            let e = event(ctx, &f, "sharepoint.change_resolved")
                .context("resolution", "accepted_as_new_version")
                .context("new_version", new_version)
                .context("original_restored", false);
            pdms_audit::append(&mut tx, vec![e]).await?;
            tx.commit().await?;
        }
    }
    Ok(new_version)
}

/// Compare the file with PDMS's record again, after someone fixed it by hand
/// (requires `ManageSharePoint`). True when it matches.
pub async fn recheck(core: &Core, ctx: &Ctx, version_id: Uuid) -> CoreResult<bool> {
    ctx.require(Capability::ManageSharePoint)?;
    let (graph, _) = status::parts(core).await?;
    let f = load(core, version_id).await?;
    let item = current(graph, &f).await?;
    let ok = item.as_ref().is_some_and(|i| matches_record(&f, i));
    if ok && f.state != "ok" {
        mark_ok(core, ctx, &f, "matches_on_recheck", None, None).await?;
    } else {
        let state = match (ok, item.is_some()) {
            (true, _) => "ok",
            (false, true) => "changed_outside",
            (false, false) => "missing",
        };
        sqlx::query!(
            "UPDATE sharepoint_files SET state = $2, checked_at = now(), changed_at = coalesce(changed_at, now())
              WHERE document_version_id = $1",
            version_id,
            state
        )
        .execute(&core.db)
        .await?;
    }
    Ok(ok)
}

/// Stop listing a file PDMS does not track (requires `ManageSharePoint`).
pub async fn dismiss_unexpected(core: &Core, ctx: &Ctx, item_id: &str) -> CoreResult<()> {
    ctx.require(Capability::ManageSharePoint)?;
    let mut tx = core.db.begin().await?;
    let r = sqlx::query!(
        "UPDATE sharepoint_unexpected_items SET dismissed_by = $2, dismissed_at = now()
          WHERE item_id = $1 AND dismissed_at IS NULL RETURNING name, path",
        item_id,
        ctx.user_id()
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(CoreError::NotFound)?;
    pdms_audit::append(
        &mut tx,
        vec![
            AuditEvent::new(Category::Admin, "sharepoint.unexpected_dismissed", ctx.actor.clone(), "sharepoint_item")
                .object(item_id, Some(&r.name))
                .context("path", r.path),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
```

In `sharepoint/mod.rs`, add `pub mod resolve;`.

- [x] **Step 3: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint_ops`
Expected: the five new tests and all earlier ones `... ok`.

- [x] **Step 4: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/sharepoint crates/pdms-core/tests/sharepoint_ops.rs .sqlx
git commit -m "sharepoint: restore originals, accept changes as new versions, re-check, dismiss"
```

---
### Task 6: `verify-integrity` checks SharePoint files

**Files:**
- Modify: `crates/pdms-core/src/integrity.rs`
- Modify: `crates/pdms/src/main.rs`
- Modify: `crates/pdms-core/tests/sharepoint.rs` (P3's test that expected files to be skipped)
- Test: `crates/pdms-core/tests/sharepoint_ops.rs`

**Interfaces:**
- Consumes: Task 4 `changes::DriveItem`; P2 `GraphClient::{get, download}`; P3 `IntegrityReport.sharepoint_files_skipped`, which is replaced here.
- Produces:
  - `pdms_core::integrity::VerifyOptions { rehash: bool, download_sharepoint: bool }`;
  - `pub async fn verify_with(core: &Core, opts: VerifyOptions) -> CoreResult<IntegrityReport>`;
  - `verify(core, rehash)` stays, as `verify_with(core, VerifyOptions { rehash, download_sharepoint: false })`;
  - `IntegrityReport.sharepoint_files_checked: u64`, replacing `sharepoint_files_skipped`;
  - Problem kind `sharepoint_file`;
  - CLI flag `pdms verify-integrity --rehash-sharepoint`.

What is checked:

| Option | SharePoint files |
|---|---|
| `--skip-files` | present and the right size (Graph metadata) |
| default | also QuickXorHash (Graph metadata, no download) |
| `--rehash-sharepoint` | also download and compare SHA-256 |

Any file still flagged by the change check is reported too.

> **As built (2026-10-23):** `files_checked` now counts local and S3 files with its own counter (P3 subtracted the skipped SharePoint files); P3's `sharepoint_files_skipped` audit context is replaced by `sharepoint_files_checked` and `sharepoint_downloaded`; the `Problem.kind` doc comment lists `sharepoint_file`; the hash comparison uses `h.finalize()[..]` (`as_slice` is deprecated). ADMIN_GUIDE and BACKUP.md describe the new checks and `--rehash-sharepoint` now, in the same commit.

- [x] **Step 1: Write the failing test.** Add `use pdms_core::integrity;` to the imports of `crates/pdms-core/tests/sharepoint_ops.rs`, and append:

```rust
#[tokio::test]
async fn verify_integrity_checks_sharepoint_files() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let roads = department(&core, "RB", "Road and Bridge").await;
    let alice = entra_user(&core, "alice@county.example", Some(roads)).await;
    let (_a, _va, edited) = stored_document(&core, alice, roads, "Fuel Supply", &pdf("fuel")).await;
    let (_b, _vb, deleted) = stored_document(&core, alice, roads, "Office Lease", &pdf("lease")).await;
    let clean = integrity::verify(&core, true).await.unwrap();
    assert!(clean.problems.is_empty(), "{:?}", clean.problems);
    assert_eq!(clean.sharepoint_files_checked, 2);

    // Same size, different content: only a hash shows it.
    sp.human_edit(&edited, &pdf("FUEL"), "Pat Clerk");
    sp.human_delete(&deleted, "Pat Clerk");
    let fast = integrity::verify(&core, false).await.unwrap();
    assert_eq!(fast.problems.len(), 1, "presence and size only: {:?}", fast.problems);
    assert!(fast.problems[0].detail.contains("missing from SharePoint"));
    let full = integrity::verify(&core, true).await.unwrap();
    assert!(full.problems.iter().any(|p| p.detail.contains("QuickXorHash")), "{:?}", full.problems);
    let deep = integrity::verify_with(
        &core,
        integrity::VerifyOptions { rehash: true, download_sharepoint: true },
    )
    .await
    .unwrap();
    assert!(deep.problems.iter().any(|p| p.detail.contains("SHA-256")), "{:?}", deep.problems);
    assert!(deep.problems.iter().all(|p| p.kind == "sharepoint_file"));
}
```

In P3's `crates/pdms-core/tests/sharepoint.rs`, rename `integrity_check_leaves_sharepoint_files_to_the_change_check` to `integrity_check_covers_sharepoint_files`, and change its last line `assert_eq!(r.sharepoint_files_skipped, 1);` to `assert_eq!(r.sharepoint_files_checked, 1);`.

Run: `cargo test -p pdms-core --test sharepoint_ops verify_integrity`
Expected: compile error `no field sharepoint_files_checked` / `cannot find verify_with`.

- [x] **Step 2: Implement.** In `crates/pdms-core/src/integrity.rs`:

(a) Add to the imports:

```rust
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
```

(b) In `IntegrityReport`, replace P3's `sharepoint_files_skipped` field and its doc comment with:

```rust
    /// Files stored in SharePoint, checked against SharePoint's metadata (and,
    /// with `download_sharepoint`, by downloading them).
    pub sharepoint_files_checked: u64,
```

(c) Rename `pub async fn verify(core: &Core, rehash: bool) -> CoreResult<IntegrityReport>` to `pub async fn verify_with(core: &Core, opts: VerifyOptions) -> CoreResult<IntegrityReport>` and make `let rehash = opts.rehash;` its first statement. Above it add:

```rust
/// What `verify_with` checks beyond presence and size.
#[derive(Copy, Clone, Debug, Default)]
pub struct VerifyOptions {
    /// Re-read local files (SHA-256); compare SharePoint files' QuickXorHash.
    pub rehash: bool,
    /// Also download every SharePoint file and compare SHA-256 (slow).
    pub download_sharepoint: bool,
}

/// Run every check. `rehash = false` checks presence and size only (fast).
pub async fn verify(core: &Core, rehash: bool) -> CoreResult<IntegrityReport> {
    verify_with(core, VerifyOptions { rehash, download_sharepoint: false }).await
}
```

(d) In `verify_with`:
- delete `let mut sharepoint_files_skipped = 0u64;`, and in the blob loop change P3's `sharepoint_files_skipped += 1; continue;` to just `continue;` (SharePoint files are never looked for in local storage);
- after the export-package loop, before `let versions = …`, add:

```rust
    let sharepoint_files_checked = check_sharepoint(core, opts, &mut problems, &mut bytes).await?;
```

- in the `IntegrityReport { … }` literal, replace `sharepoint_files_skipped,` with `sharepoint_files_checked,`;
- add to the audit event's contexts, after `.context("contents_rehashed", rehash)`:

```rust
    .context(
        "sharepoint_files_checked",
        i64::try_from(report.sharepoint_files_checked).unwrap_or(i64::MAX),
    )
    .context("sharepoint_downloaded", opts.download_sharepoint)
```

(e) Add this function above `verify_with`:

```rust
/// Check every file PDMS stored in SharePoint against Graph's metadata and,
/// optionally, its content. Problems are collected, never fatal.
async fn check_sharepoint(
    core: &Core,
    opts: VerifyOptions,
    out: &mut Vec<Problem>,
    bytes: &mut u64,
) -> CoreResult<u64> {
    let rows = sqlx::query!(
        r#"SELECT f.drive_id, f.item_id, f.quick_xor_hash, f.size, f.state, f.last_known_path,
                  v.blob_sha256 AS "sha256!"
             FROM sharepoint_files f JOIN document_versions v ON v.id = f.document_version_id
            ORDER BY f.uploaded_at"#
    )
    .fetch_all(&core.db)
    .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let Some(graph) = core.graph.as_ref() else {
        out.push(Problem {
            kind: "sharepoint_file",
            subject: "SharePoint".into(),
            detail: format!(
                "{} files are stored in SharePoint, but Microsoft Graph is not configured, so they cannot be checked",
                rows.len()
            ),
        });
        return Ok(0);
    };
    for r in &rows {
        let problem = |detail: String| Problem {
            kind: "sharepoint_file",
            subject: r.last_known_path.clone(),
            detail,
        };
        if r.state != "ok" {
            out.push(problem(format!(
                "flagged as {} (Administration → SharePoint)",
                r.state.replace('_', " ")
            )));
        }
        let path = format!("/drives/{}/items/{}?$select=id,size,file", r.drive_id, r.item_id);
        let item = match graph.get::<crate::sharepoint::changes::DriveItem>(&path).await {
            Ok(i) => i,
            Err(crate::graph::GraphError::NotFound) => {
                out.push(problem("missing from SharePoint".into()));
                continue;
            }
            Err(e) => {
                out.push(problem(format!("could not be read: {e}")));
                continue;
            }
        };
        *bytes += u64::try_from(r.size).unwrap_or(0);
        if item.size != Some(r.size) {
            out.push(problem(format!(
                "size is {} bytes, expected {}",
                item.size.unwrap_or(-1),
                r.size
            )));
            continue;
        }
        if opts.rehash && item.quick_xor_hash() != Some(r.quick_xor_hash.as_str()) {
            out.push(problem("content changed (QuickXorHash differs from the upload)".into()));
        }
        if opts.download_sharepoint {
            let url = format!("/drives/{}/items/{}/content", r.drive_id, r.item_id);
            match graph.download(&url).await {
                Ok(mut s) => {
                    let mut h = Sha256::new();
                    let mut failed = None;
                    while let Some(c) = s.next().await {
                        match c {
                            Ok(c) => h.update(&c),
                            Err(e) => {
                                failed = Some(e.to_string());
                                break;
                            }
                        }
                    }
                    match failed {
                        Some(e) => out.push(problem(format!("could not be read: {e}"))),
                        None if h.finalize().as_slice() != r.sha256.as_slice() => {
                            out.push(problem("content does not match its SHA-256".into()));
                        }
                        None => {}
                    }
                }
                Err(e) => out.push(problem(format!("could not be read: {e}"))),
            }
        }
    }
    Ok(u64::try_from(rows.len()).unwrap_or(u64::MAX))
}
```

In `crates/pdms/src/main.rs`:
- add to `Command::VerifyIntegrity`, after `skip_files`:

```rust
        /// Also download every file stored in SharePoint and re-hash it with SHA-256 (slow).
        #[arg(long)]
        rehash_sharepoint: bool,
```

- change the match arm to `Command::VerifyIntegrity { skip_files, rehash_sharepoint, json } => {`;
- change the call to:

```rust
            let r = pdms_core::integrity::verify_with(
                &core,
                pdms_core::integrity::VerifyOptions {
                    rehash: !skip_files,
                    download_sharepoint: rehash_sharepoint,
                },
            )
            .await?;
```

- replace P3's `if r.sharepoint_files_skipped > 0 { … }` block with:

```rust
                if r.sharepoint_files_checked > 0 {
                    println!(
                        "SharePoint files: {} checked ({}).",
                        r.sharepoint_files_checked,
                        if rehash_sharepoint {
                            "downloaded and re-hashed"
                        } else if skip_files {
                            "presence and size only"
                        } else {
                            "size and QuickXorHash"
                        }
                    );
                }
```

- [x] **Step 3: Run the tests and confirm they pass.**

```bash
cargo test -p pdms-core --test sharepoint_ops verify_integrity
cargo test -p pdms-core --test sharepoint
cargo test -p pdms-core --test phase4
cargo run -q -p pdms -- verify-integrity --help | grep rehash-sharepoint
```

Expected: tests `ok`; the help line `--rehash-sharepoint  Also download every file stored in SharePoint and re-hash it with SHA-256 (slow)`.

- [x] **Step 4: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/integrity.rs crates/pdms/src/main.rs crates/pdms-core/tests .sqlx
git commit -m "integrity: check SharePoint files (metadata by default, --rehash-sharepoint downloads)"
```

---
### Task 7: Administration → SharePoint, and a notice on the document page

**Files:**
- Modify: `crates/pdms-core/src/sharepoint/status.rs` (`panel`)
- Create: `crates/pdms-web/src/routes/sharepoint_admin.rs`, `crates/pdms-web/templates/admin/sharepoint.html`
- Modify: `crates/pdms-web/src/routes/mod.rs`, `crates/pdms-web/src/lib.rs`, `crates/pdms-web/src/views.rs`, `crates/pdms-web/templates/base.html`
- Modify: `crates/pdms-web/src/routes/documents.rs` (`show`), `crates/pdms-web/templates/documents/show.html`
- Modify: `crates/pdms-web/templates/admin/entra.html` (P2's page: a pointer here)
- Test: `crates/pdms-web/tests/sharepoint_admin.rs` (create)

**Interfaces:**
- Consumes:
  - Tasks 1–5: `status::{document_flags, unexpected_files, UnexpectedFile, DELTA_CURSOR, RECONCILE_CURSOR, ENTRA_CURSOR}`, `changes::enqueue_now`, `access::enqueue_reconcile`, `resolve::*`.
  - Existing web helpers: `CurrentUser::require`, `views::{Layout, FlashMsg, done, redirect_with, fmt_time, fmt_time_opt}`.
- Produces:
  - `status::{panel(core, ctx) -> CoreResult<Panel>, Panel, RunStatus, DeadJob, FlaggedFile, ForeignGrant}`;
  - routes: `GET /admin/sharepoint`, `POST /admin/sharepoint/check`, `POST /admin/sharepoint/access`, `POST /admin/sharepoint/files/{version}/{restore|accept|recheck}`, `POST /admin/sharepoint/unexpected/{item}/dismiss`. Each handler checks `ManageSharePoint` itself (they sit in `doc_admin_routes`);
  - `Layout.can_sharepoint`.

> **As built (2026-10-23):**
> - The page follows R2's admin page pattern (as the pre-flight says): `page-head` with the two buttons in `page-actions` ("Check for changes now" the only primary one), every section a `panel` with a `panel-head` and count, run status as a table with `status` badges, `table-wrap` tables with hidden captions, `p.empty` for empty lists, "not turned on" as `flash-info role="status"`, and a `flash-error role="alert"` when files need a decision. The handler calls `.admin_tab("sharepoint")`; the menu item is in the *Integrations* group of `Layout::admin_groups()`, not `base.html`. The Entra page section is a `panel`.
> - Owner answer 5: `FlaggedFile.external_id` is `None` too for a document the viewer cannot open, and the page shows "A Restricted document (you cannot open it)" and the version only.
> - Owner answer 6: the notice is a `p.flash.flash-info role="status"` inside `header.doc-head`, after `.doc-title-row`: "A file of this document was changed outside PDMS. Downloads of it are paused until an administrator checks it.", with a link to Administration → SharePoint for people who hold `ManageSharePoint`. `ShowPage` carries a `sp_flagged: bool`, not the list.
> - `/admin/sharepoint` is added to `every_admin_page_uses_the_admin_pattern` and to `tools/ui-screenshots/shoot.py`; the page and the document page were checked in dark and light (no overflow, one `<h1>`, no console errors). ADMIN_GUIDE's "SharePoint storage" section describes the page.

- [x] **Step 1: Write the failing web test.** Create `crates/pdms-web/tests/sharepoint_admin.rs`:

```rust
#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
//! Administration → SharePoint and the document-page notice. This app stores
//! files locally; the SharePoint records are written directly, as the change
//! check would leave them.

mod support;

use axum::http::StatusCode;
use pdms_authz::Visibility;
use pdms_core::directory;
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use support::*;
use uuid::Uuid;

/// A document with one file, recorded as edited in SharePoint by Pat Clerk.
/// Returns (document, file version).
async fn flagged_document(app: &App, owner: Uuid, dept: Uuid, visibility: Visibility, title: &str) -> (Uuid, Uuid) {
    let core = &app.core;
    let mut w = pdms_storage::spool::SpoolWriter::create(&core.config.storage.spool_path, 1 << 20)
        .await
        .unwrap();
    w.write(b"%PDF-1.4\n% agreement\n%%EOF\n").await.unwrap();
    let file = documents::files::Upload { file: w.finish().await.unwrap(), file_name: "agreement.pdf".into() };
    let t = types::type_by_key(&core.db, "contract").await.unwrap().id;
    let doc = documents::create(
        core,
        &system(),
        NewDocument {
            metadata: MetadataInput { title: title.into(), document_type_id: t, department_id: dept, ..Default::default() },
            owner_id: owner,
            status: DocumentStatus::Active,
            tags: vec![],
            file: Some(file),
            note: String::new(),
            visibility,
        },
    )
    .await
    .unwrap();
    let version: Uuid = sqlx::query_scalar("SELECT id FROM document_versions WHERE document_id = $1 AND file_changed")
        .bind(doc)
        .fetch_one(&core.db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO sharepoint_files (document_version_id, document_id, drive_id, item_id, list_item_id, name,
                                       quick_xor_hash, size, last_known_path, state, changed_by, changed_at)
         VALUES ($1, $2, 'drive-1', $3, 'li-1', 'x.pdf', 'AAAAAAAAAAAAAAAAAAAAAAAAAAA=', 27, $4,
                 'changed_outside', 'Pat Clerk <pat.clerk@county.example>', now())",
    )
    .bind(version)
    .bind(doc)
    .bind(format!("item-{version}"))
    .bind(format!("Departments/Road and Bridge/{title}/{title} v1 – Original.pdf"))
    .execute(&core.db)
    .await
    .unwrap();
    (doc, version)
}

#[tokio::test]
async fn sharepoint_panel_is_for_administrators() {
    let app = app().await;
    let core = &app.core;
    let dept = directory::create_department(&core.db, &system(), "RB", "Road and Bridge").await.unwrap();
    let (admin, admin_totp) = user(core, "admin@x.org", Some(dept), Some("admin")).await;
    let (clerk, clerk_totp) = user(core, "clerk@x.org", Some(dept), None).await;
    let (doc, version) = flagged_document(&app, admin, dept, Visibility::Department, "Fuel Supply Agreement").await;
    // A Restricted document the administrator cannot open.
    flagged_document(&app, clerk, dept, Visibility::Restricted, "Personnel Investigation").await;

    let mut c = Browser::new(&app);
    c.login("clerk@x.org", &clerk_totp).await;
    assert_eq!(c.get("/admin/sharepoint").await.status, StatusCode::FORBIDDEN);
    // People who can open the document see that its file was changed.
    let page = c.get(&format!("/documents/{doc}")).await;
    assert!(page.body.contains("changed directly in SharePoint"), "{}", page.body);

    let mut b = Browser::new(&app);
    b.login("admin@x.org", &admin_totp).await;
    let page = b.get("/admin/sharepoint").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("SharePoint storage is not turned on"));
    assert!(page.body.contains("Fuel Supply Agreement"));
    assert!(page.body.contains("Pat Clerk"));
    assert!(page.body.contains(&format!("/admin/sharepoint/files/{version}/restore")));
    // ID only, and no path, for the document the administrator cannot open.
    assert!(!page.body.contains("Personnel Investigation"), "title or path leaked");
    assert!(page.body.contains("you cannot open this document"));
    assert!(page.body.contains(r#"href="/admin/sharepoint""#), "navigation link");

    let r = b.post("/admin/sharepoint/check", &[("_csrf", &page.csrf())]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'sharepoint.delta'")
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(queued, 1);
    // Without SharePoint storage, resolving explains itself instead of failing.
    let r = b.post(&format!("/admin/sharepoint/files/{version}/restore"), &[("_csrf", &page.csrf())]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
}
```

Run: `cargo test -p pdms-web --test sharepoint_admin`
Expected: FAIL. `/admin/sharepoint` returns 404, and the document page has no notice.

- [x] **Step 2: The panel query.** Append to `crates/pdms-core/src/sharepoint/status.rs`, and change its `pdms_authz` import to `use pdms_authz::{Capability, Permission, role_keys};`:

```rust
#[derive(Clone, Debug, Serialize)]
pub struct RunStatus {
    pub name: String,
    pub last_run: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeadJob {
    pub kind: String,
    pub count: i64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlaggedFile {
    pub version_id: Uuid,
    pub document_id: Uuid,
    pub external_id: String,
    /// `None` when the viewer cannot open the document.
    pub title: Option<String>,
    /// The SharePoint path names the document, so it is hidden the same way.
    pub path: Option<String>,
    pub version_no: i32,
    pub state: String,
    pub changed_by: Option<String>,
    pub changed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ForeignGrant {
    pub department: String,
    pub granted_to: String,
    pub roles: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Panel {
    pub enabled: bool,
    pub site_url: String,
    pub client_id: String,
    pub runs: Vec<RunStatus>,
    pub dead_jobs: Vec<DeadJob>,
    pub flagged: Vec<FlaggedFile>,
    pub unexpected: Vec<UnexpectedFile>,
    pub foreign: Vec<ForeignGrant>,
}

/// Everything Administration → SharePoint shows (requires `ManageSharePoint`).
pub async fn panel(core: &Core, ctx: &Ctx) -> CoreResult<Panel> {
    ctx.require(Capability::ManageSharePoint)?;
    let names = vec![DELTA_CURSOR.to_owned(), RECONCILE_CURSOR.to_owned(), ENTRA_CURSOR.to_owned()];
    let runs = sqlx::query_as!(
        RunStatus,
        "SELECT name, last_run, last_error FROM sync_cursors WHERE name = ANY($1) ORDER BY name",
        &names
    )
    .fetch_all(&core.db)
    .await?;
    let dead_jobs = sqlx::query_as!(
        DeadJob,
        r#"SELECT kind, count(*) AS "count!", (array_agg(last_error ORDER BY updated_at DESC))[1] AS last_error
             FROM jobs
            WHERE status = 'dead' AND (kind LIKE 'sharepoint.%' OR kind LIKE 'directory.entra_sync%')
            GROUP BY kind ORDER BY kind"#
    )
    .fetch_all(&core.db)
    .await?;
    let rows = sqlx::query!(
        "SELECT f.document_version_id, f.document_id, d.external_id, d.title, v.version_no, f.state,
                f.changed_by, f.changed_at, f.last_known_path
           FROM sharepoint_files f
           JOIN document_versions v ON v.id = f.document_version_id
           JOIN documents d ON d.id = f.document_id
          WHERE f.state <> 'ok'
          ORDER BY f.changed_at DESC NULLS LAST LIMIT 500"
    )
    .fetch_all(&core.db)
    .await?;
    let mut flagged = Vec::with_capacity(rows.len());
    for r in rows {
        let visible = crate::documents::access::can(core, ctx, r.document_id, Permission::View).await?;
        flagged.push(FlaggedFile {
            version_id: r.document_version_id,
            document_id: r.document_id,
            external_id: r.external_id,
            title: visible.then_some(r.title),
            path: visible.then_some(r.last_known_path),
            version_no: r.version_no,
            state: r.state,
            changed_by: r.changed_by,
            changed_at: r.changed_at,
        });
    }
    let foreign = sqlx::query_as!(
        ForeignGrant,
        "SELECT d.name AS department, g.granted_to, g.roles
           FROM sharepoint_foreign_grants g JOIN departments d ON d.id = g.department_id
          ORDER BY d.name, g.granted_to"
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Panel {
        enabled: core.sharepoint.is_some(),
        site_url: core.config.sharepoint.site_url.clone(),
        client_id: core.config.graph.client_id.clone(),
        runs,
        dead_jobs,
        flagged,
        unexpected: unexpected_files(&core.db).await?,
        foreign,
    })
}
```

- [x] **Step 3: The page.** Create `crates/pdms-web/src/routes/sharepoint_admin.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Administration → SharePoint: background run status, files changed
//! directly in SharePoint and how to resolve them, files PDMS does not track,
//! and access on department folders that PDMS did not grant.

use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, Response};
use pdms_authz::Capability;
use pdms_core::sharepoint::{access, changes, resolve, status};
use uuid::Uuid;

use crate::error::AppResult;
use crate::session::CurrentUser;
use crate::state::AppState;
use crate::views::{FlashMsg, Layout, done, fmt_time, fmt_time_opt, redirect_with};

const BACK: &str = "/admin/sharepoint";

pub struct RunRow {
    pub label: &'static str,
    pub last_run: String,
    pub error: String,
}

pub struct FlagRow {
    pub version_id: Uuid,
    pub document_id: Uuid,
    pub external_id: String,
    pub title: String,
    pub path: String,
    pub can_view: bool,
    pub version_no: i32,
    pub missing: bool,
    pub changed_by: String,
    pub changed_at: String,
}

pub struct StrayRow {
    pub item_id: String,
    pub name: String,
    pub path: String,
    pub created_by: String,
    pub by_pdms: bool,
    pub first_seen: String,
}

#[derive(Template)]
#[template(path = "admin/sharepoint.html")]
struct SharePointPage {
    l: Layout,
    enabled: bool,
    site_url: String,
    client_id: String,
    runs: Vec<RunRow>,
    dead_jobs: Vec<status::DeadJob>,
    flagged: Vec<FlagRow>,
    unexpected: Vec<StrayRow>,
    foreign: Vec<status::ForeignGrant>,
}

fn run_label(name: &str) -> &'static str {
    match name {
        status::DELTA_CURSOR => "Change check",
        status::RECONCILE_CURSOR => "Department folder access",
        status::ENTRA_CURSOR => "Entra user sync",
        _ => "Other",
    }
}

pub async fn page(State(state): State<AppState>, user: CurrentUser, flash: FlashMsg) -> AppResult<Html<String>> {
    user.require(&state, Capability::ManageSharePoint, BACK).await?;
    let p = status::panel(&state.core, &user.ctx).await?;
    let page = SharePointPage {
        l: Layout::new(&state, &user, flash, "SharePoint", "admin"),
        enabled: p.enabled,
        site_url: p.site_url,
        client_id: p.client_id,
        runs: p
            .runs
            .into_iter()
            .map(|r| RunRow {
                label: run_label(&r.name),
                last_run: fmt_time_opt(state.tz, r.last_run),
                error: r.last_error.unwrap_or_default(),
            })
            .collect(),
        dead_jobs: p.dead_jobs,
        flagged: p
            .flagged
            .into_iter()
            .map(|f| FlagRow {
                version_id: f.version_id,
                document_id: f.document_id,
                external_id: f.external_id,
                can_view: f.title.is_some(),
                title: f.title.unwrap_or_default(),
                path: f.path.unwrap_or_default(),
                version_no: f.version_no,
                missing: f.state == "missing",
                changed_by: f.changed_by.unwrap_or_else(|| "unknown".into()),
                changed_at: fmt_time_opt(state.tz, f.changed_at),
            })
            .collect(),
        unexpected: p
            .unexpected
            .into_iter()
            .map(|u| StrayRow {
                item_id: u.item_id,
                name: u.name,
                path: u.path,
                created_by: u.created_by,
                by_pdms: u.by_pdms,
                first_seen: fmt_time(state.tz, u.first_seen),
            })
            .collect(),
        foreign: p.foreign,
    };
    Ok(Html(page.render()?))
}

pub async fn check_now(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK).await?;
    let r = changes::enqueue_now(&state.core.db).await;
    done(&state, BACK, r, "Checking SharePoint for changes. Refresh this page in a minute.")
}

pub async fn update_access(State(state): State<AppState>, user: CurrentUser) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK).await?;
    let r = access::enqueue_reconcile(&state.core.db).await;
    done(&state, BACK, r, "Updating department folder access. Refresh this page in a minute.")
}

pub async fn restore(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK).await?;
    let r = resolve::restore_original(&state.core, &user.ctx, version_id).await;
    done(&state, BACK, r, "The original file is back in SharePoint.")
}

pub async fn accept(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK).await?;
    let r = resolve::accept_as_new_version(&state.core, &user.ctx, version_id).await;
    done(&state, BACK, r, "The change was added to the document as a new version.")
}

pub async fn recheck(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(version_id): Path<Uuid>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK).await?;
    match resolve::recheck(&state.core, &user.ctx, version_id).await {
        Ok(true) => Ok(redirect_with(&state, BACK, true, "The file matches PDMS's record again.")),
        Ok(false) => Ok(redirect_with(&state, BACK, false, "The file still differs from PDMS's record.")),
        Err(e) => done(&state, BACK, Err::<(), _>(e), ""),
    }
}

pub async fn dismiss(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> AppResult<Response> {
    user.require(&state, Capability::ManageSharePoint, BACK).await?;
    let r = resolve::dismiss_unexpected(&state.core, &user.ctx, &item_id).await;
    done(&state, BACK, r, "Dismissed.")
}
```

Create `crates/pdms-web/templates/admin/sharepoint.html`:

```html
{% extends "base.html" %}
{% block content %}
{% if !enabled %}
<p class="flash flash-error" role="status">SharePoint storage is not turned on for this installation. See <code>docs/SHAREPOINT_SETUP.md</code>, then run <code>./install.sh --configure sharepoint</code>.</p>
{% else %}
<p class="muted">Site <code>{{ site_url }}</code> · PDMS Service app <code>{{ client_id }}</code></p>
{% endif %}

<section aria-labelledby="runs-h">
  <h2 id="runs-h">Background work</h2>
  <table>
    <thead><tr><th scope="col">Task</th><th scope="col">Last run</th><th scope="col">Problem</th></tr></thead>
    <tbody>
    {% for r in runs %}<tr><td>{{ r.label }}</td><td>{{ r.last_run }}</td><td class="small">{{ r.error }}</td></tr>
    {% else %}<tr><td colspan="3">Nothing has run yet.</td></tr>{% endfor %}
    </tbody>
  </table>
  {% if !dead_jobs.is_empty() %}
  <table>
    <caption>Jobs that stopped after repeated failures</caption>
    <thead><tr><th scope="col">Job</th><th scope="col">Count</th><th scope="col">Last error</th></tr></thead>
    <tbody>{% for j in dead_jobs %}<tr><td><code>{{ j.kind }}</code></td><td>{{ j.count }}</td><td class="small">{% if let Some(e) = j.last_error %}{{ e }}{% endif %}</td></tr>{% endfor %}</tbody>
  </table>
  {% endif %}
  <div class="toolbar">
    <form method="post" action="/admin/sharepoint/check" class="inline-form"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit">Check for changes now</button></form>
    <form method="post" action="/admin/sharepoint/access" class="inline-form"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit">Update folder access now</button></form>
  </div>
</section>

<section aria-labelledby="flagged-h">
  <h2 id="flagged-h">Files changed outside PDMS</h2>
  <p class="muted">PDMS compares every file it stored with SharePoint every few minutes. <strong>Restore original</strong> puts PDMS's recorded file back. <strong>Accept as new version</strong> keeps the change as a new version of the document (you need edit rights on it) and puts the earlier file back.</p>
  <table>
    <thead><tr><th scope="col">Document</th><th scope="col">Version</th><th scope="col">What happened</th><th scope="col">By</th><th scope="col">Noticed</th><th scope="col"><span class="visually-hidden">Actions</span></th></tr></thead>
    <tbody>
    {% for f in flagged %}
      <tr>
        <td>{% if f.can_view %}<a href="/documents/{{ f.document_id }}">{{ f.external_id }}</a> {{ f.title }}<br><code class="small">{{ f.path }}</code>{% else %}{{ f.external_id }} <span class="muted">(you cannot open this document)</span>{% endif %}</td>
        <td>v{{ f.version_no }}</td>
        <td>{% if f.missing %}Deleted{% else %}Edited{% endif %}</td>
        <td>{{ f.changed_by }}</td>
        <td>{{ f.changed_at }}</td>
        <td>
          {% if !f.missing %}
          <form method="post" action="/admin/sharepoint/files/{{ f.version_id }}/restore" class="inline-form"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit" class="small">Restore original</button></form>
          <form method="post" action="/admin/sharepoint/files/{{ f.version_id }}/accept" class="inline-form"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit" class="small">Accept as new version</button></form>
          {% else %}<span class="small">Restore it from the site's recycle bin, then</span>{% endif %}
          <form method="post" action="/admin/sharepoint/files/{{ f.version_id }}/recheck" class="inline-form"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit" class="small">Re-check</button></form>
        </td>
      </tr>
    {% else %}<tr><td colspan="6">None. Every file matches what PDMS stored.</td></tr>{% endfor %}
    </tbody>
  </table>
</section>

<section aria-labelledby="stray-h">
  <h2 id="stray-h">Files PDMS does not track</h2>
  <p class="muted">Files added to the site some other way. PDMS never deletes them; a site owner can, after checking.</p>
  <table>
    <thead><tr><th scope="col">File</th><th scope="col">Added by</th><th scope="col">First seen</th><th scope="col"><span class="visually-hidden">Actions</span></th></tr></thead>
    <tbody>
    {% for u in unexpected %}
      <tr><td><code>{{ u.path }}</code></td>
      <td>{% if u.by_pdms %}PDMS (a save that did not finish; safe to delete){% else %}{{ u.created_by }}{% endif %}</td>
      <td>{{ u.first_seen }}</td>
      <td><form method="post" action="/admin/sharepoint/unexpected/{{ u.item_id }}/dismiss" class="inline-form"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit" class="small">Dismiss</button></form></td></tr>
    {% else %}<tr><td colspan="4">None.</td></tr>{% endfor %}
    </tbody>
  </table>
</section>

<section aria-labelledby="foreign-h">
  <h2 id="foreign-h">Other access on department folders</h2>
  <p class="muted">PDMS gives each department's staff read access on its folder. These permissions were added some other way; PDMS leaves them alone.</p>
  <table>
    <thead><tr><th scope="col">Department folder</th><th scope="col">Who</th><th scope="col">Access</th></tr></thead>
    <tbody>
    {% for g in foreign %}<tr><td>{{ g.department }}</td><td>{{ g.granted_to }}</td><td>{{ g.roles }}</td></tr>
    {% else %}<tr><td colspan="3">None.</td></tr>{% endfor %}
    </tbody>
  </table>
</section>
{% endblock %}
```

- [x] **Step 4: Wire it in.**

(a) `crates/pdms-web/src/routes/mod.rs`: add `pub mod sharepoint_admin;`.

(b) `crates/pdms-web/src/lib.rs`: add `sharepoint_admin` to the `use routes::{ … }` list, and append to `doc_admin_routes`, before its final `;`:

```rust
        .route("/sharepoint", get(sharepoint_admin::page))
        .route("/sharepoint/check", post(sharepoint_admin::check_now))
        .route("/sharepoint/access", post(sharepoint_admin::update_access))
        .route("/sharepoint/files/{version}/restore", post(sharepoint_admin::restore))
        .route("/sharepoint/files/{version}/accept", post(sharepoint_admin::accept))
        .route("/sharepoint/files/{version}/recheck", post(sharepoint_admin::recheck))
        .route("/sharepoint/unexpected/{item}/dismiss", post(sharepoint_admin::dismiss))
```

(c) `crates/pdms-web/src/views.rs`:
- add `pub can_sharepoint: bool,` to `Layout`;
- add `Capability::ManageSharePoint,` to the `can_admin` array in `Layout::new`, and `can_sharepoint: user.can(Capability::ManageSharePoint),` to the literal;
- in `admin_home`, add `} else if self.can_sharepoint { "/admin/sharepoint"` before the final `} else {`.

(d) `crates/pdms-web/templates/base.html`: after the `{% if l.can_integrations %}…{% endif %}` navigation line, add:

```html
      {% if l.can_sharepoint %}<li><a href="/admin/sharepoint">SharePoint</a></li>{% endif %}
```

(e) P2's `crates/pdms-web/templates/admin/entra.html`: directly before `{% endblock %}`, add:

```html
{% if l.can_sharepoint %}
<section aria-labelledby="sp-access-h">
  <h2 id="sp-access-h">SharePoint folder access</h2>
  <p>After each sync, PDMS gives every department's synced staff read access on that department's SharePoint folder and removes it when they leave. Status and anything that needs attention: <a href="/admin/sharepoint">Administration → SharePoint</a>.</p>
</section>
{% endif %}
```

(f) Document page notice. In `crates/pdms-web/src/routes/documents.rs`:
- add `sp_flags: Vec<(i32, String)>,` to `struct ShowPage` after `l: Layout,`;
- in `show`, before `let page = ShowPage {`, add `let sp_flags = pdms_core::sharepoint::status::document_flags(&core.db, id).await?;`;
- add `sp_flags,` to the `ShowPage { … }` literal next to `collab,`.

In `crates/pdms-web/templates/documents/show.html`, directly before the line `  <div class="toolbar">` inside `doc-header`, add:

```html
  {% if !sp_flags.is_empty() %}
  <div class="flash flash-error" role="status">
    {% for f in sp_flags %}The file of version {{ f.0 }} was {% if f.1 == "missing" %}deleted{% else %}changed{% endif %} directly in SharePoint. {% endfor %}
    An administrator has been notified and will restore it or accept the change.
  </div>
  {% endif %}
```

- [x] **Step 5: Run the tests and confirm they pass.**

```bash
cargo test -p pdms-web --test sharepoint_admin
cargo test -p pdms-web
```

Expected: `sharepoint_panel_is_for_administrators ... ok`, and every `pdms-web` binary reports `test result: ok.`

- [x] **Step 6: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
git add crates/pdms-core/src/sharepoint/status.rs crates/pdms-web .sqlx
git commit -m "web: Administration -> SharePoint status and resolution, document-page notice"
```

---
### Task 8: Installer and Docker Compose

P2 left the Compose secret wiring for the Graph certificate, and the installer step, to this plan.

**Files:**
- Modify: `docker-compose.yml`
- Modify: `install.sh`
- Modify: `.gitignore`

**Interfaces:**
- Consumes:
  - P2's `[graph]` (`tenant_id`, `client_id`, `certificate` via `PDMS_GRAPH__CERTIFICATE_FILE`) and `[entra_sync] enabled`. P2's validation refuses `entra_sync.enabled` without `auth.oidc.issuer_url`, and refuses a partial `[graph]`.
  - P3's `[sharepoint]` (`site_url`, `library`) and `[storage] backend = "sharepoint"`.
- Produces:
  - `./install.sh --configure sharepoint` (also asked on a first install);
  - the variables `PDMS_INSTALL_SHAREPOINT_SITE_URL`, `PDMS_INSTALL_GRAPH_TENANT_ID`, `PDMS_INSTALL_GRAPH_CLIENT_ID`, `PDMS_INSTALL_ENTRA_SYNC` for unattended installs;
  - the Docker secret `graph_certificate`;
  - `config/pdms.env`, which sets `PDMS_STORAGE__BACKEND=sharepoint`;
  - `config/pdms-service.cer`, the public certificate to upload to Entra.

- [ ] **Step 1: Compose.** In `docker-compose.yml`:

(a) In the `x-pdms` anchor's `environment: &pdms-env` map, after `PDMS_GRAPH_CALENDAR__CLIENT_SECRET_FILE: …`, add:

```yaml
    PDMS_GRAPH__CERTIFICATE_FILE: /run/secrets/graph_certificate
```

(b) In the `x-pdms` anchor, after the `environment` map and before `volumes:`, add:

```yaml
  env_file:
    # Written by ./install.sh --configure sharepoint: PDMS_STORAGE__BACKEND=sharepoint.
    # (The [storage] table in config/pdms.toml belongs to the ClamAV block.)
    - path: ./config/pdms.env
      required: false
```

(c) Append `graph_certificate` to the `secrets: […]` list of the `x-pdms` anchor and to the `migrate` service's `secrets: […]` list.

(d) Under the top-level `secrets:`, after `graph_client_secret: …`, add:

```yaml
  graph_certificate: { file: ./secrets/graph_certificate }
```

- [ ] **Step 2: The installer's prerequisites and bookkeeping.** In `install.sh`:

(a) In the header comment, change the `--configure` list to:

```bash
#                                    network, database-port, source, sso, smtp, sharepoint,
#                                    outlook, ocr, clamav, admin
```

(b) In the `--help` text, after the `PDMS_INSTALL_OUTLOOK_MAILBOX …` line, add:

```
  PDMS_INSTALL_SHAREPOINT_SITE_URL (blank: document files stay on this server),
  PDMS_INSTALL_GRAPH_TENANT_ID, PDMS_INSTALL_GRAPH_CLIENT_ID, PDMS_INSTALL_ENTRA_SYNC (yes | no, default yes),
  PDMS_INSTALL_STOP_SHAREPOINT (yes: a blank site URL switches an existing install back to local files)
```

(c) In the prerequisites, after the `docker compose version …` line, add:

```bash
compose_version=$(docker compose version --short 2>/dev/null || echo 0)
printf '%s\n' 2.24.0 "${compose_version#v}" | sort -V -C \
  || fail "Docker Compose 2.24 or newer is required (found $compose_version)."
```

(d) Replace `settings_hash` so a storage change restarts the services:

```bash
settings_hash() { { cat "$CONFIG" config/Caddyfile config/pdms.env secrets/* .env 2>/dev/null || true; } | openssl dgst -sha256 | awk '{print $NF}'; }
```

(e) In section 4, change `for f in oidc_client_secret smtp_password graph_client_secret; do` to:

```bash
for f in oidc_client_secret smtp_password graph_client_secret graph_certificate; do
```

(f) In the final summary, change the `Change one part later:` line to:

```bash
   Change one part later: ./install.sh --configure sso|smtp|sharepoint|outlook|ocr|clamav|network|database-port|source|admin
```

- [ ] **Step 3: The SharePoint section.** Insert directly before the line `# --- 7. Outlook calendar (Microsoft Graph) ----…`. The block uses nested here-documents ending in `EOF`, matching the rest of `install.sh`.

```bash
# --- 6b. SharePoint document storage and Entra user sync (Microsoft Graph) -------------
if needs sharepoint sharepoint; then
  say "SharePoint document storage and Entra user sync (optional)"
  note "PDMS can keep every document file in a dedicated SharePoint site and keep people and"
  note "their departments in step with Entra ID (the Department field). Set up the site and the"
  note "\"PDMS Service\" app registration first: docs/SHAREPOINT_SETUP.md."
  note "Leave the site URL blank to keep files on this server."
  was_sharepoint=no
  [[ -f config/pdms.env ]] && grep -qx 'PDMS_STORAGE__BACKEND=sharepoint' config/pdms.env && was_sharepoint=yes
  ask SHAREPOINT_SITE_URL "SharePoint site URL (e.g. https://contoso.sharepoint.com/sites/PDMSRecords)" ""
  SHAREPOINT_SITE_URL=${SHAREPOINT_SITE_URL%/}
  if [[ -n "$SHAREPOINT_SITE_URL" ]]; then
    [[ "$SHAREPOINT_SITE_URL" =~ ^https://[A-Za-z0-9.-]+\.sharepoint\.(com|us)/sites/[A-Za-z0-9._-]+$ ]] \
      || fail "the site URL must look like https://<tenant>.sharepoint.com/sites/<name>"
    sso_tenant=$(sed -n 's#^issuer_url = "https://login.microsoftonline.com/\([^/]*\)/v2.0"#\1#p' "$CONFIG" | head -1)
    ask GRAPH_TENANT_ID "Directory (tenant) ID" "$sso_tenant"
    ask GRAPH_CLIENT_ID "Application (client) ID of the PDMS Service app" ""
    guid='^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
    [[ "$GRAPH_TENANT_ID" =~ $guid && "$GRAPH_CLIENT_ID" =~ $guid ]] \
      || fail "the tenant and client IDs are GUIDs like 01234567-89ab-cdef-0123-456789abcdef"
    if [[ ! -s secrets/graph_certificate ]]; then
      # RSA-2048, as Entra and P2's client assertion (PS256) expect. Unencrypted
      # PKCS#8 key; it stays in secrets/ and is mounted as a Docker secret.
      tmpd=$(mktemp -d)
      openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 730 -subj "/CN=PDMS Service" \
        -keyout "$tmpd/key.pem" -out "$tmpd/cert.pem" 2>/dev/null
      cat "$tmpd/cert.pem" "$tmpd/key.pem" > secrets/graph_certificate
      cp "$tmpd/cert.pem" config/pdms-service.cer
      rm -rf "$tmpd"
      echo "   generated secrets/graph_certificate (valid for 2 years)"
    fi
    chmod 644 secrets/graph_certificate
    [[ -s config/pdms-service.cer ]] || openssl x509 -in secrets/graph_certificate -out config/pdms-service.cer
    chmod 644 config/pdms-service.cer
    thumb=$(openssl x509 -in config/pdms-service.cer -noout -fingerprint -sha1 | sed 's/^.*=//; s/://g')
    expires=$(openssl x509 -in config/pdms-service.cer -noout -enddate | sed 's/^notAfter=//')
    note "Upload config/pdms-service.cer to the PDMS Service app registration"
    note "(Certificates & secrets -> Certificates). Thumbprint $thumb, expires $expires."
    if [[ -n "$sso_tenant" ]]; then
      yes_no ENTRA_SYNC "Sync people and their departments from Entra ID every night?" y
    else
      note "Entra user sync needs Entra ID sign-in first (--configure sso); it stays off for now."
      ENTRA_SYNC=no
    fi
    sync=false; [[ $ENTRA_SYNC == yes ]] && sync=true
    for t in graph sharepoint entra_sync; do
      if has_table "$t" && ! has_block sharepoint; then
        remove_table "$t"; note "replaced the hand-written [$t] settings with the ones above"
      fi
    done
    write_block sharepoint <<EOF
[graph]
# The "PDMS Service" app registration (docs/SHAREPOINT_SETUP.md). Certificate and
# key: secrets/graph_certificate (Docker secret). Public part for Entra:
# config/pdms-service.cer.
tenant_id = $(toml "$GRAPH_TENANT_ID")
client_id = $(toml "$GRAPH_CLIENT_ID")

[sharepoint]
# Document files live here; config/pdms.env selects it (PDMS_STORAGE__BACKEND=sharepoint).
site_url = $(toml "$SHAREPOINT_SITE_URL")
library = "Documents"

[entra_sync]
enabled = $sync
EOF
    printf '# Written by ./install.sh --configure sharepoint.\nPDMS_STORAGE__BACKEND=sharepoint\n' > config/pdms.env
    chmod 644 config/pdms.env
    echo "   SharePoint storage configured. Check it under Administration -> SharePoint once running."
  else
    if [[ $was_sharepoint == yes ]]; then
      warn "Files already stored in SharePoint stay there, but PDMS will no longer be able to open them."
      warn "Entra user sync, set up in this same step, stops too."
      yes_no STOP_SHAREPOINT "Stop using SharePoint for document files anyway?" n
      [[ $STOP_SHAREPOINT == yes ]] || fail "keeping SharePoint storage; nothing was changed"
    fi
    # Only [sharepoint]: hand-written [graph] and [entra_sync] settings (Entra sync
    # without SharePoint storage, ADMIN_GUIDE) stay as they are.
    has_table sharepoint && ! has_block sharepoint && remove_table sharepoint
    write_block sharepoint <<'EOF'
[sharepoint]
# Not set up: document files stay on this server. Run ./install.sh --configure sharepoint
# (see docs/SHAREPOINT_SETUP.md).
site_url = ""
EOF
    rm -f config/pdms.env
  fi
fi
```

- [ ] **Step 4: Git-ignore the generated files.** In `.gitignore`, under `/config/Caddyfile`, add:

```
/config/pdms.env
/config/pdms-service.cer
```

- [ ] **Step 5: Check the scripts.**

```bash
bash -n install.sh && echo syntax-ok
./install.sh --help | grep -c PDMS_INSTALL_SHAREPOINT_SITE_URL
tmp=$(mktemp -d) && mkdir -p "$tmp/secrets" "$tmp/config" \
  && for s in pg_superuser_password pg_owner_password pg_app_password db_owner_url db_app_url pdms_secret_key \
       oidc_client_secret smtp_password graph_client_secret graph_certificate; do : > "$tmp/secrets/$s"; done \
  && cp docker-compose.yml "$tmp/" \
  && docker compose -f "$tmp/docker-compose.yml" --project-directory "$tmp" config --quiet && echo compose-ok; rm -rf "$tmp"
```

Expected: `syntax-ok`, `1`, `compose-ok`.

- [ ] **Step 6: Commit.**

```bash
git add docker-compose.yml install.sh .gitignore
git commit -m "install: --configure sharepoint, Graph certificate secret, storage via config/pdms.env"
```

---
### Task 9: Setup guide, documentation, live tests, and final verification

**Files:**
- Create: `docs/SHAREPOINT_SETUP.md`
- Modify: `docs/ADMIN_GUIDE.md` (§4 "File storage and virus scanning" table; P2's "Entra ID user sync" subsection; new "### SharePoint storage" in §12)
- Modify: `docs/BACKUP.md` ("What to protect")
- Modify: `docs/ARCHITECTURE.md` (§12 data-model table)
- Modify: `docs/DECISIONS.md` (new entries before `## Open questions`; statuses of D-081 to D-086 and D-089; the open-questions row)
- Modify: `docs/ROADMAP.md` (§0.3)
- Modify: `docs/superpowers/plans/2026-10-14-pdms-sharepoint-entra-index.md` ("Produced by P4")
- Modify: `crates/pdms-core/tests/sharepoint.rs` (P3's file: shared live-test setup and two opt-in live tests)

**Interfaces:**
- Consumes: everything above; P3's live test `live_sharepoint_round_trip`, its variables `PDMS_LIVE_TENANT_ID`, `PDMS_LIVE_CLIENT_ID`, `PDMS_LIVE_CERTIFICATE_FILE`, `PDMS_LIVE_SITE_URL`, and its test helpers `core_with`, `new_department`, `editing_department`, `as_user`, `pdf`, `new_doc`, `external_id`.
- Produces: the county runbook; `live_pdms_writes_are_recognised` and `live_department_staff_get_read_access` (ignored by default); the extra variable `PDMS_LIVE_READER_EMAIL`.

- [ ] **Step 1: The county setup guide.** Create `docs/SHAREPOINT_SETUP.md`:

````markdown
# Setting up SharePoint for PDMS

PDMS keeps every document file in one dedicated SharePoint site, and reads
people and their departments from Entra ID. This guide is for the county's
Microsoft 365 administrator.

- **Part A** can be done at any time.
- **Part B** is done when PDMS is ready to connect.
- **Part C** covers looking after the connection.

Do everything for the **test** site and test server first.

| | Test | Production |
|---|---|---|
| Site | `PDMS Records – Test` | `PDMS Records` |
| App registration | `PDMS Service – Test` | `PDMS Service` |
| PDMS server | the test server | the production server |

Use a separate app registration for each server, so the test server's
certificate can never reach production files.

## Part A: the site

### A1. Create the site

In the SharePoint admin center, choose **Sites → Active sites → Create →
Communication site**:

- name `PDMS Records – Test` (production: `PDMS Records`);
- design **Blank**;
- language English.

Note the site address, for example
`https://contoso.sharepoint.com/sites/PDMSRecordsTest`. The PDMS installer
asks for it.

### A2. Permissions

On the site, open **Settings (gear) → Site permissions → Advanced permissions
settings**:

- **Owners:** one or two named administrators. These are the only people who
  can change files directly; PDMS watches for that (step B9).
- **Members** and **Visitors:** remove everyone. Leave the groups empty.

Do not add people or groups anywhere else on the site. PDMS gives each
department's staff read access to their own department folder itself.

### A3. Sharing

- **Site permissions → Change how members can share:** choose "Only site
  owners can share files, folders, and the site", and turn **Allow access
  requests** off.
- **SharePoint admin center → Active sites → the site → Settings → External
  file sharing:** "Only people in your organization".

### A4. The document library

In **Documents → Settings (gear) → Library settings → Versioning settings**:

- **Create major versions:** yes (keep the default limit or higher);
- **Require content approval:** no;
- **Require check out:** no.

Leave the library named `Documents`.

### A5. The three top-level folders

In `Documents`, create these folders, spelled exactly:

- `Departments`
- `County-wide`
- `Restricted`

On `County-wide` only: choose **… → Manage access → Advanced settings →
Stop inheriting permissions**, then **Grant permissions** to **Everyone
except external users** with **Read**.

Leave `Departments` and `Restricted` inheriting (owners only). PDMS creates
the department and document folders itself. If a top-level folder is missing,
PDMS creates it and logs a warning; `County-wide` then still needs the
permission change above.

### A6. Retention (recommended)

In Microsoft Purview, **Data lifecycle management → Retention policies → New
retention policy**: static scope, location **SharePoint classic and
communication sites**, include only this site, and retain items for the
county's records period (or forever). Files deleted from the site can then
always be recovered.

### A7. The Department field

PDMS puts each person in a department from the **Department** field of their
Entra account. Make sure every staff account has one consistent value per
department, and make the field part of account creation. To list the values
in use (Microsoft Graph PowerShell):

```powershell
Connect-MgGraph -Scopes "User.Read.All"
Get-MgUser -All -Filter "accountEnabled eq true and userType eq 'Member'" -Property Department |
  Group-Object Department | Sort-Object Count -Descending | Select-Object Count, Name
```

## Part B: connecting PDMS

### B1. Register the app

In the Entra admin center, choose **Applications → App registrations → New
registration**:

- name `PDMS Service – Test` (production: `PDMS Service`);
- **Accounts in this organizational directory only**;
- no redirect URI.

Note the **Application (client) ID** and the **Directory (tenant) ID**.

### B2. API permissions

Under **API permissions → Add a permission → Microsoft Graph → Application
permissions**, add:

- `Sites.Selected`
- `User.Read.All`

Then choose **Grant admin consent**. Do not add `Sites.ReadWrite.All` or
`Files.ReadWrite.All`: `Sites.Selected` limits PDMS to the one site you grant
in B5.

### B3. Run the installer on the PDMS server

```sh
./install.sh --configure sharepoint
```

It asks for the site address, the tenant ID (it suggests the one used for
sign-in), and the client ID. It then:

- creates the certificate PDMS signs in with: `secrets/graph_certificate`
  (private; never copy it off the server except in backups);
- writes the public part to `config/pdms-service.cer` and prints its
  thumbprint and expiry date;
- asks whether to sync people and departments from Entra ID every night
  (needs Entra ID sign-in, `--configure sso`, first);
- restarts PDMS.

Copy `config/pdms-service.cer` to your workstation, for example with `scp`.

### B4. Upload the certificate

In the app registration, choose **Certificates & secrets → Certificates →
Upload certificate** and pick `pdms-service.cer`. Check that the thumbprint
matches the one the installer printed.

### B5. Give the app full control of this one site

PDMS needs full control so it can create the library columns and folders and
grant and remove read access on department folders. The grant is made in two
steps, because Microsoft Graph creates the permission first and then raises
it. Sign in as a Global Administrator or SharePoint Administrator. Use one of
the two methods.

**Microsoft Graph PowerShell** (module `Microsoft.Graph` 2.x):

```powershell
Connect-MgGraph -Scopes "Sites.FullControl.All" -TenantId "<tenant-id>"
$site = Get-MgSite -SiteId "contoso.sharepoint.com:/sites/PDMSRecordsTest"
$perm = New-MgSitePermission -SiteId $site.Id -BodyParameter @{
  roles = @("write")
  grantedToIdentities = @(@{ application = @{ id = "<client-id>"; displayName = "PDMS Service – Test" } })
}
Update-MgSitePermission -SiteId $site.Id -PermissionId $perm.Id -BodyParameter @{ roles = @("fullcontrol") }
Get-MgSitePermission -SiteId $site.Id | Format-List Id, Roles, GrantedToIdentitiesV2
```

The same calls in raw Microsoft Graph (Graph Explorer, signed in with
`Sites.FullControl.All`):

```http
GET https://graph.microsoft.com/v1.0/sites/contoso.sharepoint.com:/sites/PDMSRecordsTest?$select=id

POST https://graph.microsoft.com/v1.0/sites/{site-id}/permissions
{"roles":["write"],"grantedToIdentities":[{"application":{"id":"<client-id>","displayName":"PDMS Service – Test"}}]}

PATCH https://graph.microsoft.com/v1.0/sites/{site-id}/permissions/{permission-id}
{"roles":["fullcontrol"]}
```

**PnP PowerShell** (needs its own app registration for sign-in, see the PnP
documentation):

```powershell
Connect-PnPOnline -Url "https://contoso-admin.sharepoint.com" -Interactive -ClientId "<pnp-app-id>"
$p = Grant-PnPAzureADAppSitePermission -AppId "<client-id>" -DisplayName "PDMS Service – Test" `
       -Permissions Write -Site "https://contoso.sharepoint.com/sites/PDMSRecordsTest"
Set-PnPAzureADAppSitePermission -Site "https://contoso.sharepoint.com/sites/PDMSRecordsTest" `
  -PermissionId $p.Id -Permissions FullControl
```

Recent PnP releases name these `Grant-PnPEntraIDAppSitePermission` and
`Set-PnPEntraIDAppSitePermission`.

To disconnect PDMS later, remove this permission
(`Remove-MgSitePermission -SiteId $site.Id -PermissionId <id>`).

### B6. Check the connection

In PDMS, open **Administration → SharePoint** and choose **Check now**. On
its first connection PDMS creates the library columns (Document ID, Document
Type, Tags, Status, Visibility, Department, Owner, the four dates, Version,
Current, Open in PDMS) and checks the three folders. The page should show the
run as successful within a minute.

If it shows "PDMS cannot sign in to Microsoft Graph", check, in order:

1. the certificate upload (B4);
2. admin consent (B2);
3. the site grant (B5).

### B7. Library views (by hand)

Microsoft Graph cannot create views, so add this one by hand once the columns
exist. In `Documents`, open the view menu (**All Documents**) and choose
**Create new view**:

- name **Current documents**, type **List**, **Make this the default view**;
- then **Edit current view**:
  - **Columns:** Name, Document ID, Title, Document Type, Department, Status,
    Review Date, Expiration Date, Open in PDMS;
  - **Sort:** Review Date, ascending;
  - **Filter:** show items only when **Current** is equal to **Yes**;
  - **Folders:** **Show all items without folders**.

The existing **All Documents** view shows every version; you may rename it
**All versions**. "Open in PDMS" is plain text, because Microsoft Graph cannot
write link columns: copy it into the browser.

### B8. Departments

If Entra user sync is on, open **Administration → Entra ID sync**, map each
Department value from A7 to a PDMS department, and choose **Sync now**.
Within about a minute PDMS gives each department's staff read access to
`Departments/<department>`. **Administration → SharePoint** shows the run and
anyone SharePoint could not resolve (for example, an account without a
mailbox).

### B9. Prove the change check (test site only)

1. Upload a document in PDMS.
2. In SharePoint, as a site owner, open its file and change it.
3. Within about five minutes (`[sharepoint] delta_interval_minutes`), PDMS
   flags the document, and administrators get "SharePoint: 1 file(s) changed
   outside PDMS".
4. On **Administration → SharePoint**, choose **Restore original**. The flag
   clears.
5. On the server, run:
   `docker compose run --rm app verify-integrity --rehash-sharepoint`.
   It should report no problems.

## Part C: looking after it

### C1. Renew the certificate (every two years)

The installer prints the expiry date; set a reminder 30 days before it. To
renew without an outage, add the new certificate to the app before PDMS uses
it:

```sh
cd /path/to/pdms
d=$(mktemp -d secrets/renew.XXXX)
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 730 -subj "/CN=PDMS Service" \
  -keyout "$d/key.pem" -out "$d/cert.pem"
# Upload $d/cert.pem to the app registration now (B4). Keep the old certificate there.
cat "$d/cert.pem" "$d/key.pem" > secrets/graph_certificate && chmod 644 secrets/graph_certificate
cp "$d/cert.pem" config/pdms-service.cer && rm -rf "$d"
docker compose up -d --force-recreate app worker
```

Then choose **Check now** on **Administration → SharePoint**, and delete the
old certificate from the app registration.

### C2. When something looks wrong

| You see | Likely cause | What to do |
|---|---|---|
| "PDMS cannot sign in to Microsoft Graph" (at most hourly) | certificate expired or not uploaded, consent or site grant removed | B4, B2, B5; then **Check now** |
| "File storage (SharePoint) is unavailable right now" for users | Microsoft 365 outage or throttling | wait; nothing was saved or changed |
| Someone cannot open their department folder in SharePoint | blank or unmapped Department, a local (non-Entra) account, or no mailbox | A7, B8; the error list on **Administration → SharePoint** |
| "Other access on department folders" | someone shared a folder directly in SharePoint | remove it in SharePoint if it was not intended; PDMS never removes it |
| "Files PDMS does not track" | someone added a file in SharePoint, or an upload failed half-way | delete it in SharePoint if it does not belong, then **Dismiss** |
````

Run: `grep -c '^### ' docs/SHAREPOINT_SETUP.md`
Expected: `18`.

- [ ] **Step 2: The administrator guide.** In `docs/ADMIN_GUIDE.md`:

(a) In the "### File storage and virus scanning" table, after the `[storage] backend` row, add:

```markdown
| `[storage] backend = "sharepoint"` | | Document files in a SharePoint site. Set up with [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md) and `./install.sh --configure sharepoint`, which selects it in `config/pdms.env` (`PDMS_STORAGE__BACKEND=sharepoint`) and writes `[graph]`, `[sharepoint]`, and `[entra_sync]`. See "SharePoint storage" in §12 |
```

(b) In P2's "### Entra ID user sync" subsection, directly above `**Setup.**`, add:

```markdown
With SharePoint storage, `./install.sh --configure sharepoint` does the
setup below for you (see [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md)). The
manual steps are for Entra sync without SharePoint storage.
```

(c) In "## 12. Operations", before "### Backups", insert:

```markdown
### SharePoint storage

With document files in SharePoint ([SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md)),
**Administration → SharePoint** (administrators only) shows:

- **Change check:** PDMS reads the site's change feed every
  `[sharepoint] delta_interval_minutes` (default 5) and shows the last run,
  its result, and any error. **Check now** runs it at once.
- **Department folder access:** department members whose account comes from
  Entra ID get read access to `Departments/<department>` in SharePoint. PDMS
  updates it about a minute after a membership change, after each Entra
  sync, and every night. **Update access now** runs it at once. People
  SharePoint could not resolve (for example, no mailbox) are listed with the
  error. Local accounts, such as the break-glass administrator, never get
  SharePoint access.
- **Other access on department folders:** access that PDMS did not grant
  (someone shared the folder in SharePoint). PDMS reports it and never
  removes it.
- **Files changed outside PDMS:** a file whose size or content hash no longer
  matches what PDMS uploaded, or that was deleted. The document page shows a
  notice too. Administrators get one notice per check that finds changes.
  The title and path are hidden from administrators who cannot view the
  document.
- **Files PDMS does not track:** files added in SharePoint, listed once they
  are ten minutes old (uploads in progress are not listed). PDMS never deletes
  them; delete them in SharePoint if they do not belong, then choose
  **Dismiss**.
- **Failed jobs:** SharePoint jobs that gave up, with their last error.

Resolving a file changed outside PDMS (each action is audited):

| Action | What it does | Who |
|---|---|---|
| **Restore original** | Puts back the SharePoint version whose content matches PDMS's record (checked by SHA-256). Renames and moves are put back too | administrators |
| **Accept as new version** | Downloads the file as it is now, runs the normal upload checks (file type, virus scan), and adds it as a new PDMS version with a note. The original stays in the history | administrators with edit permission on the document |
| **Re-check** | Checks the file again, for example after you restored a deleted file from the site's recycle bin, and clears the flag if it matches | administrators |

Renamed or moved files and folders are put back in PDMS's layout
automatically, without a flag. Users never see a changed file: downloads are
checked against PDMS's SHA-256 and stop with an error if they differ.

If PDMS cannot sign in to Microsoft Graph (expired certificate, removed
consent or site grant), administrators get a notice at most once an hour, and
uploads and downloads fail with "File storage (SharePoint) is unavailable".
See [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md) C2.

`verify-integrity` checks SharePoint files through Microsoft Graph: present
and the right size (`--skip-files`), and by default also the QuickXorHash
SharePoint reports. `--rehash-sharepoint` downloads every file and compares
its SHA-256 (slow).
```

- [ ] **Step 3: Backups.** In `docs/BACKUP.md`, "## What to protect":

(a) After the **File store** row, add:

```markdown
| **SharePoint site** (with SharePoint storage) | the `PDMS Records` site | Every document file. Not copied by `pdms-backup.sh`: protected by library versioning, the recycle bin, and the Purview retention policy ([SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md) A4, A6). Include the site in your Microsoft 365 backup if you have one |
```

(b) In the **Secrets** row, after "Also the database passwords and the SSO/SMTP secrets", add: ", and `graph_certificate` (without it, make a new one and upload it to the app registration: SHAREPOINT_SETUP.md C1)".

(c) In the **Configuration** row, change `` `config/pdms.toml` (and `.env` if you use one) `` to `` `config/pdms.toml`, `config/pdms.env` (and `.env` if you use one) ``.

(d) After the "**Consistency rule**" paragraph, add:

```markdown
With SharePoint storage the same rule holds: PDMS uploads a file before the
database rows that point at it are committed. After restoring an older
database dump, files uploaded since the dump stay in SharePoint and appear
under **Administration → SharePoint → Files PDMS does not track**.
```

- [ ] **Step 4: Architecture.** In `docs/ARCHITECTURE.md` §12, after the phase-5 row, add:

```markdown
| SharePoint and Entra (`0010_entra_sync`, `0013_sharepoint_files`, `0014_sharepoint_access`; plans P2–P4) | `user_source` enum and users `source`, `entra_department`, `entra_synced_at`; department_entra_values, sync_cursors; sharepoint_department_folders, sharepoint_document_folders, sharepoint_files (incl. `state`, `changed_by`, `changed_at`); sharepoint_folder_access, sharepoint_foreign_grants, sharepoint_unexpected_items |
```

- [ ] **Step 5: Decision records.** Find the last decision number:

```bash
grep -oE '^### D-[0-9]+' docs/DECISIONS.md | tail -1
```

Expected: `### D-103` (P3's). If it is higher, renumber the entries below from the next free number. Before `## Open questions`, add:

```markdown
### D-104 Department folder access: one read grant per person, Entra accounts only — Accepted
Entra security groups are not available to PDMS (D-086), so each member of a
department gets their own `read` permission on `Departments/<department>`
through the Graph invite call (`requireSignIn`, no invitation email). Only
active members whose account comes from Entra get one; local break-glass
accounts never do. PDMS records each grant and removes only what it
recorded. Other access on the folder is reported and never changed. Runs are
coalesced to one per minute after membership changes, follow each Entra
sync, and repeat nightly. Permissions SharePoint copies from the site when a
folder's inheritance breaks are not reported.

### D-105 Changes in SharePoint are found by comparing file hashes — Accepted
A tracked file counts as changed when its size or QuickXorHash in the change
feed differs from what PDMS recorded at upload. `lastModifiedBy` is shown and
audited but never trusted: it decides only whether a rename or a column
change needs PDMS's layout put back. An expired change token (HTTP 410)
rereads everything; unchanged files are not flagged.

### D-106 Files PDMS does not track: listed after ten minutes, never deleted — Accepted
An upload in progress has no database row until it commits, so untracked
files are listed only once they are ten minutes old. PDMS never deletes them;
an administrator deletes them in SharePoint or dismisses them (audited).

### D-107 SharePoint administration: `ManageSharePoint` — Accepted
A new capability, held by the org-wide administrator role, gates the status
page and the restore, re-check, and dismiss actions. "Accept as new version"
also needs `edit` on the document, through the normal version check, because
it adds content. The status page hides the title and path of documents the
administrator cannot view. "Restore original" picks the SharePoint version by
SHA-256, never by its label.

### D-108 The installer selects SharePoint storage through `config/pdms.env` — Accepted
The ClamAV installer block already owns the `[storage]` table, so
`--configure sharepoint` writes `PDMS_STORAGE__BACKEND=sharepoint` to
`config/pdms.env`, which Docker Compose loads as an optional `env_file`
(Compose 2.24 or newer). The Graph certificate is always mounted as the
`graph_certificate` secret; an empty file means "not configured". The
installer creates the certificate with `openssl`, because the PDMS image is
not built yet at that point.

### D-109 `verify-integrity --rehash-sharepoint` — Accepted
By default `verify-integrity` checks SharePoint files through Graph metadata
(present, size, QuickXorHash) without downloading them. Downloading every file
and comparing SHA-256 is opt-in, because it can take hours on a large
library.

### D-110 Graph sign-in failures: one notice an hour — Accepted
HTTP 401/403 from Microsoft Graph means the certificate, consent, or site
grant is broken, and every run fails the same way. Administrators get one
in-app (and email) notice per hour, and the status page shows the latest
error. The notice names no document.
```

Then:

```bash
sed -i -E '/^### D-08[1-6] |^### D-089 /s/ — Accepted \(design, not built\)$/ — Accepted/' docs/DECISIONS.md
grep -nE '^### D-08[1-9]' docs/DECISIONS.md
```

Expected: D-081 to D-086 and D-089 end in "— Accepted". D-087 and D-088 are unchanged (sub-project 3).

In the open-questions table, change the "SharePoint/OneDrive storage" row's answer to: `Decided and built: SharePoint holds the files (D-081 to D-084, D-089, D-104 to D-110). Setup: SHAREPOINT_SETUP.md`.

- [ ] **Step 6: Roadmap.** In `docs/ROADMAP.md` §0.3:

(a) Below P2's `2a` row, add:

```markdown
| 1b | **SharePoint access, change detection, and operations** (plan P4) | Per-person read access on department folders; detecting and resolving changes made in SharePoint; `verify-integrity` for SharePoint; Administration → SharePoint; installer; county setup guide | **Done** | [P4 plan](superpowers/plans/2026-10-14-p4-sharepoint-access-changes-ops.md) |
```

(b) Set sub-project 1's status to `**Built** (P1, P3, P4); live test on the county's test site pending` and sub-project 2's to `**Built** (P2, P4); live test pending`.

(c) In "**Waiting on the county**", change "the steps are in Appendix A of the spec" to "the steps are in [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md)".

- [ ] **Step 7: The plan index.** In `docs/superpowers/plans/2026-10-14-pdms-sharepoint-entra-index.md`, replace the "Produced by P4" table's rows with:

```markdown
| Jobs | `sharepoint::RECONCILE_JOB = "sharepoint.reconcile_access"` (defined in `sharepoint::access`); `sharepoint::DELTA_JOB = "sharepoint.delta"` (defined in `sharepoint::changes`); both registered by `sharepoint::register_ops_jobs(worker, core)` |
| Run records | `sync_cursors` rows `sharepoint.delta` (with the delta link) and `sharepoint.reconcile` |
| DB | `0014_sharepoint_access.sql`: `sharepoint_folder_access`, `sharepoint_foreign_grants`, `sharepoint_unexpected_items`, `sharepoint_files.changed_by/changed_at` |
| Authz | `Capability::ManageSharePoint` (org-wide `admin` role); notifications `dispatch::Visibility::Anyone` |
| Admin | `/admin/sharepoint` (in `doc_admin_routes`) status panel and resolution; P2's `/admin/entra` gains a SharePoint access section; a notice on the document page |
| CLI | `pdms verify-integrity --rehash-sharepoint`; `IntegrityReport.sharepoint_files_checked` replaces P3's `sharepoint_files_skipped` |
| Install | `./install.sh --configure sharepoint`; Compose secret `graph_certificate`; `config/pdms.env` |
| Docs | `docs/SHAREPOINT_SETUP.md`; ADMIN_GUIDE "SharePoint storage"; D-104 to D-110 |
```

and below the table add the "Deviations from the index and the spec" list from the end of this plan.

- [ ] **Step 8: Opt-in live tests.** In `crates/pdms-core/tests/sharepoint.rs` (P3's file):

(a) Above `live_sharepoint_round_trip`, add the shared setup:

```rust
/// A `Core` connected to the real SharePoint **test** site named by the
/// `PDMS_LIVE_*` variables (see `live_sharepoint_round_trip`).
async fn live_core() -> (Core, pdms_testkit::TestDb) {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let graph = pdms_core::config::GraphConfig {
        tenant_id: var("PDMS_LIVE_TENANT_ID"),
        client_id: var("PDMS_LIVE_CLIENT_ID"),
        certificate: pdms_core::config::Secret::new(
            std::fs::read_to_string(var("PDMS_LIVE_CERTIFICATE_FILE")).unwrap(),
        ),
        ..Default::default()
    };
    let site = var("PDMS_LIVE_SITE_URL");
    core_with(|c| {
        c.graph = graph;
        c.storage.backend = StorageBackend::SharePoint;
        c.sharepoint.site_url = site;
    })
    .await
}

/// A department code no earlier live run used.
fn live_code() -> String {
    let id = Uuid::new_v4().simple().to_string();
    format!("LIVE{}", id[..6].to_uppercase())
}
```

(b) In `live_sharepoint_round_trip`, replace everything from `let var = |k: &str|` through the `let code = format!(…);` line with:

```rust
    let (core, _db) = live_core().await;
    let code = live_code();
```

(c) After `live_sharepoint_round_trip`, add:

```rust
/// PDMS's own folders, files, and column values must not look like changes
/// made outside PDMS. This checks how SharePoint reports the app in
/// `lastModifiedBy` (D-105). Same variables as `live_sharepoint_round_trip`.
///
/// ```text
/// cargo test -p pdms-core --test sharepoint live_pdms_writes_are_recognised -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "needs a real SharePoint test site and app registration"]
async fn live_pdms_writes_are_recognised() {
    let (core, _db) = live_core().await;
    // Read the whole change feed once, so only this test's writes follow.
    sharepoint::changes::detect_changes(&core).await.unwrap();
    let code = live_code();
    let (dept, alice) = editing_department(&core, &code, &format!("PDMS live test {code}"), "live@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = pdf("PDMS live change check");
    let id =
        new_doc(&core, &ctx, dept, alice, "Live change check", Visibility::Department, Some(("live.pdf", &bytes))).await;
    sharepoint::mirror_document(&core, id).await.unwrap();

    // SharePoint's change feed can lag behind by a minute or more.
    let mut seen = sharepoint::changes::DeltaReport::default();
    for _ in 0..18 {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let r = sharepoint::changes::detect_changes(&core).await.unwrap();
        seen.items += r.items;
        seen.modified += r.modified;
        seen.missing += r.missing;
        seen.mirrors += r.mirrors;
        if seen.items >= 3 {
            break;
        }
    }
    assert!(seen.items >= 3, "the feed never listed the department folder, document folder, and file: {seen:?}");
    assert_eq!((seen.modified, seen.missing, seen.mirrors), (0, 0, 0), "PDMS's own writes were reported: {seen:?}");
    assert!(sharepoint::status::document_flags(&core.db, id).await.unwrap().is_empty());
    println!("left in SharePoint: Departments/PDMS live test {code}/{} – Live change check", external_id(&core, id).await);
}

/// Read access for a real person. `PDMS_LIVE_READER_EMAIL` is an ordinary
/// member account in the test tenant, not a site owner. This checks that the
/// invite call works with `Sites.Selected` plus full control on the site
/// (D-104), and that SharePoint's copied site permissions are not reported.
///
/// ```text
/// PDMS_LIVE_READER_EMAIL=someone@county.gov … \
/// cargo test -p pdms-core --test sharepoint live_department_staff_get_read_access -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "needs a real SharePoint test site, app registration, and a test account"]
async fn live_department_staff_get_read_access() {
    let reader = std::env::var("PDMS_LIVE_READER_EMAIL").unwrap_or_else(|_| panic!("set PDMS_LIVE_READER_EMAIL"));
    let (core, _db) = live_core().await;
    let code = live_code();
    let dept = new_department(&core, &code, &format!("PDMS live test {code}")).await;
    let person = directory::create_user(&core.db, &system(), &reader, &reader, Some(dept)).await.unwrap();
    sqlx::query("UPDATE users SET source = 'entra' WHERE id = $1")
        .bind(person)
        .execute(&core.db)
        .await
        .unwrap();

    let r = sharepoint::access::reconcile(&core).await.unwrap();
    assert_eq!((r.granted, r.foreign, r.errors.len()), (1, 0, 0), "{r:?}");
    let again = sharepoint::access::reconcile(&core).await.unwrap();
    assert_eq!((again.granted, again.revoked, again.foreign), (0, 0, 0), "{again:?}");

    directory::remove_department_member(&core.db, &system(), person, dept).await.unwrap();
    let r = sharepoint::access::reconcile(&core).await.unwrap();
    assert_eq!((r.revoked, r.errors.len()), (1, 0), "{r:?}");
    println!("left in SharePoint: Departments/PDMS live test {code} (empty)");
}
```

Run: `cargo test -p pdms-core --test sharepoint -- --list --ignored 2>/dev/null | grep live_`
Expected: `live_department_staff_get_read_access: test`, `live_pdms_writes_are_recognised: test`, `live_sharepoint_round_trip: test`.

- [ ] **Step 9: Full verification.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo sqlx prepare --workspace --check -- --all-targets
cargo test --workspace
bash -n install.sh
grep -rn "sharepoint_files_skipped\|mock_sharepoint\|MockSharePoint" crates docs --include=*.rs --include=*.md | grep -v superpowers
```

Expected:
- `fmt` and `clippy` produce no output and exit 0;
- `prepare --check` exits 0;
- every `test result: ok.` line has `0 failed`; the three `live_` tests show as ignored;
- `bash -n` prints nothing;
- the final `grep` prints nothing.

- [ ] **Step 10: Commit.**

```bash
git add docs crates/pdms-core/tests/sharepoint.rs
git commit -m "docs: SharePoint setup guide, admin guide, decisions D-104 to D-110; live SharePoint tests"
```

- [ ] **Step 11: Live test and rollout on the test server (spec §13, step 3).** Needs the county's Part A and Part B. Not a code change.

> **Warning:** step 1 erases the test server's database and files. Confirm with the owner before running it.

1. On the test server: `cd <install directory> && git pull && docker compose down -v`.
2. `./install.sh`. Answer the SharePoint step with the test site, the tenant ID, and the `PDMS Service – Test` client ID. Do SHAREPOINT_SETUP.md B4 to B6.
3. `docker compose run --rm app seed-demo`. Demo files go to the test site.
4. From a workstation with the repository, run the three live tests against the test site. Set the `PDMS_LIVE_*` variables, with `PDMS_LIVE_CERTIFICATE_FILE` pointing at a copy of the test server's `secrets/graph_certificate`. Delete that copy afterwards.
5. Do SHAREPOINT_SETUP.md B7 to B9. B9 covers: edit as a site owner, see the flag and the notice, restore, and run `verify-integrity --rehash-sharepoint`.
6. Work through ROADMAP §1.2. Record the results under ROADMAP §1.3, and set sub-projects 1 and 2 to **Done** when they pass.

---

## Deviations from the index and the spec

Recorded in the index by Task 9, Step 7.

1. **Mock Graph:** the index expected a P4 SharePoint mock. P4 extends P2's `tests/mock_graph` and P3's child module `tests/mock_graph/sp.rs` (`MockGraph`, `SpItem`, `SpState`) instead.
2. **Job constants:** the index has `sharepoint::RECONCILE_JOB` and `sharepoint::DELTA_JOB`. They are defined in `sharepoint::access` and `sharepoint::changes` and re-exported, so the paths in the index still work. Both are registered by the new `sharepoint::register_ops_jobs(worker, core)`, chained after P3's `register_jobs` on the last line of `maintenance::worker()`.
3. **Run records:** the cursor `sharepoint.reconcile` is added beside `sharepoint.delta` in P2's `sync_cursors`.
4. **Database:** the index lists only `sharepoint_folder_access` in `0014_sharepoint_access.sql`. P4 also adds `sharepoint_foreign_grants`, `sharepoint_unexpected_items`, and `sharepoint_files.changed_by/changed_at`.
5. **Authorization:** `Capability::ManageSharePoint` is new, and so is `dispatch::Visibility::Anyone`, for notices that name no document.
6. **Integrity:**
   - The spec's `verify-integrity --rehash` → `--rehash-sharepoint`. `--rehash` stays the local-file option.
   - P3's `IntegrityReport.sharepoint_files_skipped` → `sharepoint_files_checked`. P3's test is renamed to `integrity_check_covers_sharepoint_files`.
   - `integrity::verify` → `verify_with(core, VerifyOptions)`. `verify(core, rehash)` stays as a wrapper.
7. **Storage selection:** the spec's `[storage] backend = "sharepoint"` in `pdms.toml` → `PDMS_STORAGE__BACKEND=sharepoint` in `config/pdms.env` (Compose `env_file`). This needs Docker Compose 2.24 or newer.
8. **Certificate:** the installer creates it with `openssl`, not P2's `pdms gen-graph-cert`, because the image is not built yet at that point. The format is the same: PEM certificate plus PKCS#8 key, RSA-2048, two years. The file is mode 644 inside the 700 `secrets/` directory, like the other Compose secrets. (P2 writes mode 600.)
9. **Admin page:** `/admin/sharepoint` lives in `doc_admin_routes`.
10. **Audit:** one extra action, `sharepoint.unexpected_dismissed`.
11. **Edits to P2 and P3 code:**
    - P2: `entra_sync::run_job` (Ok arm) and `sync_one` queue a folder-access run, and `templates/admin/entra.html` gains a SharePoint access section.
    - P3: the `maintenance::worker()` last line changes.
12. **Spec §7.6, area folders:** an area folder in the change feed counts only when its name changed. SharePoint can list a parent folder whenever something inside it changes, and the setup guide has people create the area folders by hand.
13. **Spec §7.4, copied permissions:** when a folder's inheritance breaks, SharePoint copies the site's permissions onto it. Permissions that match the `Departments` folder's (same grantee and roles) are not reported as other access.
