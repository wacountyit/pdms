# SharePoint storage and Entra sync — plan index

**Spec:** [2026-10-14-sharepoint-storage-entra-sync-design.md](../specs/2026-10-14-sharepoint-storage-entra-sync-design.md) (approved 2026-10-14)

The spec covers several pieces that each produce working, tested software on
their own. They are built as four plans, **in this order**. Each plan lists
the names it produces and consumes, so an implementer can read only their
plan plus this index.

| Plan | File | Delivers | Depends on |
|---|---|---|---|
| P1 | [2026-10-14-p1-document-visibility.md](2026-10-14-p1-document-visibility.md) | Department / County-wide / Restricted visibility in the engine, SQL filter, database, service, web, and API. Replaces "confidential" and "inherit permissions". | — |
| P2 | [2026-10-14-p2-graph-client-entra-sync.md](2026-10-14-p2-graph-client-entra-sync.md) | Microsoft Graph client (certificate auth, retries); Entra user sync from the Department field; department mapping admin; SSO by object ID only | P1 merged (shares migrations numbering only) |
| P3 | [2026-10-14-p3-sharepoint-storage.md](2026-10-14-p3-sharepoint-storage.md) | SharePoint document storage: layout, upload and streamed download, metadata columns, folder moves, synchronous tightening | P1, P2 |
| P4 | [2026-10-14-p4-sharepoint-access-changes-ops.md](2026-10-14-p4-sharepoint-access-changes-ops.md) | Per-person department-folder access, change detection and admin resolution, integrity check, status panel, installer, setup guide | P3 |

**When each plan is done:**

- `cargo fmt`, `clippy -D warnings`, and `cargo test --workspace` pass.
- `.sqlx/` is refreshed.
- `docs/ROADMAP.md` §0 is updated: the status in §0.3, §0.5 and §0.6 in
  the plan's PR; once it merges, the plan moves from §0.1a to §0.1 (on
  `main`).
- The plan's tasks are ticked.

## Refinements to the spec (decided while planning)

These do not change behaviour. They change where things live.

1. **Migrations are split per plan.** The spec's single `0008_sharepoint_entra.sql` becomes:
   - `0008_document_visibility.sql` (P1);
   - `0010_entra_sync.sql` (P2: `department_entra_values`, `sync_cursors`, user columns);
   - `0013_sharepoint_files.sql` (P3; renumbered 2026-10-22, R2 took `0011` and `0012`);
   - `0014_sharepoint_access.sql` (P4: `sharepoint_folder_access`).
2. **The Graph client lives in `pdms-core::graph`** (new module), not in
   `pdms-storage`. `pdms-storage` cannot depend on `pdms-core`, and the Entra
   sync needs the same client. The SharePoint store therefore lives in
   `pdms-core::sharepoint`.
3. **Configuration sections** (spec §9 refined, because Entra sync and
   SharePoint share one app registration):
   - `[graph]`: `tenant_id`, `client_id`, `certificate` (PEM certificate + key, normally supplied as `PDMS_GRAPH__CERTIFICATE_FILE=/run/secrets/…` through the loader's `_FILE` convention), `timeout_seconds`, `max_retries`, `authority_url`
     (default `https://login.microsoftonline.com`), `api_url` (default
     `https://graph.microsoft.com/v1.0`). Both URLs are overridable so tests
     can point at a mock server.
   - `[sharepoint]`: `site_url`, `library`, `delta_interval_minutes`,
     `upload_chunk_bytes`.
   - `[entra_sync]`: `enabled`, `run_at`, `attribute`, `include_guests`.
   - `[storage] backend = "sharepoint"` selects SharePoint for **document
     files**.
4. **`core.storage` (local or S3) stays in every configuration.** It holds
   records-request export packages, which are PADM artifacts rather than
   county documents, and it remains the document store for development and
   tests. Document file I/O goes through a new `documents::filestore` module
   that routes to SharePoint or to `core.storage`.
5. **Text extraction and exports read document files through
   `documents::filestore`**, never `core.storage.get(hash)` directly.

## Cross-plan contracts

Names and signatures that later plans rely on. If an implementer must change
one, they update this table in the same commit.

### Produced by P1

| Name | Definition |
|---|---|
| `pdms_authz::Visibility` | `#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(rename_all = "snake_case")] enum Visibility { #[default] Department, CountyWide, Restricted }` |
| `Visibility::ALL`, `as_str()`, `label()`, `FromStr` | `as_str` / `FromStr`: `"department"`, `"county_wide"`, `"restricted"`. `label`: `"Department"`, `"County-wide"`, `"Restricted"`. `FromStr::Err = UnknownVisibility(String)` |
| `Visibility::is_tighter_than(self, other) -> bool` | Restrictiveness order: CountyWide < Department < Restricted |
| `pdms_authz::AllowReason::CountyWide` | New `decide` outcome: view on a County-wide document by an active principal |
| `pdms_authz::DocumentAttrs` | `{ id, department_id, owner_id, visibility: Visibility }`; `inherit_acl` and `is_confidential` are removed |
| DB | `CREATE TYPE document_visibility AS ENUM ('department','county_wide','restricted')`; `documents.visibility document_visibility NOT NULL DEFAULT 'department'` |
| `pdms_core::documents::NewDocument.visibility` | `Visibility` (set at upload) |
| `pdms_core::documents::Document.visibility` | `Visibility` |
| `pdms_core::documents::set_visibility` | `pub async fn set_visibility(core: &Core, ctx: &Ctx, id: Uuid, to: Visibility) -> CoreResult<()>`. Allowed for the document's owner or a `manage_permissions` holder. Audit action `document.visibility_changed` with `change("visibility", from.as_str(), to.as_str())`. P3 adds the SharePoint hook inside this function. |
| Web | `POST /documents/{id}/visibility` (form field `visibility`); create form field `visibility` |

### Produced by P2

| Name | Definition |
|---|---|
| `pdms_core::graph::GraphClient` | Cheap to clone (`Arc` inside). `async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, GraphError>`, `post<B: Serialize + ?Sized, T: DeserializeOwned>(&self, path, body: &B)`, `patch<B, T>(&self, path, body)`, `delete(&self, path) -> Result<(), GraphError>`, `get_delta<T: DeserializeOwned>(&self, start: &str) -> Result<DeltaPage<T>, GraphError>` (follows `@odata.nextLink` to the end; returns all items plus the `@odata.deltaLink`), `put_bytes(&self, absolute_url: &str, bytes: Bytes, range: (u64, u64, u64)) -> Result<Option<serde_json::Value>, GraphError>` (upload-session chunk, no auth header), `download(&self, path: &str) -> Result<pdms_storage::ByteStream, GraphError>` (follows the 302 server-side). `path` is relative to `api_url` (e.g. `"/users/delta?$select=..."`) or an absolute `https://` URL (for nextLink and deltaLink). |
| `pdms_core::graph::DeltaPage<T>` | `{ items: Vec<T>, delta_link: String }` |
| `pdms_core::graph::GraphError` | `Throttled { retry_after: Duration }` (after retries are exhausted), `NotFound`, `Conflict`, `ResyncRequired` (HTTP 410), `Auth(String)`, `Http { status: u16, code: String, message: String }`, `Transport(String)`. Implements `Display` and `From<GraphError> for CoreError`. |
| `pdms_core::Core.graph` | `Option<pdms_core::graph::GraphClient>`: `Some` when `[graph]` has `tenant_id`, `client_id`, `certificate` |
| `pdms_core::directory::entra_sync` | `pub const SYNC_JOB: &str = "directory.entra_sync"`; `pub async fn run(core: &Core, ctx: &Ctx) -> CoreResult<SyncReport>`; `pub async fn sync_one(core: &Core, oid: &str) -> CoreResult<Option<Uuid>>`; `pub struct SyncReport { pub created: u32, pub updated: u32, pub departed: u32, pub unmapped: u32, pub changed_departments: BTreeSet<Uuid> }` |
| `pdms_core::directory::entra_values` | `list(db) -> CoreResult<Vec<EntraValue>>`, `upsert(db, ctx, value: &str, department_id: Uuid) -> CoreResult<()>`, `remove(db, ctx, value: &str) -> CoreResult<()>`, `unmapped(db) -> CoreResult<Vec<UnmappedValue>>`, `normalize(value: &str) -> String` |
| DB | `department_entra_values`, `sync_cursors (name text PK, delta_link text, last_run timestamptz, last_error text, last_summary jsonb, delta_select text)`, `users.entra_department`, `users.entra_synced_at`, `users.source user_source ('local','entra')` |
| Worker | Handler for `SYNC_JOB`; daily schedule at `[entra_sync] run_at`. **P4 adds** "enqueue `sharepoint.reconcile_access` after a successful sync" inside this handler. |
| CLI | `pdms sync-directory`; `pdms gen-graph-cert --out <file> [--days N] [--name CN]` (`--days` 30-1095; writes the PEM bundle, certificate + unencrypted key, mode 600, to `<file>` and the public certificate to `<file>.crt`; never overwrites; prints the `.crt` path and the thumbprint) |

**Changes recorded when P2 was written** (the P2 plan is authoritative for exact signatures):

- **Config key:** `[graph] certificate`, not `certificate_file`. The config loader maps any `PDMS_*_FILE` environment variable onto the key without `_FILE`.
- **Signing:** client assertions are signed **PS256** with an `x5t#S256` header (Microsoft's current guidance).
- **Error mapping:** `GraphError::Conflict` maps to `CoreError::Conflict`; every other Graph error maps to `CoreError::Internal`, so a Graph "not found" is never shown to people as a 404.
- **Grants:** migration 0010 also grants `DELETE` on `department_entra_values` (spec §6 listed SELECT, INSERT, UPDATE), because removing a mapping needs it.
- **Departures:** audited as the existing `user.status_changed`, which `set_user_status` writes. There is no `directory.user_departed` (spec §11 adjusted).
- **Guests:** guests are skipped by `userType = Guest` (any letter case); a null `userType` counts as a member (spec §7.5 said `ne 'Member'`).
- **Full rounds sweep:** a round that lists every account (first run, full resync, expired delta token, changed attribute or `include_guests`) departs this issuer's active accounts it did not list that were created before the round started. The sweep is skipped, and reported with the names of up to 20 accounts, when the round has no member accounts or would depart more than max(5, 20% of the issuer's active linked accounts); the last administrator, whom `set_user_status` keeps, does not count.
- **Local passwords:** a linked account with local credentials is departed like any other (by a delta or by the sweep), and the run adds a line to `SyncReport.errors` saying it had a local password. The last administrator is still kept and reported.
- **Cursor and attribute:** `sync_cursors.delta_select` records what a delta link was started with: the `$select`, plus `;guests` when `include_guests` is on. When `[entra_sync] attribute` or `include_guests` changes, the next run reads every account again.
- **Mapping changes:** `entra_values::upsert` and `remove` re-apply the mapping at once, in the same transaction, to active synced people whose stored Department value matches (same `idp` rule and `user.idp_memberships_synced` audit as the sync). **P4:** a mapping change should also queue a folder-access reconcile.
- **Turned off:** `entra_sync::run`, `run_full` and `pdms sync-directory` are refused unless `[entra_sync] enabled`; a queued job that finds the sync off finishes without retrying.
- **Additions:**
  - `GraphConfig.timeout_seconds` and `max_retries`;
  - `SyncReport.errors: Vec<String>`; `SyncReport` is `#[serde(default)]`, so a stored summary still reads after a later plan adds a field;
  - `sync_cursors.last_summary jsonb`;
  - `entra_sync::{run_full, status, request_sync, schedule_daily, run_job, TICK_JOB, LOCK_KEY}`;
  - `oidc::resolve_identity`, `normalize_issuer`.
- **Types:** `model::UserSource` was dropped; the sync writes `users.source` directly in SQL.
- **Mapping:** a manual department membership in the mapped department satisfies the mapping.
- **Config check:** `entra_sync.enabled` requires `[graph]`, `auth.oidc.issuer_url` and `auth.oidc.subject_claim = "oid"`.
- **Sign-in:** when the sync is on, a first sign-in asks Entra about the person and is denied with `email_in_use` if an unlinked account has that email; a Graph error denies sign-in. The token never sets names or groups while the sync is on.
- **Downloads:** download streams have no total deadline, so P3's large files are safe from it, but the download's pre-authenticated GET does not yet retry 429/503/504.
- **Decision numbers:** P2 recorded D-096. P4's planned decisions D-091 to D-097 must be renumbered from the next free number (D-097 on 2026-10-21) when P4 is executed.
- **Left to P4:** the Docker Compose secret and `install.sh` wiring for the certificate.
- **Open behaviour:** an account that is departed and later re-enabled in Entra is **not** reactivated automatically; an admin reactivates it. A simultaneous first sign-in and nightly sync are resolved with `ON CONFLICT DO NOTHING`.

### Produced by P3

| Name | Definition |
|---|---|
| `pdms_core::sharepoint::SharePoint` | `Clone`; built in `Core::new` when `[storage] backend = "sharepoint"` (needs `[graph]`). Holds the `GraphClient`; `resolved().await -> CoreResult<&Resolved>` looks up `site_id`, `drive_id`, `list_id` and the area folder item IDs lazily on first use, creating missing columns and area folders, and retries after a failure. `Core.sharepoint: Option<SharePoint>` |
| `pdms_core::sharepoint::layout` | Pure functions: `area_for(Visibility) -> Area` (`Departments`, `CountyWide`, `Restricted`); `document_folder_name(external_id, title) -> String`; `file_name(external_id, version_no, label, ext) -> String`; `sanitize_segment(&str, max_chars) -> String`; `columns(&PlacementDoc, &PlacementFile) -> serde_json::Map` |
| `pdms_core::documents::filestore` | `put_version_file(core, &PutTarget, &StoredFile, src: &Path) -> CoreResult<FileLocation>`; `open(core, document_id, version_no, hash, size) -> CoreResult<ByteStream>`; `open_version_file(core, version_id) -> CoreResult<ByteStream>`; `record_location`, `backend_name`, `copy_for_restore`, `known_media_type` |
| DB | `sharepoint_files` (spec §6, keyed by `document_version_id`), plus `list_item_id`, `name`, `fields_hash`; `sharepoint_department_folders` and `sharepoint_document_folders` (folders tracked by Graph item ID). Migration `0013_sharepoint_files` |
| Jobs | `sharepoint::MIRROR_JOB = "sharepoint.mirror_document"`; `pub async fn enqueue_mirror<'e>(exec: impl PgExecutor<'e>, document_id: Uuid) -> CoreResult<()>`; `mirror_later(core, conn, document_id)` (crate-private, called in every changing transaction); `MIRROR_SWEEP_JOB = "sharepoint.mirror_sweep"` (nightly); `register_jobs(worker, core)` |
| Hook | `pub async fn apply_placement_now(core: &Core, document_id: Uuid, to: Spot, reason: &str) -> CoreResult<()>`, called by `set_visibility`, by `update` (department change) and by `delete` whenever `layout::narrows(from, to)` |
| Errors | `CoreError::Unavailable(String)`: web and API 503, except on the document forms and `views::done`, which keep the page and show the message (D-102) |
| Change flag | `sharepoint::files::flag_changed(db, document_id, item_id, source, detail)`: marks the file `changed_outside` and writes `sharepoint.changed_outside` in one transaction. Private to `files` for now; P4 re-exports it for delta findings |
| Audit | `sharepoint.changed_outside`, `sharepoint.folder_moved` (category system; context `from`, `to`, `reason`; D-102) |
| Department folders | `pub async fn department_folder_id(core: &Core, area: Area, department_id: Uuid) -> CoreResult<String>` (create-if-missing) |

**Changes recorded when P3 was written** (the P3 plan, including its closing "Deviations" section, is authoritative):

- **Signatures:**
  - `apply_placement_now(core, document_id, to: Spot, reason: &str)`: it runs *before* the change is saved, so it is told the target; the reason goes into the `sharepoint.folder_moved` event.
  - `put_version_file(core, &PutTarget, &StoredFile, src: &Path)`.
  - New: `filestore::open(core, document_id, version_no, hash, size)`.
- **Startup:** the site, library, and area folders are resolved lazily on first use. The server still starts when SharePoint is down, as D-021 does for OIDC.
- **Tables:**
  - extra: `sharepoint_department_folders` and `sharepoint_document_folders`, tracking folders by Graph item ID;
  - `sharepoint_files` gains `list_item_id`, `name`, and `fields_hash`.
- **Errors:** new `CoreError::Unavailable`, which is HTTP 503 on the web and the API.
- **Behaviour:**
  - Soft-deleted documents move to `Restricted/<department>` until they are restored.
  - Department transfers move the folder *before* committing, like other narrowing changes.
  - "Open in PADM" is a text column, because Graph cannot write hyperlink columns.
  - Empty files cannot be stored.
  - File names use the PADM version number, so they skip numbers for metadata-only versions.
  - Files are uploaded under a temporary name and renamed by the mirror job (D-103).
  - Folder moves are audited as `sharepoint.folder_moved` (D-102).
- **Setup:** the "Current documents" view must be created by hand (Graph v1.0 cannot create views). P4's setup guide covers it.
- **Tests:** they extend P2's `tests/mock_graph` with an `sp.rs` child module.

### Produced by P4

| Name | Definition |
|---|---|
| Jobs | `sharepoint::RECONCILE_JOB = "sharepoint.reconcile_access"`; `sharepoint::DELTA_JOB = "sharepoint.delta"` |
| DB | `sharepoint_folder_access` (spec §6) |
| Admin | `/admin/sharepoint` status panel and flagged-files resolution; `/admin/entra` panel from P2 gains a SharePoint access section |
| Docs | `docs/SHAREPOINT_SETUP.md` (county setup runbook, from spec Appendix A, with exact PnP/Graph commands); ADMIN_GUIDE sections |

**Changes recorded when P4 was written** (the P4 plan's closing "Deviations" section is authoritative; its Task 9 Step 7 only needs to confirm this list):

- **Tests:** they use P2's `tests/mock_graph` plus P3's `sp.rs` (`MockGraph`, `SpItem`, `SpState`), not a separate P4 mock.
- **Jobs:** `RECONCILE_JOB` and `DELTA_JOB` live in `sharepoint::access` and `sharepoint::changes` and are re-exported from `sharepoint`. They are registered by `sharepoint::register_ops_jobs(worker, core)`, chained after P3's `register_jobs`.
- **Run records:** `sync_cursors` gains a `sharepoint.reconcile` record.
- **Migration 0014** (P4's; it was 0012) also adds `sharepoint_foreign_grants`, `sharepoint_unexpected_items`, and `sharepoint_files.changed_by` / `changed_at`.
- **Authorization:** new `Capability::ManageSharePoint`, and new `dispatch::Visibility::Anyone` for notices that name no document.
- **Integrity:**
  - `verify-integrity --rehash-sharepoint` (`--rehash` stays the local-file option);
  - `IntegrityReport.sharepoint_files_checked`;
  - `integrity::verify_with(core, VerifyOptions)`, with `verify(core, rehash)` kept as a wrapper.
- **Storage selection in Docker:** `PDMS_STORAGE__BACKEND=sharepoint` in `config/pdms.env` (Compose `env_file`, so Docker Compose ≥ 2.24 is needed). The installer creates the Graph certificate with `openssl`, in the same format as `pdms gen-graph-cert`; the file is mode 644 inside the 700 `secrets/` directory, like the other Compose secrets.
- **Admin page:** `/admin/sharepoint` lives in `doc_admin_routes`.
- **Admin page pattern (added by R2, 2026-10-22):** build `admin/sharepoint.html` to R2's admin pattern ([R2 spec](../specs/2026-10-22-ui-redesign-r2-design.md) §5.2), not the markup in P4 Task 7. Concretely: a `{% block heading %}` with `<header class="page-head">` (h1, `.page-lede`, `.page-actions`); each section a `.panel` with a `.panel-head`; notices as `flash-info` with `role="status"`; problems as `flash-error` with `role="alert"`; empty states as `<p class="empty">`. The page has no tab in `base.html` any more: add `link("sharepoint", "SharePoint")` to the *Integrations* group in `Layout::admin_groups()` (`src/views.rs`), behind its capability, and set `.admin_tab("sharepoint")` in the handler. `admin/entra.html` already uses the pattern; add P4's section as another `.panel`.
- **Migrations (R2, 2026-10-22):** R2 took `0011` (`0011_small_changes.sql`) and `0012` (`0012_color_scheme.sql`), so P3's is `0013` and P4's `0014`. R2 also took decisions D-097 to D-101.
- **Audit:** extra action `sharepoint.unexpected_dismissed`.
- **Edits to earlier plans' code:**
  - P2's `entra_sync::run_job` and `sync_one` queue a folder-access run;
  - `admin/entra.html` gains a SharePoint access section;
  - P3's `maintenance::worker()` last line changes.
- **Change feed:** an area folder appearing in it counts only when its name changed.
- **Copied permissions:** site permissions that SharePoint copies when inheritance breaks are not reported as "other access".
- **Test-server rollout** (P4 Task 9) erases the test server's data and **must be confirmed with the owner first**.

## Shared development setup (do once)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
# Dev database (README "Develop"); it has no restart policy, so start it after a reboot:
docker start pdms-dev-db
# Compile-time query checks and migrations use the owner role:
export DATABASE_URL=postgres://pdms_owner:devowner@127.0.0.1:55432/pdms
# One-time: the SQLx CLI (applies migrations, refreshes .sqlx/):
cargo install sqlx-cli --no-default-features --features postgres,rustls --locked
# After adding a migration:
sqlx migrate run --source migrations
# After changing any sqlx::query! or migration:
cargo sqlx prepare --workspace -- --all-targets
# Tests reuse the dev server instead of starting a Postgres container per test binary:
export PDMS_TEST_DATABASE_URL=postgres://postgres:devsuper@127.0.0.1:55432/postgres
export PDMS_TEST_OWNER_PASSWORD=devowner PDMS_TEST_APP_PASSWORD=devapp
```

With `DATABASE_URL` set, the `sqlx::query!` macros check against the live
dev database, so it must have the new migration applied. Without it, set
`SQLX_OFFLINE=true` to build from `.sqlx/`. CI builds offline and fails if
`.sqlx/` is stale.
