# Decisions

Lightweight decision records. D-001 to D-013 were reviewed with the Phase 0
plan and accepted on 2026-10-13. D-014 to D-025 were made while building
Phase 0, D-026 to D-040 while building Phase 1, and D-041 onward while
building Phases 2–5. D-081 to D-089 were decided on 2026-10-14, when the
county set PDMS's direction (SharePoint storage, Entra sync); see ROADMAP
§0. D-090 and D-091 were decided on 2026-10-15 (visibility follow-ups).
D-092 to D-095 were decided on 2026-10-15 for the UI redesign R1 (built and
finished 2026-10-21; D-092 has a 2026-10-21 update on R2's timing).
D-096 was decided while building plan P2 (Entra sync, 2026-10-21).
D-097 to D-101 were decided on 2026-10-22 for the UI redesign R2.
D-102 was decided on 2026-10-22, before plan P3 (SharePoint storage) was built,
and D-103 while building it. D-104 to D-110 were decided on 2026-10-23 for plan
P4 (SharePoint access, change detection, and operations), with the owner's
answers to its pre-flight questions. D-111 to D-116 were decided on 2026-10-23
for S1 (settings in Administration). D-117 was decided on 2026-10-28 for GS
(the Entra sync limited to one group). D-118 was decided on 2026-10-30 for DV
(viewing documents in the browser).
New entries take the next number after the highest one on `main` and in
open pull requests.
Entries marked "design, not built" or "not done" are agreed but not yet
implemented.

### D-001 Single binary, separate server and worker processes — Accepted
One image and one binary with subcommands. The server and worker run as separate
compose services so a stuck job can't starve HTTP, and the worker can be
restarted alone (needed for the idempotency acceptance test). Small installs can
run `serve --with-worker`.

### D-002 Two database roles; app role owns nothing — Accepted
`pdms_owner` runs migrations and owns the schema. `pdms_app` gets explicit
per-table grants with no default privileges, so every new table is
inaccessible until a migration grants access. Users, departments, groups, and
roles get no `DELETE` (deactivate instead). Role names are fixed because SQLx
migrations can't be parameterized.

### D-003 Allow-only ACLs; overrides via broken inheritance, not deny entries — Accepted (amended by D-085)
Deny ACEs make evaluation order-dependent and hard to reason about. "Override"
means setting `inherit_acl = false` on the document and granting explicitly.
Confidential documents ignore department and global grants. Admins get global
`view` and `manage_permissions` through a seeded, editable grant, not hard-coded
superuser access.

### D-004 Audit hash computed in Rust, chain serialized by a head-row lock — Accepted
Alternatives were computing hashes in a PL/pgSQL trigger (the Rust verifier
would then have to match Postgres's text rendering of jsonb exactly) and
per-partition chains (more complex for little gain at this volume). The head
lock serializes audited commits; the audit insert is always the last statement
of a transaction so the lock is held for milliseconds. Revisit if write volume
exceeds roughly 200 audited transactions per second.

### D-005 Audit values exclude floating point — Accepted
Hashes are recomputed from data read back from jsonb. Floats can change textual
form on round-trip; integers, strings, booleans, and null can't. Money and
decimals are recorded as strings.

### D-006 In-house Postgres job queue instead of `apalis` — Accepted
The queue is about 300 lines. We need control over idempotency keys, leases,
and the dead-letter state, and it avoids depending on a fast-moving crate.

### D-007 Notification delivery: exactly-once except the SMTP-ack window — Accepted
No SMTP transaction can be atomic with a Postgres commit. Deliveries are
committed as `sending` before the SMTP call and `sent` after. If the worker dies
between the server's 250 response and the commit, the next run sends again with
the same deterministic `Message-ID`, which many clients and Exchange treat as a
duplicate. We chose this (at-least-once in that window) over at-most-once
because a missed renewal notice costs more than a rare duplicate. The
acceptance test kills the worker at every other point and asserts zero
duplicates and zero misses.

### D-008 No cross-request authorization cache — Accepted
Each request loads the principal (memberships, groups, roles) in one query and
evaluates grants against the DB. Revocation takes effect on the next request.
Add a cache only if profiling shows a need, and invalidate it on any
grant-table write.

### D-009 Organization-neutral naming — Accepted
`org.name` comes from config (set by `install.sh`, default "Policy and Document
Management System"). Org-specific roles use neutral keys with editable display
names: `legal_counsel` (which an organization might display as "General Counsel's Office") and
`department_head`. Requirements like "Appointed by the board" are
admin-entered data, not seeds.

### D-010 Local break-glass requires TOTP — Accepted
ASVS L2 expects MFA for administrative access. A password-only local admin
account would be the weakest path into the system.

### D-011 Entra identity key is `oid`, not `sub` — Accepted
`sub` is pairwise per application registration. `oid` is stable across apps
and matches Graph.

### D-012 Department hierarchy deferred — Accepted
Departments are flat in Phase 0. A sub-department needs its own grant. Parent
links can be added later without changing the grant model.

### D-013 Package renamed `pdnms` → workspace `pdms` — Accepted
The scaffold's `Cargo.toml` package name looked like a typo. The root is now a
virtual workspace and the binary crate is named `pdms`.

### D-014 CSRF: HMAC token bound to the session, plus Origin check — Accepted
The token is HMAC-SHA256(key derived from `PDMS_SECRET_KEY`, session token).
It is accepted from `X-CSRF-Token` (HTMX) or a `_csrf` form field, so forms
also work without JavaScript. Unsafe requests with a foreign `Origin` (or
`Sec-Fetch-Site: cross-site`) are rejected first. This replaced the
`sessions.csrf_secret` column from the reviewed schema: nothing needs storing.

### D-015 Conservative RustCrypto versions — Accepted
We use `argon2` 0.5, `aes-gcm` 0.10, `sha2` 0.10, `hkdf` 0.12, and `rand` 0.8
rather than the newer major lines. These are the widely deployed, audited
releases; the new lines changed APIs. Upgrade together in one change later.

### D-016 Compile-time-checked SQL for static queries; QueryBuilder for dynamic filters — Accepted
Static queries use `sqlx::query!` (checked against the schema, with offline
metadata in `.sqlx/`). Filtered lists (audit search; later document search) use
`QueryBuilder` with bound parameters, and the authz predicate is pushed in as a
`SqlFragment`. Integration tests cover the dynamic queries.

### D-017 `auth.local.require_totp` can be turned off only for demos — Accepted
The seeded demo users have no TOTP. Turning the requirement off logs a warning
at startup, and each such login is audited with a warning. `install.sh` never
turns it off.

### D-018 Runtime image: `debian:bookworm-slim`, non-root UID 10001 — Accepted
Distroless (gcr.io) was not reachable from the build network, and may not be
from restricted networks. Docker Hub images are widely mirrored. No packages are
added: rustls and the embedded time-zone data mean the binary needs nothing
else. `pdms healthcheck` replaces curl for container health checks.

### D-019 Secrets as files — Accepted
`install.sh` writes `secrets/` (directory 0700, files 0644 so non-root containers
can read their bind mounts). Compose mounts each file as a secret, and the app
reads `PDMS_*_FILE`. Secrets never appear in `docker inspect` environment
output or in the config file.

### D-020 Reading the audit log is audited; export is auditor-only — Accepted
`audit.searched` and `audit.exported` record who looked at what (with the
filter). Administrators can view and verify the log but cannot export it unless
they also hold `auditor`. This separates running the system from auditing it.

### D-021 OIDC discovery failure does not stop the server — Accepted
If the IdP is unreachable at startup, single sign-on is disabled (logged as an
error) until the next restart, and break-glass login keeps working. A
background re-discovery loop is a candidate improvement.

### D-022 OIDC users are auto-provisioned; email-linking only for unbound accounts — Superseded by D-086 (email linking removed)
New SSO users are created on first login with no access beyond what IdP group
mappings give them, which follows from deny-by-default. An administrator can
pre-create a user by email; the first SSO login links that account only if it
is not already bound to another identity. Set `auth.oidc.auto_provision = false`
to require pre-provisioning.

### D-023 Administrator lockout guards — Accepted
An administrator cannot deactivate their own account, and the last active
org-wide administrator cannot be removed or deactivated.

### D-024 Which denials are audited — Accepted (updated in Phase 1)
Every denial is recorded as `access.denied` with `outcome = 'denied'`:
- capability checks in the web layer (admin pages, the audit log);
- every document decision in `documents::access`, which records the
  document and the permission that was missing.

Lists never produce denials, because they only ever contain permitted rows.

### D-025 Entra ID tokens must carry `sub` — Accepted
The OIDC library rejects ID tokens without `sub` (required by the spec), even
though identity is keyed on `oid`. Entra always sends both.

### D-026 Content-addressed, write-once blob storage — Accepted
Files are stored under their SHA-256 and are never overwritten.
- **Local backend:** writes to a temp file, re-hashes while copying, fsyncs,
  renames into `ab/cd/<hash>`, and marks the file read-only.
- **S3 backend:** uses a conditional `PUT` (`If-None-Match: *`) for files up to
  16 MB and multipart uploads above that. It works with AWS, other S3-compatible
  interface, SeaweedFS, and MinIO.

MinIO no longer publishes freely pullable images, so the integration tests use
SeaweedFS's S3 gateway, which also honors the conditional `PUT`. The
`BlobStore` trait has no delete; purging under retention will be a separate
privileged operation.

### D-027 File types are decided by content, not by the client — Accepted (amended by D-118)
Uploads are sniffed from their first 64 KB and must match an allowlist: PDF,
Office (OOXML and legacy), OpenDocument, RTF, common images, text/CSV, and
email. Legacy Office/Outlook (OLE) and zip-based formats also need a matching
extension. Executables and unknown binaries are rejected, and every rejection
is audited. Downloads are `Content-Disposition: attachment` with `nosniff`,
using the stored, sniffed media type; only a short list of types may instead
be shown in the browser (D-118).

### D-028 CSRF for file uploads without JavaScript — Accepted
A plain HTML file form cannot send a header. On an allowlist of upload routes
only (`POST /documents`, `POST /documents/{id}/versions`), the middleware
defers the check to the handler. The handler requires `_csrf` as the **first**
multipart field and verifies it before reading any other field or file data.
Every other multipart request without the header is rejected.

### D-029 Custom fields: immutable key and type, versioned definition — Accepted
- **Values:** stored in `documents.custom_fields` keyed by the field's key,
  with numbers as exact decimal strings.
- **Definition changes:** renaming, re-labelling, or changing options or rules
  appends a definition version and never rewrites values.
- **Retired data:** removed options display as "(retired option)". Deprecated
  fields leave forms but keep their values in views, search, and exports.
- **Type changes:** changing a document's type keeps the old type's values.
- **Recording:** each version snapshot records which definition version
  applied to each value.

### D-030 Every content or descriptive change appends an immutable version — Accepted
Uploading a file, editing metadata, and restoring each add a
`document_versions` row with a full metadata snapshot and the file current at
that point. Database triggers reject UPDATE and DELETE on versions, blobs, and
field versions, even by the schema owner.

Restoring "details" restores title, description, dates, custom fields, and
tags. It never restores owner, department, status, or access settings, which
need their own permissions.

### D-031 Status transitions — Accepted, with one item deferred
- **Date-driven statuses:** only the scheduler (Phase 2) can set Renewal Due,
  Overdue, and Expired.
- **Leaving review:** In Review → Active needs `approve`.
- **Other transitions:** need `edit` and are validated against a fixed table
  (for example, Superseded can only become Archived).
- **Deferred:** blocking a user from approving their own submission is
  enforced by the Phase 3 approval workflow, which records who submitted. The
  authz function for it exists and is tested.

### D-032 Deletion is soft, retention-aware, and leaves a tombstone — Accepted
- **Blocked:** deleting needs `delete` and is refused while a legal hold
  applies or the retention period (document override, else the type's policy)
  has not passed. A policy measured from a date that is not set also blocks
  deletion.
- **Tombstone:** the `document.deleted` audit entry carries the reason and a
  full metadata snapshot.
- **Restore:** administrators can restore deleted documents.
- **Not built:** hard purge. It would be an owner-role procedure, like
  audit-log archival.

### D-033 Primary ownership — Accepted (answers an open question)
Transfer needs `manage_permissions` on the document. Administrators have it on
non-confidential documents through the seeded grant. The new owner must be an
active user and gets the owner permissions (`view`, `edit`). Owners cannot
delete, share, or approve without explicit grants.

### D-034 Which reads are audited — Accepted
Opening a document, downloading any version, exporting a list, and reading a
document's audit timeline are each audited. Ordinary search result lists are
not: they contain only permitted documents, and auditing every keystroke-level
search would bury the signal. Exports record the filter and the exported IDs.

### D-035 Search — Accepted
- **Engine:** PostgreSQL full-text search (`english` configuration) over
  title, ID, description, tags, custom text and select values, and file names,
  plus prefix matching on external IDs. File-content text arrives with OCR and
  text extraction in Phase 5.
- **Archived documents:** hidden unless requested.
- **Authorization:** every list query starts from the authz predicate, and the
  equivalence property test keeps it identical to the in-memory rules.

### D-036 Seeded retention policies are placeholders — Accepted
"Permanent" (policies) and "Contract: expiration + 5 years" (contracts and
ILAs) are starting points only. Each organization must confirm periods against
its records retention schedule. Records officers and administrators manage
policies, overrides, and legal holds.

### D-037 Tag colors through a generated stylesheet — Accepted
The strict CSP forbids inline styles, so `/tag-colors.css` emits one class per
tag. Colors are validated hex, and the text color is chosen for contrast.

### D-038 Optimistic concurrency on edits — Accepted
The edit form carries `row_version`. A stale save is rejected with an
explanation instead of silently overwriting someone else's change.
Resubmitting identical content is a harmless no-op.

### D-039 Who can read a document's timeline — Accepted
The owner, anyone with `manage_permissions` on the document, and audit
viewers. Client IP addresses are shown only to audit viewers.

### D-040 System actions have no `created_by` — Accepted
Documents and versions created by the system (demo seed, scheduler) store NULL
and display "System", rather than being attributed to a person.

### D-041 Reminders are materialized with a natural key — Accepted
- **Idempotent planning:** each run, the planner computes, per document,
  event, and rule, the latest reminder that should exist as of today. It
  inserts that reminder with a unique key (document, rule, event, due date,
  trigger, offset). Re-running, running two planners at once, or restarting
  after a crash creates nothing new. Acceptance test 5 kills the worker mid-send
  and checks that every email went out exactly once.
- **Missed reminders:** when several reminders were missed (downtime, or a
  document entered with past dates), only the latest is sent, never a burst.
- **After the due date:** a non-repeating reminder more than
  `stale_after_days` late is dropped.
- **Before the due date:** a late reminder still goes out.
- **Wording:** day counts come from the dates, not the trigger. A 90-day
  reminder sent 30 days late says "due in 60 days". Long-overdue items say
  "overdue since <date>".

### D-042 Rule precedence: document → type → organization — Accepted
Only active rules count. A document's own rule for an event replaces the
defaults for that event. Deleting a rule that already produced notifications
deactivates it instead, so history keeps its rule.

### D-043 Never notify someone who cannot open the document — Accepted
Recipients are checked against the document's permissions when a reminder is
planned and again when it is sent. Anyone without access (for example,
confidential documents, or someone who has since left) gets a `skipped`
delivery with the reason recorded.

### D-044 The scheduler owns date-driven statuses, in both directions — Accepted
- **What it manages:** only Active, Renewal Due, Overdue, and Expired
  documents. It moves them among those statuses, including back to Active
  once a review is recorded or dates are corrected.
- **People:** cannot move a document out of a date-driven status by hand.
  They complete the review or renewal task (or fix the dates), so the status
  always matches the dates.
- **Not managed:** Draft, In Review, Superseded, and Archived.

### D-045 Review and renewal tasks — Accepted
- **Creation:** one task per document, kind, and due date, created when the
  date is within `task_lead_days` (default 90) or past. A task becomes
  overdue after its due date, and is cancelled automatically if the date
  changes.
- **Completing a review:** sets the last review date and computes the next
  one from the completion date plus the interval (the document's own interval,
  else its type's).
- **Completing a renewal:** requires a later renewal or expiration date.
- **Approval:** optional. The person who completed a task cannot approve it
  unless the policy allows self-approval. This is the separation-of-duties
  check from D-031, now enforced for reviews; document approval routing is
  still Phase 3.

### D-046 "Next review" is computed when left empty — Accepted
It is the last review date, else the effective date, else the execution date,
plus the review interval.

### D-047 ICS feeds — Accepted
- **Token:** a revocable random token, stored hashed and shown once.
- **Content:** the feed is rendered at fetch time with its owner's current
  permissions, so revoked access and disabled owners disappear immediately.
  It never includes confidential documents, because feed URLs end up on
  phones and in shared calendars.
- **Recurrence:** reviews carry an `RRULE`, so future cycles show in Outlook.
- **Auditing:** fetches are audited at most once an hour per feed, since
  calendar clients poll constantly.

### D-048 Email delivery — Accepted
- **Transport:** SMTP via `lettre`: STARTTLS for Exchange Online, implicit
  TLS, or a trusted relay. A file transport exists for development.
- **No email configured:** email deliveries are recorded as skipped, and
  in-app notifications still work.
- **Messages:** each has a deterministic `Message-ID` and an
  `Auto-Submitted: auto-generated` header.
- **Retries and logging:** up to 6 attempts with exponential backoff. Every
  attempt is logged, and each successful send is audited.
- **Testing:** verified against a real SMTP server (Mailpit) in the test
  suite.

### D-049 Reminders go out after a local hour — Accepted
Reminders are planned all day but sent only after `send_hour` in the
organization's time zone (default 07:00). The UTC conversion goes through the
time zone, so daylight saving time is handled.

### D-050 Outlook calendar sync through Microsoft Graph — Deferred to Phase 5
As the spec plans. ICS subscriptions cover Outlook today.

### D-051 Workflows run in fixed stages: reviewers, legal, approvers — Accepted
The spec asks for named reviewers and approvers and a legal step "where
applicable". Stages are reviewers (parallel) → legal counsel → approvers
(parallel, all must approve). Arbitrary step graphs were not needed and would
make the screens harder to follow. The data model (`stage` per step) allows
custom orders later.

### D-052 Approval steps require the approve permission — Accepted
Being named an approver does not grant authority by itself: approval steps
also need `approve` on the document from the central engine, legal steps need
`view` plus the role, and the submitter can never sign off on their own
submission unless `[workflow] allow_self_approval` is set. Routing is validated at
submission (no step may be impossible to complete), and checked again at
decision time.

### D-053 Seeded grants for legal counsel and department heads — Accepted
Migration 0005 seeds two editable role grants so the workflow works out of the
box: Legal Counsel may *view* non-confidential documents org-wide, and
Department Head may *approve* (holders assigned for a department: that
department's documents only). Neither reaches confidential documents. Both
roles have no holders until an administrator assigns them.

### D-054 Failed rounds return the document; history is per round — Accepted
Reject and request changes end the round (remaining steps are marked "not
needed") and return the document to Draft, or to Active for a revision of an
active document (the planner re-applies Overdue/Renewal Due). A new round is
a new submission; every round is kept.

### D-055 Approval is refused after a new file during review — Accepted
Approvals refer to the file version submitted. If a new file version is added
during review, approval is refused until the review is cancelled and
resubmitted. Metadata edits do not count.

### D-056 One notification pipeline for reminders and action notices — Accepted
`notifications` gained `kind` (reminder/action), `appointment_id`, and stored
subject/body/link for action notices, with a `dedupe_key`. Action notices are
written in the same transaction as the workflow change that causes them, and
reuse deliveries, the email job, visibility checks at planning and at send
time, the inbox, and the delivery log.

### D-057 Signatures are tracked, not collected — Accepted
As the spec requires: status, timestamps, who recorded it, and the signed copy
as a version. A `SignatureProvider` trait (built-in: manual) is the seam for an
e-signature service, which would set `provider_ref` and record results through
the same `record_signed`. Only never-requested signers can be removed.

### D-058 Appointments are never edited into history — Accepted
Reappointment and replacement end the current appointment and create a new,
linked one; resignations end it. History is therefore the list of rows, and
each term has its own checklist. An active appointment past its end date is a
holdover (common for boards whose members serve until a successor qualifies),
shown as overdue rather than ended automatically.

### D-059 Appointment visibility and roles — Accepted
New role `appointments_clerk`. Capabilities `ManageAppointments` (admin,
clerk) and `ViewAllAppointments` (admin, clerk, auditor). Otherwise a position
is visible to its manager and its department's members, and the manager may
record checklist items. Appointment reminders, calendar entries, and ICS
events follow the same rule.

### D-060 Appointment reminders reuse rules with a position scope — Accepted
`due_event` gained `term_end` and `reappointment` (in their own migration,
because Postgres cannot use a new enum value in the transaction that adds it).
Rules can be organization-wide (seeded) or per position (core API; a per-position
editor in the UI is left for later). "Owner" means the position's manager for
these events.

### D-061 The audit log is the outbound event stream — Accepted
Webhooks, integration providers, and audit forwarding read audit entries after
a stored, row-locked cursor. Audit entries are written in the same transaction
as every change and their sequence numbers commit in order, so nothing is
missed and no service needed changes. A new consumer starts at the current end
of the log (history is not replayed). Published: document, workflow,
signature, review task, appointment, and position actions; never reads
(views, downloads) or denials.

### D-062 Webhooks act as a service user — Accepted
A webhook names a user; an event is delivered only if that user may view the
document or position it is about, decided by the central engine at dispatch
time. Administrators therefore control what a receiving system learns with
the same grants as everywhere else. Integration providers run as part of the
organization's own installation and receive every published event.

### D-063 Webhook delivery: signed, HTTPS, retried by the job queue — Accepted
JSON POST with `X-PDMS-Event`, `X-PDMS-Delivery` (stable across retries), and
`X-PDMS-Signature: t=<ts>,v1=<HMAC-SHA256("<ts>.<body>")>`. Secrets are shown
once and stored encrypted with a key derived from the master secret. HTTPS
only (plain HTTP behind a test-only flag); redirects are not followed.
Retries use the job queue's exponential backoff; every attempt is logged and
can be redelivered.

### D-064 API tokens act as their user, limited by scopes — Accepted
`pdms_`-prefixed secrets, SHA-256 stored, shown once, required expiry (max
`api.max_token_days`), revocable by the owner or an administrator. Each
request loads the user's current principal, so revocation, deactivation, and
grant changes apply immediately. Every request is logged (`api_request_log`,
purged after `request_log_days`); token use is audited at most hourly, and
every audited action through the API records the token ID. Per-token rate
limit. Scopes: documents read/write, calendar, appointments, reports, audit.

### D-065 The first API is read-mostly — Accepted
Integrations named in the spec (records platforms, records-request portals) need
to read documents, files, dates, and history. Writes are limited to creating
document records and validated status changes. File upload, edits, workflow,
and appointment changes through the API are left for when an integration
needs them; the web app remains the place for those.

### D-066 Records requests need the export permission — Accepted
Building packages requires the `BuildExportPackages` capability *and* the
`export` permission on every document (checked per document, denials
audited). Migration 0006 seeds a global `export` grant for the Records Officer
role, which by design never reaches confidential documents ("excludes from
broad exports"); those need an explicit document grant. The search offered to
records officers is the same authorized search.

### D-067 Package format and storage — Accepted
ZIP with README, `manifest.json`, per-document metadata, files, optional audit
history (JSON and CSV), and `SHA256SUMS`. Each stored file is re-hashed while
packaging and must match. The package is stored in the blob store (content
addressed, never overwritten) with its SHA-256 and the manifest's SHA-256 in
`export_packages`, so the organization can prove exactly what it released.
Only the requester and auditors can download it; both actions are audited,
and each document's timeline shows the export with the request reference.

### D-068 No audit checkpoint table — Accepted
The Phase 0 plan listed `audit_checkpoints` for archival. Nothing purges audit
rows, and audit forwarding (file or syslog, each entry with its hash) gives an
independent, external copy, which is what checkpoints were for. Revisit if
audit archival is ever required.

### D-069 Backups: database first, then files — Accepted
PDMS writes a file before committing the rows that reference it and never
modifies or deletes files, so a dump followed by a file-store copy is always
consistent. `deploy/backup/pdms-backup.sh` does exactly that (plus
configuration and secrets, with checksums); `pdms-restore.sh` restores and
ends with `verify-integrity`. For backup software that supports it, a pre-script that runs the backup
script into a protected directory is recommended over volume snapshots.

### D-070 Reports are computed live — Accepted
Reports run against current data through the same authorization as the rest
of the app (no reporting tables to keep in sync or secure separately). CSV
exports are audited, and cells that a spreadsheet would treat as formulas are
prefixed with `'`.

### D-071 Test databases clean up after themselves — Accepted
The test kit's Postgres container lives for the whole test process (in a
`static`, which Rust never drops), so it is removed by a detached watcher once
the process exits. Local runs can instead point `PDMS_TEST_DATABASE_URL` at
the dev server; test databases are named after their process and those of
finished runs are dropped on the next run. `sqlx::migrate!` crates have a
`build.rs` so a new migration always rebuilds them.

### D-072 Comments: one level of threads, anyone who can view may comment — Accepted (amended by D-091)
A comment and its replies are enough for document discussions and keep the
page readable. Commenting never changes the record, so viewers may comment by
default (configurable to require `edit`). Comments respect the document's
ACL for reading, writing, and notifications.

### D-073 Comment edits and deletions never lose text — Accepted (amended by D-091)
Every version is a row in `comment_revisions`, protected by triggers like the
version tables. Deletion is soft. Earlier versions are shown to the author,
permission managers, and auditors (the audit log also holds the text). Only
authors edit; authors and permission managers delete.

### D-074 Mentions by email or unique handle — Accepted
`@carol@example.org` or `@carol` (the email's local part, if exactly one
active user has it). Ambiguous or unknown handles are left as text. Mentioned
users who cannot view the document are never notified (their delivery is
recorded as skipped). Newly added mentions on edit are notified.

### D-075 Text extraction in Rust; OCR in a sidecar — Accepted
Embedded PDF text (`pdf-extract`), Word XML, and text files are read in Rust.
OCR uses OCRmyPDF/Tesseract in an optional container with a narrow HTTP
interface (`POST /ocr`), as the spec allows for capabilities Rust lacks; its
wrapper is a small Python standard-library script because OCRmyPDF is
Python. A PDF with less than `min_text_chars` embedded characters is treated
as scanned. Text is stored per version, reused for identical files, and the
latest is part of full-text search (weight D).

### D-076 Dates found in files are suggestions only — Accepted
`ocr::dates` finds written dates and classifies them by the nearest preceding
keyword in the same sentence; terms and review cycles are recognized too.
Suggestions equal to the current value are not stored. A person with `edit`
must accept each one; accepting writes an ordinary audited metadata version.
Confidence values describe the classification, not the correctness of the
date, and the source sentence is always shown.

### D-077 Outlook events through Graph: reconcile, don't push — Superseded by D-087
Instead of hooking every date change, a periodic job computes the events that
should exist and reconciles them with stored links (content hash per event):
create, update, recreate if deleted in Outlook, move between mailboxes, or
delete. It is idempotent, survives outages, and catches changes from any
path. Application permissions with client credentials, scoped to specific
mailboxes by an Exchange application access policy. Confidential documents
are excluded (a shared calendar is a broad audience), as in ICS feeds.

### D-078 Graph and OCR are off by default — Accepted (Graph part superseded by D-087)
Both depend on outside services (an Entra app registration and a mailbox; an
OCR container). The system is fully functional without them; ICS feeds and
embedded-text extraction cover the common cases.

### D-079 Guided installer — Accepted
Modeled on the organization's existing installer style: the installer asks
everything first (proxy mode, free host ports, optional database port, name,
time zone, Entra ID, SMTP, Outlook, OCR, ClamAV, first administrator), then
builds and starts unattended. Secrets stay in Docker secret files (never in
`.env`); `.env` holds only deployment settings. Managed configuration blocks
are marked so `--configure <part>` can redo one part without touching
hand-edited settings, and settings from earlier installers are adopted, not
overwritten. Services restart only when a setting changed. The bundled Caddy
option covers sites without a reverse proxy (Let's Encrypt for a domain, an
internal certificate for an IP address).

### D-080 License: AGPL-3.0-or-later — Accepted
Chosen by the organization. All Rust dependencies are under permissive,
AGPL-compatible licenses (checked with `cargo metadata`), as are the vendored
htmx (0BSD) and the bundled root certificates (CDLA-Permissive-2.0). To make
section 13 (network use) easy to honor, every page links to `/source` (which
redirects to `[org] source_url`, or explains how to ask while it is unset; the
server warns at startup until it is set) and `/license` (the text compiled
into the binary). Source files carry SPDX identifiers; migrations do not,
because editing an applied migration changes its checksum and would stop
existing installations from starting.

### D-081 SharePoint holds the files; PDMS keeps everything else — Accepted (built in P3, 2026-10-22)
The county wants documents to live in SharePoint and be browsable there,
while PDMS stays the application everyone uses. Three options were
considered:

- SharePoint stores the files only;
- SharePoint is the system of record for metadata, permissions, and search
  too;
- a split by stage (drafts in SharePoint, records in PDMS).

Files-only was chosen. It keeps everything PDMS already does: date-driven
reminders, workflow, restricted documents, and the tamper-evident audit
trail. It is also the smallest change: a new storage backend plus sync.
Metadata, workflow, reminders, appointments, permissions, audit, and the
search index stay in PostgreSQL. SharePoint is the file store and a
read-only browsing mirror, and PDMS is authoritative when they disagree.
Spec: `docs/superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md`.

### D-082 App-only Graph access to one site; downloads go through PDMS — Accepted (certificate sign-in built in P2; downloads through PDMS built in P3; the one-site grant documented in SHAREPOINT_SETUP.md, P4, 2026-10-23)
PDMS authenticates to Microsoft Graph as itself, with a certificate rather
than a client secret, using `Sites.Selected`. It is granted full control on
one dedicated site only; full control is needed to manage folder
permissions.

Users never receive SharePoint links from PDMS. Every download is
authorized, streamed, and audited by PDMS, whereas a pre-authenticated
SharePoint URL would bypass both checks for its lifetime. Delegated
(per-user) Graph access was rejected because every user would then need
SharePoint permissions, which is what PDMS's own model replaces.

### D-083 Search stays in PDMS — Accepted (design, not built)
Text is extracted from each file at upload, while PDMS holds the bytes, and
indexed with the document's details. Results are filtered by PDMS
permissions. Microsoft Search would filter by SharePoint permissions, which
under D-081 are locked to the PDMS app, so its results could not match what
each user may see in PDMS.

### D-084 SharePoint layout with folder-level permissions only — Accepted (layout and columns built in P3; department folder access built in P4, 2026-10-23)
**Layout.** One site and one library, with top-level `Departments/`,
`County-wide/`, and `Restricted/`. Under each are department folders, then a
folder per document (`{ID} – {title}`), then one file per version. Files
are never overwritten.

**Columns.** PDMS writes metadata columns (type, tags, status, dates, owner,
current version, link to PDMS), so people browsing read-only can sort and
filter.

**Permissions.** These are set only on the top-level and department
folders, and changing a document's visibility moves its folder. Per-file
permissions were rejected because SharePoint handles large numbers of
unique permissions poorly. PDMS grants each department folder to that
department's people individually, because Entra groups are not available
(D-086).

### D-085 Three visibility levels replace "confidential" and "inherit permissions" — Accepted (visibility built in P1; SharePoint folder moves built in P3, 2026-10-22)
Each document has one of three visibility levels:

- **Department** (default): its department, plus anyone it is explicitly
  shared with.
- **County-wide:** every active county account may view it; editing rights
  are unchanged.
- **Restricted:** explicit grants and the owner only. This is today's
  confidential behaviour.

The owner chooses the level at upload and can change it later. Every change
is audited. A change that tightens access moves the SharePoint folder before
the request completes.

The two existing flags (`is_confidential`, `inherit_acl`) had identical
effects in the authorization engine, and adding a third would allow
contradictory combinations. One enum therefore replaces both; this amends
the "broken inheritance" part of D-003. The interface says "County-wide",
not "Public", so it is never confused with publishing outside the county.

### D-086 Users from Entra; department from the Department field — Accepted (built in P2, 2026-10-21; amended by D-117)
The county's AD groups are not synced to Entra, so groups cannot drive
access. Instead, a nightly and on-demand Microsoft Graph `/users/delta` sync
creates, updates, and deactivates member accounts.

**Departments.** Each user's Department attribute is mapped to a PDMS
department through an admin-maintained list of values. Several spellings
can map to one department. Users whose value is blank or unmapped can sign
in but see only what is shared with them, and they are listed for admins.
The account-creation procedure must therefore set the Department field.

**Other rules.**

- Roles are assigned in PDMS.
- Guest (B2B) accounts are excluded.
- Single sign-on matches users by Entra object ID only. Email-based account
  linking (D-022) is removed, because the sync already knows every object
  ID.

### D-087 Calendar is internal; the Outlook calendar sync is removed — Accepted (done 2026-10-22, with R2)
The calendar was always meant to be PDMS's own look-ahead at upcoming
reviews, renewals, and expirations (month and agenda views, dashboards). The
optional Outlook calendar sync built in Phase 5 (D-050, D-077) is not wanted.
It will be removed (code, configuration, installer step, docs), which also
narrows the Graph permissions PDMS needs. ICS feed subscriptions are not
affected by this decision. Supersedes D-077 and the Graph part of D-078.

### D-088 ILA document type; admin-curated tags — Accepted (done 2026-10-22, with R2)
The seeded "Partnership Agreement" type is renamed **Interlocal Agreement
(ILA)**, with ID prefix `ILA`. Admins can still add their own types.

- **Types** say what a document *is*. A document has exactly one, and it
  drives numbering, the review cycle, legal review, signatures, retention,
  and extra fields.
- **Tags** are cross-cutting labels, any number per document.

Tags become admin-curated (`[documents] free_form_tags = false`), so the
filter list stays short and consistent for a county-wide audience.

**Done 2026-10-22** (with R2 part 4, migration `0011`): the type's key became
`interlocal_agreement`; a name or prefix an administrator had already changed
is kept, and existing documents keep their `PAG-` IDs. `free_form_tags` now
defaults to `false`. Editors pick from curated tags, while tag managers can
still create one. Tags typed earlier show under **Needs review** on the Tags
page. The Outlook removal (D-087) went in the same migration: queued sync jobs
are deleted and `graph_calendar_links` is dropped. Events already created in
Outlook stay until someone deletes them.

### D-089 Changes made directly in SharePoint are detected, not prevented — Accepted (built in P4, 2026-10-23: D-105, D-106)
One or two named site owners keep edit rights for emergencies, so PDMS
cannot rely on SharePoint permissions alone. Every few minutes PDMS reads
the site's change feed (Graph delta). If a file is edited, added, or deleted
by anyone other than PDMS, the document is flagged, the change is audited,
and admins are alerted. An admin then does one of two things:

- restores the original, from SharePoint version history or the recycle
  bin;
- accepts the change as a new PDMS version, which goes through the normal
  upload checks.

A Purview retention policy on the site is recommended, so files cannot be
permanently deleted.

### D-090 Calendars and dashboards show every document you can see — Accepted (2026-10-15)
A person's calendar, dashboard, and ICS feeds include every document they
may view, County-wide ones included. Decided by the owner.

### D-091 County-wide comments are owner/admin only; only authors delete comments — Accepted (2026-10-15)
On a County-wide document only the owner and PDMS administrators (the
org-wide `admin` role) may comment or reply; everyone who can view still
reads the comments. On every document only a comment's author may delete it
(not administrators, owners, or `manage_permissions` holders), and the author
may edit it only while still allowed to comment. Reason: checks and
balances. No one can remove someone else's words, and soft-deleted text stays
in the history and the audit log. Refused attempts are audited
(`comment.post_denied`, `comment.edit_denied`, `comment.delete_denied`) and
not published to webhooks. Amends D-072 and D-073.

### D-092 UI direction D; R1 scope and order — Accepted (2026-10-15)
The project owner chose direction "D" from four demo mockups (A Civic Modern,
B Clean Workspace, C Executive Overview, D = A's layout in C's style): a
sidebar shell and one visual system. Round 1 (R1) covers the new shell on
every page plus five redesigned screens (sign-in, dashboard, documents list,
document page, county overview), the branding admin page, and username
sign-in. R1 is done before sub-projects 1–3 so there is a presentable shell
to show county leadership. Round 2 (R2, the remaining pages) comes after
sub-projects 1–3 so it covers the final features. See
[the R1 spec](superpowers/specs/2026-10-15-ui-redesign-r1-design.md).

**Update 2026-10-21:** R1 is built and merged.
- The owner started R2 now, with a second contributor, in parallel with
  sub-projects 1–3 rather than after them.
- R2 therefore coordinates with the work that touches the same screens: plan
  P2's Entra ID sync page (since merged), P4's SharePoint page, and sub-project 3's
  removal of the Outlook sync and change to tags.
- See [the R2 brief](superpowers/specs/2026-10-21-ui-redesign-r2-brief.md).

### D-093 Branding is set on an admin page and is public before sign-in — Accepted (2026-10-15)
Administrators set the county name, accent color, and seal under
Administration → Branding (the settings permission); no developer or config
edit is needed. The accent must give white text a contrast ratio of at least
4.5:1 or it is refused; hover, link, and tint shades are derived. The seal is
a PNG or JPEG of at most 512 KB, 16–2048 px per side, checked by its real
file type (SVG is refused), and is stored in PDMS's database. The seal and
name appear on the sign-in page, so they are public to anyone who can reach
PDMS. One organization-name resolver supplies the name to page titles, the
sidebar, notification emails, calendar feeds, webhooks, and records-request
packages; a blank name falls back to `[org] name` in the config file.
Changes apply at once in the saving process and within 60 seconds elsewhere.

### D-094 Sign-in page layout and usernames for local accounts — Accepted (2026-10-15)
The sign-in page shows username and password first, then "Sign in with
Microsoft", which is always visible. If Microsoft sign-in is not configured,
clicking it returns to the sign-in page with a "not set up yet" notice; if
local sign-in is disabled in config, only the Microsoft button shows. Local
accounts may have an optional username (2–64 characters of `A-Z a-z 0-9 . _
-`, never `@`, unique ignoring case). The Username field accepts a username
or an email. Every failed sign-in shows one generic message, so the page does
not reveal which accounts exist.

### D-095 Dashboard shows "Recently updated", not an activity feed — Accepted (2026-10-15)
The dashboard lists recently updated documents (only ones the viewer can
see) instead of an activity feed, because a feed built from the audit log
would expose it. "Needs your attention" holds only the viewer's own tasks and
approvals; "Coming up" and the tiles cover everything the viewer can see.
There is no "Share" button: PDMS has no sharing model apart from access
grants. "Your access" moved to the account page.

**Amended 2026-10-22 by D-099:** the dashboard now has an activity feed,
built so that it cannot expose the audit log.

### D-096 Graph app-only authentication with a certificate (PS256) — Accepted (2026-10-21; amended by D-117)
PDMS signs a client assertion with its own certificate (header `alg` PS256,
`x5t#S256` SHA-256 thumbprint, ten-minute lifetime), as the Microsoft
identity platform documents; there is no client secret to leak or rotate in
config. The certificate and key are one PEM file supplied through
`PDMS_GRAPH__CERTIFICATE_FILE`; `pdms gen-graph-cert` creates it (RSA-2048,
two years by default) and writes the public certificate beside it as
`<file>.crt`, never overwriting either file. Signing uses `ring`; the `rsa`
crate is used only to generate the key once. The bearer token is sent only
to `[graph] api_url`'s origin, never through a redirect, and
pre-authenticated download and upload URLs are fetched without it. Downloads
have no total deadline (only connect and read timeouts) so large files
stream; JSON calls and upload chunks have a per-request timeout.

The directory sync (D-086) follows these rules. It never reactivates an
account and never links accounts by email. A full round also departs people
the directory no longer lists, but holds back and reports if no member
accounts come back or if more than max(5, 20%) of linked accounts would be
departed. The last administrator is never departed automatically; it is
reported. A linked account that also has a local password is departed like
any other and reported, because a departed employee with a working password
is the larger risk; the break-glass account has no directory link and is
never touched. While it is on, group mappings from the identity provider are
not applied and the token never sets names or groups; configuration requires
`auth.oidc.subject_claim = "oid"`, and runs are refused while it is off.

**Confirmed by the owner (2026-10-28):** a linked account that also has a
local password is departed like any other, and group mappings from the
identity provider are not applied while the sync is on. The P2 build left
two timing cases open. The case where a Department mapping change races a
run applying the same person is fixed with the group-scoped sync (GS; its
[spec](superpowers/specs/2026-10-27-entra-sync-group-scope-design.md) §3).
The case of a mapping edit made while the sync is off stays as it is. GS
also amends "never reactivates": people the sync departed for leaving the
PDMS group are reactivated once they are back in it.

### D-097 R2 scope, order and admin navigation — Accepted (2026-10-22)
The owner's answers to the R2 brief's questions, recorded in the
[R2 spec](superpowers/specs/2026-10-22-ui-redesign-r2-design.md) §2:
- **Scope:** every screen in brief §3.1 and §3.2, plus the new-document and
  edit forms and the workflow, signature and comment sections. Version and
  comment history, the error page, `/source` and `/license` stay as they are.
  Dark mode, an activity feed and a browser-tab icon are in R2 (D-098 to
  D-100).
- **Admin navigation:** one grouped menu instead of the tab row: People and
  access; Documents and records; Notifications; Integrations; System. Every
  admin page follows one pattern (spec §5.2), which P4's SharePoint page
  follows too.
- **Sub-project 3** (D-087, D-088) was built inside R2's admin part, at the
  owner's call, and the OCR status has its own *Text extraction (OCR)* page.
- **Phone checks:** calendar, notifications, appointments and the account
  page. Admin pages are checked at desktop width.
- **Delivery:** built on one branch as one pull request, before the spec was
  formally approved; the owner approved it on 2026-10-22.

Updates D-092: R2 ran alongside sub-projects 1–3 rather than after them.

### D-098 Dark mode is the default, with a per-person switch — Accepted (2026-10-22)
Everyone sees dark mode until they choose otherwise, under **Your account →
Appearance** (Dark, Light or Match my device) or with the one-click switch
in the sidebar.
- **Storage:** the choice is stored per user (`users.color_scheme`, migration
  `0012`), written into the page as `<html data-theme>`. This needs no
  JavaScript and avoids a flash of the wrong colors.
- **Audit and control:** each change is audited (`user.color_scheme_set`).
  Only the person changes their own setting. Pages with nobody signed in use
  dark.
- **Colors:** dark mode has its own palette (spec §7), and a test holds every
  text pair at 4.5:1 and every mark at 3:1 in both modes.
- **Accent:** Branding refuses an accent for which readable dark-mode shades
  (link, mark, tint) cannot be derived, as it already refused accents too
  light for white text.
- **Print** is always light.

### D-099 Dashboard activity feed, limited to what the page already shows — Accepted (2026-10-22)
"Recent activity" replaces "Recently updated" on the dashboard (amends
D-095). It reads the audit log, so it is narrow by construction
(`pdms-core::activity`):
- **Which events:** a fixed list of document actions with outcome `success`.
  Views, downloads, exports, permission and admin changes, denied or failed
  attempts, and comment edits and deletions are never shown.
- **Which documents:** only those the viewer may view **now**, through the
  same visibility predicate as search (D-085, D-090). A document narrowed
  after the event drops out.
- **Names:** the actor's name only for kinds whose actor the document page
  already shows to anyone who can view it. The event's changes and context
  are never read.
- **Limits:** the last 90 days, ten items. Viewing the feed is not audited.

It is on the dashboard only; there is no separate activity page.

### D-100 Browser-tab icon from the seal — Accepted (2026-10-22)
Every page links the county seal as its tab icon when one is set (the seal is
already public, D-093), otherwise a generic PDMS icon shipped in the binary
(`/static/icon.svg`). `/favicon.ico` serves the same, for pages that don't
know the branding and for browsers that ask on their own. PDMS does not
resize the seal.

### D-101 County overview counts: "Other" excludes superseded; overdue tile says what it counts — Accepted (2026-10-22)
The status chart's "Other" counts draft and expired documents only, so the
chart's total equals the Documents tile, which already left superseded
documents out. The overdue tile is labelled "Overdue dates", with the line
"Documents and board terms", because it counts overdue dates of documents
and board terms, while the chart's Overdue segment counts documents in
Overdue status.

### D-102 SharePoint storage: the owner's choices before P3 — Accepted (2026-10-22)
Six questions from P3's pre-flight check, answered by the owner:
1. **Long text is truncated in SharePoint.** Single-line columns, and the
   built-in `Title`, hold 255 characters, while PDMS titles can be 500.
   PDMS sends at most 255 characters and keeps the full value itself.
2. **A SharePoint outage keeps the person's form.** "File storage
   (SharePoint) is unavailable" is shown on the new-document and edit forms
   like a validation error, and as a message on other pages, instead of a
   503 error page. Nothing was saved, so they can try again.
3. **Folder moves in SharePoint are audited**, as one action,
   `sharepoint.folder_moved`, with the old and new path and the reason. It
   is written in the same transaction as the change it records.
4. **No real data is stored yet.** Turning SharePoint on makes files stored
   locally unreadable, so the test server is reset at that point, as the
   spec's rollout says.
5. **Orphans are accepted for now.** These are staged uploads whose save then
   fails, and folders moved before a change that then fails. The nightly
   mirror sweep and P4's change check find them. This is listed as a known
   gap.
6. **SharePoint calls while a visibility change holds its row lock are
   accepted**, because they are rare and short.

### D-103 SharePoint uploads use a temporary name first — Accepted (2026-10-22)
The spec (§7.1) says a file is uploaded straight to its final name, which
includes the version number. PDMS only knows that number inside the
database transaction that saves the version, and the upload has to finish
before that transaction starts, so that a failed upload saves nothing.
So each file is uploaded into its document's folder as
`<document ID> (uploading <random>).<ext>`. The `sharepoint.mirror_document`
job renames it to its final name once the version is saved. Until the job
runs, which is normally within seconds, someone browsing SharePoint sees the
temporary name. If the save fails after the upload, the temporary file is
left behind as an orphan (D-102, point 5).

### D-104 Department folder access: one read grant per person, Entra accounts only — Accepted (2026-10-23)
Entra security groups are not available to PDMS (D-086), so each member of a
department gets their own `read` permission on `Departments/<department>`
through the Graph invite call (sign-in required, no invitation email). Only
active members whose account comes from Entra get one; local break-glass
accounts never do. PDMS records each grant and removes only what it
recorded. Other access on the folder is reported and never changed. Runs are
coalesced to one per minute after membership changes (including those the
Entra sync makes), and repeat nightly. Permissions SharePoint copies from the
site when a folder's inheritance breaks are not reported.

### D-105 Changes in SharePoint are found by comparing file hashes — Accepted (2026-10-23)
A tracked file counts as changed when its size or QuickXorHash in the change
feed differs from what PDMS recorded at upload. `lastModifiedBy` is shown and
audited but never trusted: it decides only whether a rename or a column
change needs PDMS's layout put back. An expired change token (HTTP 410)
rereads everything; unchanged files are not flagged. The owner's answers:
- **One audit shape.** The download check (P3) and the change check write the
  same `sharepoint.changed_outside` event (category system, outcome failure,
  context `source`, `item_id`, `detail`), naming no title.
- **A change first found by a download also notifies administrators**, once;
  the next change check then records who made it, without a second notice.
- **The document page** shows everyone who can see the document a status
  message, not an alert: "A file of this document was changed outside PDMS.
  Downloads of it are paused until an administrator checks it."
  Administrators also get a link to Administration → SharePoint.

### D-106 Files PDMS does not track: listed after ten minutes, never deleted — Accepted (2026-10-23)
An upload in progress has no database row until it commits, so untracked
files are listed only once they are ten minutes old. PDMS never deletes them;
an administrator deletes them in SharePoint or dismisses them (audited). A
file PDMS uploaded for a save that then failed (D-102, point 5) is labelled as
such.

### D-107 SharePoint administration: `ManageSharePoint` — Accepted (2026-10-23)
A new capability, held by the org-wide administrator role, gates
Administration → SharePoint and the restore, re-check, and dismiss actions.
"Accept as new version" also needs `edit` on the document, through the normal
version check, because it adds content. "Restore original" picks the
SharePoint version by SHA-256, never by its label. A SharePoint outage while
resolving shows "…so the change was not made" (D-102). The owner's answer: for
a document the administrator cannot open, the page shows only "A Restricted
document" and the version number, no ID, title, or path (D-085).

### D-108 The installer's `[storage]` block selects SharePoint storage — Accepted (2026-10-23)
The owner's answer: one managed `[storage]` block in `config/pdms.toml`,
written by both `--configure sharepoint` and `--configure clamav`, holds
`backend` and `clamav_addr` together (a TOML table can't be split across two
blocks). Older installs' ClamAV block is moved into it the first time. A
hand-written `[storage]` is left alone, with a warning to set `backend`
there. This replaces the plan's `config/pdms.env` and Compose `env_file`, so
no newer Docker Compose is needed. On an install that already has documents,
switching to SharePoint asks for confirmation (D-102, point 4); `none` as the
site address switches it off again.

The Graph certificate is always mounted as the `graph_certificate` secret; an
empty file means "not configured". The owner's answers: it is P2's file pair,
`secrets/graph_certificate` and its public half `secrets/graph_certificate.crt`,
created by the installer with `openssl` only when missing (the PDMS image is
not built yet at that point); the installer shows both its SHA-1 thumbprint
(what the Entra admin center lists) and its SHA-256 thumbprint (what
Administration → Entra ID sync shows). `--configure sharepoint` writes
`[graph]` and `[sharepoint]` but never `[entra_sync]`: the Entra user sync
stays a separate step that uses the same certificate.

### D-109 `verify-integrity --rehash-sharepoint` — Accepted (2026-10-23)
By default `verify-integrity` checks SharePoint files through Graph metadata
(present, size, QuickXorHash) without downloading them. Downloading every file
and comparing SHA-256 is opt-in, because it can take hours on a large
library.

### D-110 Graph sign-in failures: one notice an hour — Accepted (2026-10-23)
HTTP 401/403 from Microsoft Graph means the certificate, consent, or site
grant is broken, and every run fails the same way. Administrators get one
in-app (and email) notice per hour, and the status page shows the latest
error. The notice names no document.

### D-111 Settings move into Administration; the configuration file wins — Accepted (2026-10-23)
The owner wants the installer to be for the first installation only, and
everything else set up, changed, tested and checked in Administration (S1,
[spec](superpowers/specs/2026-10-23-admin-settings-design.md)). The owner's
choices:
1. **The configuration file wins.** A setting in `config/pdms.toml` or a
   `PDMS_*` variable is used and shown read-only ("set in the configuration
   file"); a `PDMS_*_FILE` counts only when its file isn't empty, so the
   always-mounted, empty Docker secrets lock nothing. Everything else is set
   on the page. Nothing written by hand changes behind anyone's back.
2. **Changes apply without a manual restart** (D-112).
3. **Secrets are stored in the database, encrypted** with a key derived from
   the master key and bound to the setting's name, and never shown again once
   saved. A database backup then holds them, encrypted, so the master key's
   backup matters even more.
4. **Permission:** `ManageSettings` (organisation-wide administrators). Every
   change is audited (`settings.changed`), secrets only as "replaced" or
   "cleared".
5. **The installer** keeps the first installation and `--configure network`,
   `database-port`, `source` and `admin`; `pdms settings` is the fallback for
   a server whose admin page can't be reached.

### D-112 Settings apply live; the worker rebuilds itself — Accepted (2026-10-23)
A save checks the merged configuration and builds its services before
anything is stored, so a setting that can't take effect is refused with the
reason. The web server swaps one snapshot (`Core::live`) of the effective
configuration and its services; requests already running finish with the
old one. The worker's jobs and schedules depend on the settings, so it
stops cleanly (running jobs finish) and starts again with the new ones,
inside the same process. That works the same under Docker, systemd and
`serve --with-worker`, so it is not done by exiting the process, as the spec
first said. Processes hear of a change through `NOTIFY pdms_settings` and
check `settings_version` every 60 seconds; if stored settings can't be
built, the running ones stay and the error is logged.

### D-113 The "PDMS Service" certificate is made in PDMS — Accepted (2026-10-23)
Administrators make the certificate on **Administration → Microsoft 365 →
Connection** (RSA-2048, two years). Its private key is stored encrypted in
`app_secrets` and never leaves PDMS; only the public certificate can be
downloaded. The first certificate is active at once, and PDMS signs in with
it as soon as the tenant and client IDs are saved (before that, Microsoft
Graph stays off rather than half set up). A later certificate waits as
pending until **Use this certificate**, so it can be uploaded to Entra first:
renewal has no outage, and the old private key is deleted when it is
replaced. A certificate in `secrets/graph_certificate` still wins (D-111).
The connection test signs in with the newest certificate and reads one user,
without saving anything, and reports a missing `User.Read.All` as a note,
because SharePoint storage doesn't need it. Administrators are reminded 30
days before the active certificate expires.

### D-114 SharePoint storage is switched on Administration → SharePoint — Accepted (2026-10-23)
Administrators enter the site and library, test them, and turn SharePoint
storage on and off on **Administration → SharePoint** (the Settings panel,
`manage_settings`). The test is read-only: it finds the site, the library and
its list, counts missing PDMS columns, looks for the three area folders, and
reads the library's sharing, but creates nothing; PDMS still creates columns
and folders when it stores the first file. Three rules keep files readable:
storage turns on only while no document file is stored on this server or in
S3 (they would be unreadable, D-102) and only when the saved site passes the
test; the site and library can't change while SharePoint holds PDMS files,
which PDMS tracks by item ID; and turning storage off needs the site address
typed as confirmation, because files already in SharePoint can't be opened
until it is on again. `[storage] backend` in the configuration file still
wins (D-111), so the installer no longer writes `backend = "local"`, the
default.

### D-115 Microsoft sign-in is set up in Administration, and can't lock everyone out — Accepted (2026-10-23)
**Administration → Microsoft 365 → Sign-in** (`manage_settings`) holds the
sign-in settings: on or off, the tenant (the Entra issuer is built from it;
another provider's issuer URL is accepted as it is), the client ID, the
client secret (write-only, encrypted), the button label and account creation
on first sign-in. Saving rebuilds the sign-in provider in this process at
once, and in the other through the settings notice (D-112). If discovery
fails, Microsoft sign-in is unavailable, the page shows why, PDMS retries
every 5 minutes, and local sign-in keeps working; this replaces "disabled
until the next restart" (D-021). The test reads the discovery document and
checks the authorization request PDMS would build; it can't prove the secret
or the redirect URI, which only Microsoft checks, so the page offers **Try
sign-in** in a new tab. Lockout rule: with break-glass local sign-in on,
every change can be saved; with it off (`[auth.local] enabled = false`, a
file-only setting), Microsoft sign-in can't be turned off on the page and a
change is saved only when it passes the test.

### D-116 The installer after S1, and moving existing settings — Accepted (2026-10-23)
The installer keeps what PDMS can't start without or can't change from
inside its container: the address and proxy, the database port, the name and
time zone, the source URL, the first administrator, and which optional
containers run (OCR, the virus scanner; Compose profiles recorded in `.env`
as `PDMS_INCLUDE_OCR` and `PDMS_INCLUDE_CLAMAV`). Sign-in, email and
SharePoint left it: `--configure sso|smtp|sharepoint` say which admin page
has the setting. On an upgrade that has the blocks earlier installers wrote
(`sso`, `smtp`, `sharepoint`, `ocr`, `storage`, `clamav`), it offers once
to move them into Administration: `pdms settings import` copies what those
blocks set (not the defaults they wrote out, not values already stored, and
not hand-written settings outside them) into the database, secrets encrypted,
and makes the certificate file the active certificate (activating it if
certificates were already made on the page, so the one in use stays in use).
Only after that succeeds does the installer remove the blocks, keep a copy of
the file, empty the moved secret files and restart. Declined, it asks no more
(`--configure move-settings` moves them later). `pdms settings set`,
`set-secret` and `test` are the fallback when the pages can't be reached.

### D-117 Entra sync limited to one group — Accepted (built 2026-10-29)
Amends D-086 and D-096. The owner's answers of 2026-10-28 and 2026-10-29
are in the GS [spec](superpowers/specs/2026-10-27-entra-sync-group-scope-design.md)
§9.

- **One group decides who has a PDMS account** (amends D-086).
  `[entra_sync] group_id`, set on **Administration → Entra ID sync** as
  **Only members of this group** or in the configuration file (which wins),
  names an Entra group by its object ID. When it is set, only the group's
  members have active accounts. Members of groups inside it count, so whole
  department groups can be added. Empty means the whole tenant, as before.
  The group can be made in Entra, be dynamic, or be an on-premises AD group
  once Entra Connect syncs it (the county's AD groups are not synced today).
  Reading it needs the Graph application permission `GroupMember.Read.All`,
  only while a group is set. **Test group** shows its name and size, saving
  nothing.
- **Each run reads the whole member list** (`transitiveMembers`) before it
  changes anything, because a group delta doesn't follow nested groups. A
  list that can't be read fails the run, and nothing changes. A member PDMS
  has no account for is read and created at the next run. People outside
  the group are departed every run, with the reason "is no longer in the
  PDMS group", under the D-096 safeguards: held back when the group lists no
  members or when more than max(5, 20%) of linked accounts would leave at
  once; the last administrator kept; an account with a local password
  departed and reported; the break-glass account never touched.
- **The first narrowing** holds back its departures; the page lists them
  with **Depart these N people** (`manage_directory`). It reads the group
  again and departs only the listed people still active and still outside
  it, each audited in its own transaction, never the administrator pressing
  it, and is refused while a run is in progress.
- **Back in the group** (amends D-096, under which the sync never reactivates
  anyone): the sync reactivates someone it departed for leaving the group
  once they are back in it with an enabled member account, at the next run
  or at their next Microsoft sign-in, whichever comes first. Their roles,
  grants and memberships come back as they were. The page lists them and
  marks those who hold roles; the audit reason is "is back in the PDMS
  group". A departure made by hand, or because Entra disabled or deleted the
  account, is never undone. Clearing the group reactivates nobody. The sync
  records why it departed someone in `users.departed_from_group` (migration
  0016); any other status change clears it.
- **Sign-in:** while the sync is on and a group is set, a first sign-in asks
  Entra whether the person is in the group (`checkMemberGroups`). Someone
  outside it is refused, without an account, on a page that says they are
  not in the PDMS group and to ask IT; the audit reason is `not_in_group`.
  With the sync off, the group is not checked, and sign-in follows
  `auth.oidc.auto_provision` as before. **Assignment required** on the
  sign-in app is still recommended, because it stops sign-in at Microsoft at
  once, before the next run.
- **The mapping race is closed** (one of the two timing cases D-096 left
  open): applying a person takes a shared advisory lock and a Department
  mapping change an exclusive one, and the mapping is read inside the apply
  transaction. A mapping edit made while the sync is off stays as it is.

### D-118 Files are shown in the browser only for a short list of types — Accepted (phase 1 built 2026-10-30)
Amends D-027. Design: the DV
[spec](superpowers/specs/2026-10-30-document-viewing-design.md) §2, §3 and §6.

- **Five types may be shown:** `application/pdf`, `image/png`,
  `image/jpeg`, `image/gif` and `image/webp`. The choice goes by the stored,
  sniffed type (D-027), never by the file name or what the uploader claimed.
  Everything else stays download-only: Word, Excel, PowerPoint, OpenDocument,
  RTF, TIFF, plain text, CSV and email.
- **Why a list:** a file shown in the browser opens as a page on PDMS's own
  address, so a file that could act as a page (HTML or SVG) could act as PDMS
  for whoever opened it. PDMS refuses files that start as HTML, and other
  text with markup in it, an SVG for example, is stored as plain text, so
  nothing like that is stored today. The list keeps viewing safe even if the
  accepted types change: a type is shown only if it is on the list.
- **The view address** is `GET /documents/{id}/versions/{n}/view`. The
  **View** button in the document header and the **View** links in the Files
  table open it in a new tab. It calls the same core function as a download,
  so the `View` permission, visibility (D-085, D-090) and the stop on a
  SharePoint file changed outside PDMS (D-105) apply unchanged. A type not
  on the list is sent exactly as a download is (`attachment`).
- **A viewable file is sent inline** with `nosniff`, `Cache-Control:
  private, no-store`, and the real file name for the viewer's own Save. No
  `Accept-Ranges` header is sent, so the viewer fetches the file once and
  one view is one audit event.
- **The view policy** replaces the page policy on this answer only:
  `default-src 'none'; style-src 'unsafe-inline'; base-uri 'none';
  form-action 'none'; frame-ancestors 'none'; sandbox`.
  - `default-src 'none'`: the response may load nothing else.
  - `sandbox`, with no allowances: the browser treats the tab as coming from
    nowhere, not from PDMS, so even if something in a file ran, it couldn't
    use the person's PDMS sign-in. Both browser viewers work under it.
  - `style-src 'unsafe-inline'`: Chrome's and Edge's own image page centres
    the image and fits a large scan to the window with inline styles, which
    the page policy blocks. This is safe here because the answer is an image
    or a PDF, never HTML, and `nosniff` stops the browser reading it as HTML.
  - If the browser check finds the viewer's Save, Print or a link in a PDF
    blocked, only the matching allowance is added (`allow-downloads`,
    `allow-modals`, `allow-popups` with `allow-popups-to-escape-sandbox`).
    Never `allow-scripts`, `allow-same-origin`, `allow-forms` or
    `allow-top-navigation`.
- **Pages keep the page policy.** Only the view handler can ask for the view
  policy, by marking its response; `middleware::security_headers` then sends
  it in place of the page policy. Both policies are in `middleware.rs`, and a
  test checks that pages keep theirs. The view answer also keeps the other
  headers (`X-Frame-Options`, `Referrer-Policy`,
  `Cross-Origin-Opener-Policy`, HSTS).
- **Known risk:** the view answer carries both `Cross-Origin-Opener-Policy:
  same-origin` and a `sandbox`, and the HTML Standard says a top-level page
  with both should not load. Current Chromium and Firefox load it anyway
  (checked). If a browser on a county PC doesn't, the fix is to leave
  `Cross-Origin-Opener-Policy` off the view answer only. The browser check
  (plan Task 5) looks for this.
- **A new tab, not a frame** on the document page, because a frame would
  need the page policy and `X-Frame-Options` loosened for PDMS's own pages.
- **Audit:** every file sent writes `document.downloaded` with the existing
  `version`, `sha256` and `file_name`, plus `disposition`: `inline` for a
  view, `attachment` for a download. PDMS can't see what the viewer's own
  Save or Print does afterwards, which is why a view is audited like a
  download. `document.viewed` still means "opened the document page"
  (D-034).
- **Phase 2** (Word, Excel, PowerPoint, OpenDocument, RTF and TIFF shown as
  PDF through SharePoint's conversion) waits for the owner's answer (ROADMAP
  §0.7). Phase 1 is done once the browser check on a county PC passes (plan
  Task 5).

## Open questions (configurable defaults; not blocking)

| Question | Default chosen |
|---|---|
| Reminder offsets / overdue repeat | Seeded org defaults: 90, 30, 7 days before, on the due date, then every 7 days overdue (none for expiration), to the owner and the department head. Change under Administration → Notification rules; override per type or per document |
| Annual vs. 5-year review by type | Policy: annual. Contract: renewal-date driven, no fixed review. Partnership Agreement: 5-year. Other: none |
| Types requiring legal review / signatures | Contract and Partnership Agreement require the `legal_counsel` step and signatory tracking; Policy requires approval only; all configurable per type |
| Who assigns primary owners | Anyone with `manage_permissions` on the document (administrators have it on non-confidential documents); the new owner must be an active user. See D-033 |
| Seeded positions/appointments | None in migrations; the demo dataset includes sample boards (Advisory Committee, Program Advisory Board, Safety Coordinator) for testing only |
| Self-approval | Off (`[workflow] allow_self_approval = false`) |
| Reappointment decision lead time | 90 days before term end, per position |
| Retention periods | Placeholder policies only (D-036); confirm with your records retention schedule |
| SharePoint/OneDrive storage | Decided and built: SharePoint holds the files (D-081 to D-084, D-089, D-102 to D-110). Setup: [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md); the live test on a real site is pending (ROADMAP §0.3) |
| ECM system vs. records-request portal | Neither integrated. Either can use the REST API (read documents, files, history), receive webhooks, or get a dedicated `IntegrationProvider`; the records request package format is product-neutral |
| Audit forwarding target | Off; file (JSON lines) or syslog when configured |
| API token lifetime | Up to 365 days; 90 suggested |
| Audit retention | Audit rows are never purged by the app. A future owner-role archival procedure would export a prefix, record a checkpoint, and continue the chain from that hash |
