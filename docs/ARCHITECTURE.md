# Architecture

Self-hosted Policy and Document Management System (PDMS): the system of record
for policies, contracts, partnership agreements, and other dated documents, plus
personnel appointments. Organization-neutral; the display name is set at install
time (default: "Policy and Document Management System").

Status: **Phases 0–5 complete.** This document describes the system as built.
SharePoint file storage, Entra user sync and County-wide visibility (D-081 to
D-089) are designed in [the spec](superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md).
Visibility (P1), the Entra sync (P2) and SharePoint storage (P3) are built;
SharePoint folder permissions and change detection (P4) are tracked in
[ROADMAP §0](ROADMAP.md#0-where-things-stand). Decisions referenced as `D-nnn` are
in [DECISIONS.md](DECISIONS.md).

## 1. Principles

1. **Deny by default.** One authorization module decides every access; every
   query path (pages, search, calendar, ICS, API, exports, reports) goes through it.
2. **The database enforces what it can.** Constraints, explicit per-table grants,
   and triggers back up application logic. The app role owns nothing.
3. **Every state change and sensitive read is audited in the same transaction**
   as the change, in a hash chain that the app cannot rewrite.
4. **Nothing is overwritten.** Files are content-addressed and immutable;
   metadata changes produce version snapshots.
5. **Rust everywhere except OCR,** which runs as an isolated sidecar.

## 2. Deployment topology

```
 Browser / API client / Calendar app
             │ HTTPS
   ┌─────────▼─────────┐
   │ Caddy or nginx     │  TLS termination; sets X-Forwarded-For
   └─────────┬─────────┘
             │ HTTP
   ┌─────────▼─────────┐        ┌──────────────────────┐
   │ pdms serve         │        │ pdms worker           │  jobs, scheduler,
   │ (Axum, HTMX pages, │        │ (same image, separate │  email, webhooks,
   │  REST API)         │        │  process)             │  audit forwarding
   └───┬──────────┬─────┘        └───┬──────────┬───────┘
       │          │                  │          │
   ┌───▼───┐  ┌───▼──────────────┐   │   ┌──────▼──────┐   SMTP relay /
   │Postgres│  │File store: local │◄──┘   │ OCR sidecar │   Exchange Online
   │ 15+    │  │FS or MinIO (S3)  │       │ (optional)  │   MS Graph (optional)
   └────────┘  └──────────────────┘       └─────────────┘
```

One Docker image, one binary, subcommands:

| Command | Purpose |
|---|---|
| `pdms serve` | HTTP server |
| `pdms worker` | Background jobs and scheduler (can also run in-process via `--with-worker` for small installs) |
| `pdms migrate` | Apply migrations as `pdms_owner` |
| `pdms create-admin` | Create or reset a break-glass local admin |
| `pdms verify-audit` | Validate the audit hash chain; non-zero exit on any break |
| `pdms verify-integrity` | `verify-audit` plus re-hash every stored file against the DB (Phase 4) |
| `pdms seed-demo` | Load the demo dataset |
| `pdms gen-secret` / `check-config` / `healthcheck` | Generate a master secret; validate config (secrets redacted); container health probe |

`install.sh` asks what the system should be called (default "Policy and Document
Management System"), the time zone, and the public URL; writes
`config/pdms.toml`; generates secrets into `secrets/` (DB passwords, connection
URLs, `PDMS_SECRET_KEY`); builds and starts the stack; bootstraps the DB roles
(`deploy/postgres/initdb/` → `00_roles.sql`); runs migrations; and creates the
first administrator with TOTP. It is safe to re-run.

## 3. Cargo workspace

```
crates/
  pdms-authz/    Pure policy engine + SQL predicate builder. No I/O. Unit + property tests.
  pdms-audit/    Event model, canonical encoding, hashing, chain verifier (pure) + Postgres writer.
  pdms-core/     Config, DB, crypto, errors, and domain services as modules:
                 auth (local, OIDC, sessions, TOTP), directory, acl, audit_log, jobs,
                 maintenance, demo, documents (access, fields, files, retention, search,
                 status, tags, types, service), schedule (date rules), notifications
                 (rules, planner, delivery, inbox, mailer), reviews (tasks), calendar
                 (events, dashboards, ICS). Later: appointments, workflow, reports,
                 integrations, ocr.
  pdms-web/      Axum router, middleware, Askama templates (REST API + utoipa in Phase 4).
  pdms/          Binary: CLI subcommands above.
  pdms-testkit/  Test-only: disposable migrated Postgres per test (testcontainers).
  pdms-storage/  BlobStore trait (local FS, S3-compatible), upload spooling, content
                 sniffing, ClamAV client.
migrations/      SQLx migrations (owner role)
deploy/          DB bootstrap, Caddyfile and nginx samples
docs/
Dockerfile, docker-compose.yml, install.sh, config/pdms.example.toml
```

`pdms-authz` and `pdms-audit` are separate crates so the two most
security-critical pieces have no web or DB dependencies in their core logic,
compile fast, and are tested in isolation. SQLx offline metadata (`.sqlx/`) is
committed so CI builds without a live database.

## 4. Request pipeline

Tower layers, outermost first:

1. Request ID (generated, propagated as `X-Request-Id`), tracing span (JSON logs)
2. Body size limit
3. Prometheus request metrics (by matched route, not raw path)
4. Security headers: strict CSP (`default-src 'self'`; no inline script; HTMX
   vendored, no CDN), HSTS when secure, `nosniff`, `frame-ancestors 'none'`,
   `Cache-Control: no-store` on pages
5. Client IP: `X-Forwarded-For` honored only from trusted proxy CIDRs, taking the
   right-most untrusted hop
6. CSRF: unsafe methods must have a matching `Origin` (or no cross-site
   `Sec-Fetch-Site`), and authenticated ones must carry the session-bound token
   in `X-CSRF-Token` (HTMX) or the `_csrf` form field. The token is
   HMAC-SHA256(derived key, session token), so nothing extra is stored (D-014).
   The pre-session login form uses its own token in an encrypted cookie.
7. Flash messages (encrypted one-shot cookie for Post/Redirect/Get)
8. Handler: the `CurrentUser` extractor authenticates and loads the `Principal`,
   then builds a `Ctx` (principal + audit actor: user, IP, user agent, auth
   method, request ID) that every domain-service call takes. Login endpoints
   are rate-limited per client IP.

`Principal` is loaded fresh from the DB on every request: user status, department
memberships, groups, and role assignments. Nothing about grants is cached across
requests, so revocation applies to the next request (D-008).

## 5. Authentication

- **OIDC (Entra ID):** authorization code + PKCE, `openidconnect` crate. User
  identity key is (issuer, `oid` claim). Group claims are reconciled on each
  login via `idp_group_mappings` into department memberships, groups, and role
  assignments (`source = 'idp'` rows only; manual rows untouched). Graph-based
  sync for group overage is Phase 4.
- **Local break-glass:** Argon2id (`argon2` crate), TOTP required (`totp-rs`,
  ±1 step, each step accepted once), lockout after N failures, dummy hash
  verification for unknown accounts (no timing oracle), generic error messages.
  TOTP secrets are AES-256-GCM encrypted with a key derived from
  `PDMS_SECRET_KEY`, bound to the user ID. Can be disabled in config.
- **Sessions:** server-side `sessions` table; cookie holds a random 256-bit
  token (only its SHA-256 is stored); `Secure; HttpOnly; SameSite=Lax`; idle and
  absolute timeouts; logout and user deactivation revoke sessions.
- **API tokens (Phase 4):** hashed, scoped, act as their owning user with the
  intersection of the user's permissions and the token scopes.
- **ICS tokens (Phase 2):** revocable per-feed secrets; feeds run through the
  same authz filter as the calendar page.

All login, logout, failed login, and lockout events are audited.

## 6. Authorization

### Model

- **Subjects:** user, group, department (all members), role (holders,
  optionally department-scoped).
- **Permissions:** `view`, `create`, `edit`, `delete`, `approve`,
  `manage_permissions`, `export`. Any permission other than `create` implies
  `view`.
- **Scopes:** `global`, `department`, `document`. Grants are allow-only and can
  expire.
- **System capabilities** (admin console, audit access, export packages) come
  from system role keys: `admin`, `auditor`, `records_officer`; plus
  `legal_counsel` and `department_head` used for routing.

### Decision rule

`allowed(principal, perm, document)` is true iff the principal is active and any of:

1. the principal is the document's primary owner and `perm` is in the
   configured owner set (default `view`, `edit`);
2. some unexpired grant has a matching permission, its subject matches the
   principal, and its scope matches the document:
   - `document` scope: same document. Always applies.
   - `department` scope: same department, **and** the document is not Restricted.
   - `global` scope: the document is not Restricted.
   - A role subject matches if the principal holds that role org-wide or for
     the document's department; or
3. `perm` is `view` and the document is County-wide.

So:

- **Read-only default:** creating a department also creates a grant
  (department members → `view` on that department). A new user can view their
  department's documents, plus County-wide documents, and nothing else.
- **Visibility (D-085):** every document is *Department* (default), *County-wide*
  (additionally, every active account may **view** it; nothing else), or
  *Restricted* (department and global grants never apply, so it needs explicit
  grants; admins included). The owner chooses at upload and can change it later;
  anyone with `manage_permissions` can too. Changes are audited
  (`document.visibility_changed`).
- **Separation of duties:** workflow-level checks such as "cannot approve own
  submission" are separate functions in `pdms-authz`, configurable in settings.
- **Creating documents:** `create` on department scope (or global) for the
  target department.

### Enforcement

- `pdms-authz` exposes `decide(&Policy, &Principal, Permission, &DocumentAttrs, &[Grant], now)`
  for single objects, `decide_create` for departments, `has_capability` for
  system capabilities, and `visible_documents_predicate(...)`, which returns a
  `SqlFragment` (text interleaved with bind parameters) that `pdms-core` pushes
  into a `QueryBuilder` for list, search, calendar, ICS, export, and report
  queries.
- **Document operations** all start in `documents::access::authorize` (single
  documents) or `documents::search::query` (lists, search, exports). Both call
  `pdms-authz`, and nothing else in the codebase reads document rows for
  display. Denials are audited.
- **Equivalence test (passing):** a property test generates random principals,
  grants, and documents, loads them into Postgres, and asserts that the SQL
  predicate and `decide()` agree for every principal × permission × document
  (300 cases per run, about 6,700 checks, both outcomes well represented).
- Denied requests return 403 and are audited as `access.denied` with
  `outcome = 'denied'`. Lists filter silently.

## 7. Audit log

Schema: `migrations/0001_foundation.sql` (`audit_log`, `audit_chain_head`).

- **Contents:** actor (user ID, display-name snapshot, auth method, IP, user
  agent, API token), action (`document.metadata_updated`), category
  (`content | metadata | permission | admin | auth | access | workflow | notification | system`),
  outcome, object, denormalized `document_id` and `department_id` for
  timelines and filters, field-level `changes`, and `context` (for example an
  records request reference).
- **Chain:** `entry_hash = SHA-256(canonical(row))`, where the canonical form
  (hash_version 1) is RFC 8785-style JSON over every column except `entry_hash`.
  It includes `seq` and `prev_hash`, so each entry commits to its predecessor.
  Timestamps are RFC 3339 UTC to microseconds and bytes are hex. A golden-vector
  test pins the encoding.
  Audit values are a restricted enum with no floats (decimals are stored as
  strings), so JSONB round-trips are byte-stable (D-005).
- **Write protocol:** `audit_lock_head()` (row lock), compute, then `INSERT`. A
  `SECURITY DEFINER` trigger re-checks seq and prev_hash and advances the head.
  The audit write is the last statement of the business transaction, because
  the head lock serializes audited commits. Org-scale volume makes this fine,
  and it guarantees a gapless, linear chain (D-004).
- **Protection layers:**
  1. `pdms_app` has only `SELECT, INSERT` on `audit_log` and `SELECT` on the
     head, and owns neither, so it cannot disable triggers.
  2. Triggers reject `UPDATE`, `DELETE`, and `TRUNCATE` even for the owner role.
  3. The hash chain detects tampering by anyone who bypasses 1 and 2
     (superuser).
  4. Optional forwarding to syslog or a JSONL file, plus periodic head-hash
     checkpoints, so a superuser rewrite of the whole chain is detectable
     externally (Phase 4).
- **Verification:** `pdms verify-audit` (exit code 2 on failure), the
  "Verify integrity" button on the audit page, and a nightly worker job each
  read the table in `seq` order from one snapshot, recompute every hash, and
  report each break: modified row, missing or extra row, bad link, undecodable
  row, or a chain head that disagrees with the last row. After a break the
  verifier re-synchronizes, so one tampered row is reported once. Each
  verification is itself audited.
- **Reading the log is audited too:** searches (`audit.searched`) and exports
  (`audit.exported`). Viewing and verifying need the `auditor` or `admin` role;
  exporting needs `auditor`.
- **Version history is separate** (`document_versions`, Phase 1): it keeps
  restorable content and metadata snapshots. The audit log records actions.
- **The dashboard's activity feed** (`pdms-core::activity`, R2) reads the log
  for everyone, so it is narrow by construction:
  - a fixed list of document actions with outcome `success`;
  - joined to `documents` through the same visibility predicate as search, so
    only documents the viewer may see now;
  - only the time, the action, the actor's display name and the document are
    read, never `changes` or `context`, and the name is dropped for kinds whose
    actor the document page does not show (details and status changes,
    review outcomes, declined signatures);
  - the last 90 days, ten items, not audited (like the rest of the dashboard).

## 8. Jobs and scheduling

- `jobs` table, claimed with `FOR UPDATE SKIP LOCKED`, lease-based (a crashed
  worker's jobs become claimable when `lease_expires_at` passes), exponential
  backoff with jitter, and a `dead` state after `max_attempts`. Built in-house
  rather than with `apalis` for full control over idempotency keys and leases
  (D-006).
- **Planner** (`notifications::plan`, every 15 minutes, plus `pdms run-scheduler`).
  Each run:
  1. moves documents among Active, Renewal Due, Overdue, and Expired from
     their dates (D-044);
  2. creates or cancels review and renewal tasks and marks late tasks
     overdue (D-045);
  3. after the local send hour, materializes the latest due reminder for each
     document, event, and rule with a natural unique key, using
     `ON CONFLICT DO NOTHING` (D-041).

  Each reminder's deliveries (recipient × channel), email jobs, and audit
  entry are written in one transaction. Recipients without view access are
  recorded as skipped (D-043).
- **Delivery:** in-app deliveries are visible immediately. Each email delivery
  is a job keyed by delivery ID. The delivery moves `pending → sending → sent`;
  on failure it moves to `failed` and is retried with backoff. Every attempt
  is logged in `notification_delivery_attempts`. Each email has a
  deterministic `Message-ID`, so the only duplicate possible is a crash
  between the SMTP server's acceptance and the commit (D-007).
- **SharePoint mirror** (`sharepoint.mirror_document`, only with the
  SharePoint backend): every change to a document queues one run in the
  change's own transaction, unless one is already pending. The run gives
  files their final names, writes the library columns, marks the newest file
  `Current`, and moves or renames the document's folder. It is idempotent,
  and a nightly `sharepoint.mirror_sweep` queues it for every document.
- **Rules:** the document's own rules win over its type's, which win over the
  organization defaults, per event (D-042).
- **Calendar and dashboards** expand document dates (with projected review
  cycles) from the authorized document search. ICS feeds are rendered at
  fetch time with the feed owner's current permissions (D-047).
- **Time:** instants are stored as `timestamptz` (UTC). Review, renewal, and
  expiry dates are civil `date` values. Notifications fire at a configured
  local hour in the organization's IANA time zone (`chrono-tz`), so DST is
  handled when converting to UTC.

## 9. Documents and file storage

**Upload path:**
1. The browser posts a multipart form; the CSRF token is the first field (D-028).
2. The file is streamed to a spool file while being hashed and size-limited.
3. Its type is sniffed from the content against an allowlist (D-027).
4. ClamAV scans it, if configured: `clamd` INSTREAM, fail closed by default.
5. It is stored (`documents::filestore::put_version_file`) and recorded in
   `blobs`.
6. A `document_versions` row is appended.

**Where files live.** `documents::filestore` is the only code that decides.
With `[storage] backend = "local"` or `"s3"`, files go to the `BlobStore`
under their SHA-256. `BlobStore` has `put_file`, `get`, `size`, and `verify`,
but no delete. The backends are the local filesystem (temp file, re-hash,
fsync, rename, read-only) and S3-compatible storage (conditional `PUT`,
multipart above 16 MB). Export packages always use the `BlobStore`.

With `backend = "sharepoint"` (D-081 to D-084), `pdms-core::sharepoint` stores
document files in one SharePoint library through Microsoft Graph (P2's
`GraphClient`):
- **Setup on first use:** the site, library, list, missing columns and the
  three area folders are found or created lazily (`SharePoint::resolved`), so
  the server still starts when SharePoint is down.
- **Folders** are tracked by Graph item ID (`sharepoint_department_folders`,
  `sharepoint_document_folders`), so renames never depend on paths.
- **Upload:** before the version's transaction, the file is sent in chunks to
  the document's folder under a temporary name (D-103), and SharePoint's
  QuickXorHash and size are checked. The transaction records it in
  `sharepoint_files`. A failure is `CoreError::Unavailable`: nothing is saved
  and the form is kept (D-102).
- **Read:** downloads, text extraction, records packages and restores stream
  the file from SharePoint and check it against the recorded SHA-256 and
  size. A mismatch ends the stream with an error, marks the file
  `changed_outside`, and is audited in the same transaction.
- **Placement:** the mirror job (§8) applies names, columns and moves.
  Changes that narrow who can browse a document (`layout::narrows`) move the
  folder before the change is saved and are refused if SharePoint is down.
  Every folder move is audited as `sharepoint.folder_moved`.

**Data model:**
- `documents` holds the current metadata. `custom_fields` is typed JSONB keyed
  by immutable field keys.
- `custom_field_definitions` and `custom_field_versions` give versioned field
  definitions (D-029).
- `document_versions` is immutable: a full metadata snapshot plus the current
  file, appended on every change (D-030).
- `blobs` is immutable.

**Lifecycle:** statuses follow an explicit transition table (D-031).
Deletion is soft, blocked by legal holds and retention policies, and leaves a
tombstone audit entry (D-032).

**Search:** a `tsvector` refreshed by `refresh_document_search()`, plus
filters on type, department, owner, status, tags (any/all), date ranges, and
one custom field. It can sort by any of these. Every query is built on the
authz predicate.

## 10. Configuration and branding

`figment`: defaults → `config/pdms.toml` → `PDMS_*` environment variables. Any
secret can be given as `PDMS_<NAME>_FILE` (Docker secrets). `org.name` drives
the UI title, email subject prefix, ICS `X-WR-CALNAME`, and export manifests.
Role display names (for example `legal_counsel` → "General Counsel's Office")
are edited in the admin UI, so nothing organization-specific is compiled in.

## 11. Non-Rust components

| Component | Why | Interface |
|---|---|---|
| OCRmyPDF/Tesseract (optional, `deploy/ocr`) | No mature Rust OCR. OCRmyPDF is itself Python, so the sidecar is a ~100-line standard-library Python HTTP wrapper in the official OCRmyPDF image | `POST /ocr` (file in, text out) behind the `OcrProvider` trait; off by default, and the app runs normally without it. Embedded PDF and Word text is read in Rust |
| HTMX (vendored JS) | Server-rendered interactivity without an SPA | Static asset |
| PostgreSQL, MinIO, ClamAV | Infrastructure | Network protocols |

## 12. Data model by phase

| Phase | Tables |
|---|---|
| 0 (`0001_foundation`) | departments, users, local_credentials, department_members, groups, group_members, roles, role_assignments, idp_group_mappings, sessions, app_settings, acl_entries, audit_chain_head, audit_log, jobs |
| 1 (`0002_documents`, done) | retention_policies, document_types, custom_field_definitions, custom_field_versions, documents (incl. `external_id`, `visibility` (0008; replaced `inherit_acl`/`is_confidential`), `legal_hold`, `custom_fields jsonb`, `search tsvector`, `row_version`), blobs, document_versions, tags, document_tags; FK `acl_entries.scope_document_id → documents` |
| 2 (`0003_notifications`, done) | review_tasks, notification_rules, notification_rule_recipients, notifications, notification_deliveries (email and in-app), notification_delivery_attempts, calendar_feeds. Review schedules are the document/type `review_interval_months` plus `next_review_date` |
| 3 (`0004_appointment_due_events`, `0005_workflow_appointments`, done) | workflows, workflow_steps (the approval steps), workflow_events, signatories, positions, position_requirements, appointments, appointment_requirement_status; `notifications` generalized (`kind` reminder/action, `appointment_id`, subject/body/link, `dedupe_key`); `notification_rules.position_id`; `due_event` gains `term_end`, `reappointment`; role `appointments_clerk`; seeded grants: legal counsel view, department head approve |
| 4 (`0006_integrations`, done) | api_tokens, api_request_log, webhooks, webhook_deliveries, webhook_delivery_attempts, outbound_cursors, export_packages; seeded grant: records officer `export` (global). Integration providers are enabled in configuration, not a table; audit checkpoints were not needed (see D-068) |
| 5 (`0007_comments_ocr_graph`, done) | comments, comment_revisions (immutable), document_texts (extracted text per version; part of full-text search), date_suggestions, graph_calendar_links |
| 6 (`0008_document_visibility`, `0009_ui_redesign`, `0010_entra_sync`, done) | 0008: `documents.visibility` (department / county_wide / restricted) replaces `inherit_acl` and `is_confidential`. 0009: branding_assets (the seal); `users.username`. 0010: department_entra_values (Entra Department value to PDMS department), sync_cursors (delta link, last run, last error, last summary, delta select), `users.entra_department`, `users.entra_synced_at`, `users.source` |
| 7 (`0012_color_scheme`, R2) | `users.color_scheme` (`dark` default, `light`, `system`; D-098), loaded with the session and written as `<html data-theme>`. |
| 7 (`0011_small_changes`, R2 with sub-project 3) | Renames the seeded `partnership_agreement` type to `interlocal_agreement` ("Interlocal Agreement (ILA)", prefix `ILA`), keeping a name or prefix an administrator changed (D-088); deletes queued Outlook sync jobs and drops `graph_calendar_links` (D-087). |
| 8 (`0013_sharepoint_files`, P3) | sharepoint_department_folders (one folder per department and area), sharepoint_document_folders (one per document), sharepoint_files (one per version that introduced a file: drive and item IDs, list item ID, name, QuickXorHash, size, last known path, hash of the column values last written, state `ok` / `changed_outside` / `missing`) |

## 13. Phase 0 deliverables and tests (done)

- Workspace, CI (fmt, `clippy -D warnings`, tests, SQLx metadata check, Docker
  build), Dockerfile, `docker-compose.yml`, `install.sh`, and config loading.
- Migration 0001 and the role bootstrap.
- OIDC login (tested against a mock IdP container) and local break-glass login
  with TOTP.
- Admin pages: users, departments, groups, roles, grants, and IdP mappings, plus
  an audit log viewer with filters, CSV/JSON export, and integrity check.
- Background worker with periodic maintenance: session purge, job purge, and
  nightly audit verification.
- `pdms-authz`: unit tests for every rule above; proptest invariants
  (deny-by-default, monotonicity: adding a grant never removes access,
  revocation removes access, Restricted documents ignore broad grants,
  `create` never implies `view`); the SQL/engine equivalence test.
- `pdms-audit`: canonical encoding golden vectors, chain verify and tamper
  detection, and a concurrent-writer test (N tasks appending, chain stays
  linear).
- DB tests run as `pdms_app` and assert that UPDATE, DELETE, and TRUNCATE on
  the audit table fail; `verify-audit` detects a superuser-modified row.
- Health (`/healthz`), readiness (`/readyz`, checks DB), and `/metrics`.

## 14. Phase 1 deliverables and tests (done)

- **Schema:** migration `0002_documents`, with immutability triggers on versions, blobs, and field versions.
- **Storage:** `pdms-storage`, tested against the local filesystem, a real
  S3-compatible server (SeaweedFS), and real clamd (EICAR detection; opt-in
  test).
- **Document service:** create (with file), edit (field-level diffs,
  optimistic locking, department transfer), versions (upload, restore file or
  details), tags, status transitions, sharing, ownership transfer, access
  settings, retention and legal hold, soft delete and restore, timeline,
  search, and CSV export.
- **Web:** document list and search, create form (works without
  JavaScript), document page, edit form, history, downloads, and admin pages
  for types, fields, tags, retention, and deleted documents.
- **Authz equivalence:** the property test now runs against the real `documents` table.
- **Acceptance tests (automated):**
  - 1 (Phase 1 parts): audit timeline, both file versions, and an intact chain.
  - 2: read-only default, including search, direct URL, and download.
  - 3: revocation.
  - 7: custom field rename/add/deprecate does not break records or exports.
  - Test 6 (no expiration means no expiration alerts) belongs to Phase 2,
    where alerts exist.

## 15. Phase 2 deliverables and tests (done)

- **Schema:** migration `0003_notifications`, with seeded organization default rules.
- **Pure date rules** (`schedule`): month arithmetic, missed review cycles,
  reminder trigger selection, date-driven statuses, and the DST-aware send
  window. All unit-tested.
- **Planner, tasks, rules, delivery:**
  - planner, tasks, and rules (organization, type, and document scope, with
    authorization and auditing);
  - email via SMTP, file, or disabled transports, tested against a real SMTP
    server;
  - in-app inbox with acknowledgement.
- **Calendar:** month and agenda views, dashboards (upcoming 30/60/90/365 days,
  overdue with missed cycles, my tasks), and revocable ICS feeds.
- **Acceptance tests (automated):**
  - 4 (partnership agreement from 2013, amended 2014, five-year cycle): Overdue, two missed
    cycles, owner and department notified, versions and completed review kept.
  - 5 (worker killed mid-send, then restarted): every email sent exactly once.
  - 6 (no expiration date): stays Active, no expiration reminders, nothing
    on the calendar.
  - 2 (read-only default), now also covering the calendar, dashboard, and ICS.

## 16. Phase 3 deliverables and tests (done)

**Workflow (`pdms-core::workflow`).** A round is one `workflows` row with
ordered `workflow_steps`:

- Stages: reviewers (parallel) → legal counsel (a step assigned to the
  `legal_counsel` role, added when the type has `requires_legal_review` or
  the submitter asks) → approvers (parallel; every one must approve). A stage
  starts when the previous one is fully approved.
- Assignees are a user or a role (any holder org-wide or for the document's
  department). Decisions go through `pdms-authz`: `is_assignee`, the step's
  document permission (`approve` for approval steps, `view` otherwise), and
  `check_self_approval` against the submitter for approval and legal steps.
  Denials are audited (`workflow.decision_denied`).
- Submit: Draft/Active/Renewal Due/Overdue → In Review. All steps approved →
  Active. Reject / request changes / cancel → back to Draft (if it was a
  draft) or Active (the planner then re-applies date-driven statuses).
- While a round is in progress, manual status changes out of In Review are
  refused. Approving is refused if a new file version was added after
  submission.
- Every round, step decision, comment, and the full event list are kept.
  Each decision is one transaction: step update, stage advance, document
  status, workflow event, audit entries, and the notices.

**Action notices (`notifications::actions`, `notifications::dispatch`).**
"Review requested", "Approval requested", "Legal review requested",
"Approved/Rejected/Changes requested", and "Signature requested" are
`notifications` rows of kind `action`, delivered in-app and by email through
the same deliveries, jobs, recipient visibility checks, and send-time
re-checks as reminders. `dedupe_key` makes them idempotent.

**Signatures (`signatures`).** Signatories (user or outside person) with
status pending → requested → signed/declined, timestamps, who recorded it, and
the version holding the signed copy (uploaded as a `signed_copy` version).
Requests go through the `SignatureProvider` trait; `ManualSignatures` is the
built-in provider (`Core.signatures`).

**Appointments (`appointments`).** Positions (body, seat, appointing
authority, term, seats, manager, department, reappointment lead time),
per-position requirement checklists, and appointments:
- Appoint (seat count enforced under a row lock), reappoint (same person, new
  term), replace (successor), end, and correct.
- Reappoint and replace end the current appointment and link the new one
  (`predecessor_id`), so history is complete.
- The checklist is recorded per appointment with who and when; a new term
  starts it over.
- Visibility (`pdms-authz::can_view_position`): admins, appointments clerks,
  and auditors see all; otherwise the position's manager and members of its
  department. `can_track_position` (clerks/admins and the manager) records
  checklist items; only clerks/admins change anything else.

**Shared calendar and reminders.** The planner also plans `term_end` and
`reappointment` reminders for active appointments, using position-scoped
rules or the organization defaults (seeded: manager plus appointments
clerks). An overdue reappointment repeats weekly ("a replacement must be
selected"). Appointment events appear on the calendar, the agenda, the
dashboard (holdovers and overdue decisions under Overdue), and ICS feeds,
filtered by the viewer's position visibility.

**Tests.** Core: full partnership agreement round (review → legal → approval, notices and
emails), separation of duties (routing and deciding, role holders, policy
override), rejection/changes/cancel history, new-file conflict, signatures
with signed-copy upload, appointment history and checklists, visibility,
reminders, calendar/dashboard/ICS. Web: the same flows through the pages,
including the multipart signed-copy upload with deferred CSRF. authz: unit
tests for position visibility and step assignees.

## 17. Phase 4 deliverables and tests (done)

**Records request packages (`exports`).** Records officers (`BuildExportPackages`)
choose documents they may *export* (per-document `export` permission from the
central engine; migration 0006 seeds a global `export` grant for the role,
which never reaches Restricted documents). A package is a ZIP with
`README.txt`, `manifest.json` (request reference, options, documents, every
file with SHA-256), `documents/<ID>/metadata.json`, the chosen file versions,
optional `audit-history.json/.csv`, and `SHA256SUMS`. Every stored file is
re-hashed while packaging and must match. The ZIP is stored content-addressed
in the blob store; its and the manifest's SHA-256 are recorded. Audited:
`export_package.created`, `document.exported` per document (so it appears on
the document's timeline), `export_package.downloaded`.

**Reports (`reports`).** Upcoming, overdue, by department/owner/tag,
ownership, access review (grants, roles, orphaned and departed accounts;
`ViewAccessReports`: admin, auditor), audit summary, denials, records-request
log (`ViewAudit`). Document reports come from the authorized search. Web
and API; CSV exports are audited (`report.exported`) and neutralize
spreadsheet formulas.

**REST API (`/api/v1`, `pdms-web::routes::api`).** OpenAPI 3.1 generated with
`utoipa` (`/api/v1/openapi.json`, readable at `/api/docs`). Bearer tokens
(`pdms_…`, SHA-256 stored, shown once, expiring, revocable) act as their user,
limited to scopes (`documents:read|write`, `calendar:read`,
`appointments:read`, `reports:read`, `audit:read`). A middleware authenticates
(user and token checked on every request), rate limits per token (governor),
and logs every request to `api_request_log` (purged after
`request_log_days`); token use is audited at most hourly with
`api_token_id` on the actor, and every audited action through the API
carries the token ID. Errors are JSON. The cookie CSRF guard skips `/api/`,
which never reads cookies.

**Outbound events (`integrations`).** The audit log is the event stream: a
dispatcher (worker job every 30 s) reads entries after a locked cursor
(`outbound_cursors`) and, for published actions (documents, workflow,
signatures, tasks, appointments; not reads or denials), creates one
`webhook_deliveries` row per target plus a delivery job. Webhooks act as a
service user and only receive events about documents and positions that user
may view. Requests are JSON, signed (`X-PDMS-Signature: t=…,v1=HMAC-SHA256`),
with the delivery ID stable across retries; failures retry with the job
queue's exponential backoff, every attempt is logged, and admins can
redeliver. Secrets are AES-GCM encrypted with a key derived from the master
secret. `IntegrationProvider` is the trait for future connectors (enabled by
key in `[integrations] providers`; built-in `log`). The same cursor mechanism
forwards every audit entry (with its hashes) to a JSON-lines file or syslog
(`[audit_forward]`).

**Integrity and backup.** `pdms verify-integrity [--skip-files] [--json]`
checks every blob (present, size, SHA-256 re-hash), every export package,
version→blob references, and the audit chain; exit code 2 on any problem;
audited. `docs/BACKUP.md` and `deploy/backup/pdms-backup.sh` /
`pdms-restore.sh` implement database-then-files backups and a full restore
ending in `verify-integrity`.

**Tests.**
- Acceptance test 1 end to end (core): upload, edit and owner change by
  another user, amendment, Legal counsel and approver sign-off, ACL change by
  an administrator, records request export; chronological timeline with actors
  and before/after values, both file versions, package contents verified
  against `SHA256SUMS` and the recorded hashes, app role cannot alter audit
  rows, and `verify-integrity` detects a tampered audit row.
- Acceptance test 2 over the API: department-only listing, 403 on other
  documents and files, no writes without grants, scopes enforced.
- Webhooks against a local HTTP endpoint (signature, visibility filtering,
  idempotent dispatch, retries on HTTP 500, redelivery); provider delivery;
  audit forwarding to a file; tokens (expiry, revocation, inactive users,
  hourly use audit); rate limiting; reports authorization and CSV;
  `verify-integrity` finding altered and missing files.
- Acceptance test 8: the backup/restore drill was run against the Docker
  install (backup, `docker compose down -v`, delete `config/` and `secrets/`,
  restore) and ended with `Integrity verified: no problems found.`

## 18. Phase 5 deliverables and tests (done)

**Comments (`comments`).** One level of threading (comment, replies).
Reading needs `view`; writing needs `view` (or `edit` when
`[comments] viewers_can_comment = false`), and on County-wide documents only
the owner and administrators may write (D-091; refusals audited as
`comment.post_denied`). `@email` or `@local-part` (when
unique) mentions resolve to active users; mentioned people who can view the
document get an action notice (in-app and email), and replies notify the
parent's author. Every version of the text is stored in `comment_revisions`
(trigger-enforced immutable); edits by the author only (while they may still
comment); deletion by the author only (D-091) is a soft delete. Earlier
versions are visible to the author, permission managers, and auditors. All
actions are audited with the text (`comment.created/replied/edited/deleted`,
denials too) and published to webhooks, except the `*_denied` actions.

**Text and date suggestions (`ocr`).** Appending a file version queues
`documents.extract_text` in the same transaction. The job reads embedded PDF
text (`pdf-extract`, on a blocking thread), Word `word/document.xml`, or text
files; scanned PDFs (too little embedded text) and images go to the
`OcrProvider` (the sidecar) if configured. Results are stored per version
(`document_texts`, reused for identical files), the latest is added to the
document's full-text search, and `ocr::dates` (pure Rust, unit-tested)
classifies dates by nearby keywords and finds terms ("five (5) years") and
review cycles ("annually"). Suggestions that differ from the document's
current values are stored as `pending`; the owner is told. Only a person with
`edit` can accept (an ordinary, audited metadata version; other pending
suggestions for that field are set aside) or dismiss. Nothing is applied
automatically. OCR unavailability retries the job; unreadable files are
recorded as failed.

**Outlook calendar (removed).** Phase 5 also built an optional Outlook
calendar sync through Microsoft Graph. D-087 removed it, with its
configuration, job, admin page and installer step; migration `0011` drops its
`graph_calendar_links` table. ICS feed subscriptions are unchanged.

**Tests.** Core: comments (threads, mentions with visibility filtering and
ambiguity, edits with revisions, authors-only delete, County-wide documents limited
to owner and admin comments, history access, immutable
revisions at the database, edit-required mode); extraction of digital PDF,
copier-OCR'd PDF, Word, and image files; suggestions never applied until
accepted; accept/dismiss authorization and audit; OCR retry when unavailable;
extraction disabled. Opt-in: the real OCR sidecar reads a scanned
PDF and the dates in it become suggestions. Web: comments and suggestions
through the document page (escaping of file and comment text), history and
delete permissions, the API's comments and text endpoints, the text
extraction (OCR) admin page.

