# SharePoint storage and Entra user sync — design

| | |
|---|---|
| Date | 2026-10-14 |
| Status | Draft for review |
| Covers | Sub-project 1 (SharePoint document storage) and sub-project 2 (Entra user sync) |
| Codebase | `pdms` (the product is referred to as PADM below) |

## 1. Background

PADM is the county-wide system of record for official documents that carry
review, renewal, or expiration dates (policies, contracts, interlocal
agreements, grant agreements, and similar). It is used by everyone in the
county. It reminds people before those dates and runs review and approval.
It also tracks board appointments, enforces per-department access, and keeps
a tamper-evident audit trail.

Today PADM stores files in its own blob store (local disk or S3) and creates
users when they first sign in.

The county wants:

- **Files to live in SharePoint.** People can browse them there read-only,
  organised by department.
- **PADM to stay the interactive application** everyone uses: upload, search,
  reminders, approvals, calendar.
- **Users to come from Entra ID**, signing in to PADM with SSO, with their
  department taken from the Department field on their Entra account.

## 2. Decisions

These were agreed during design discussion on 2026-10-14.

| # | Decision |
|---|---|
| D1 | **SharePoint holds the files; PADM holds everything else.** Metadata, workflow, reminders, appointments, permissions, and audit stay in PADM's database. SharePoint is the file store plus a read-only browsing mirror. ("Option A") |
| D2 | PADM connects to SharePoint **as itself (app-only)** through Microsoft Graph. It uses a certificate credential and the `Sites.Selected` permission, scoped to **one dedicated site**. |
| D3 | **Users never receive SharePoint links from PADM.** Downloads are streamed through PADM, which checks permissions and audits each one. |
| D4 | **Search stays in PADM.** It runs over document details plus text extracted from each file at upload, filtered by PADM permissions. It does not use Microsoft Search. |
| D5 | **SharePoint layout:** one site, one library, top-level `Departments/`, `County-wide/`, `Restricted/`. Each has a folder per department, then a folder per document, then one file per version. Files are never overwritten. |
| D6 | PADM **mirrors key metadata into SharePoint columns** on every file. A default view shows current versions only. |
| D7 | **Three visibility levels** per document, chosen by the owner at upload and changeable later: **Department** (default), **County-wide** (every active county account can view), and **Restricted** (explicit grants only). |
| D8 | **SharePoint permissions are set only on top-level and department folders**, never per file. Changing visibility moves the document's folder. |
| D9 | One or two named people may hold edit rights on the site, but editing there "should never happen". PADM **detects any change made outside PADM** and flags it. |
| D10 | **Departments come from the Entra user `department` attribute**, through a mapping list in PADM. Entra groups are not used, because AD groups are not synced to Entra. |
| D11 | **Roles** (admin, legal counsel, records officer, auditor, appointments clerk, department head) are **assigned in PADM**, not derived from Entra. |
| D12 | **All enabled Entra member accounts may sign in.** Guest (B2B) accounts are excluded. SSO matches users by Entra object ID only. |
| D13 | The break-glass local administrator remains. |

## 3. Goals and non-goals

**Goals**

- G1: Every file uploaded through PADM is stored in the county's SharePoint
  site, in the D5 layout, with D6 columns.
- G2: Browsing SharePoint directly shows each person what PADM would show
  them, at folder granularity (D7, D8).
- G3: People and their departments are kept in step with Entra automatically.
  Departed staff lose access within a day.
- G4: Searching in PADM covers the contents of the files stored in SharePoint.
- G5: Changes made directly in SharePoint are detected and surfaced to admins
  within one polling interval.

**Non-goals (separate sub-projects or later)**

- UI redesign (sub-project 5).
- Bulk import of existing documents (sub-project 4).
- ILA rename, admin-curated tags, and removal of the Outlook calendar sync
  (sub-project 3; small, independent).
- Public-records (FOIA/PIA) request features beyond what exists.
- Editing documents in Office or SharePoint (co-authoring), Microsoft Search,
  and Purview retention labels managed by PADM.
- Moving the test server's existing local-disk files into SharePoint (the
  test install is reset instead; see §13).

## 4. Architecture

```
 Browser ──HTTPS──▶ PADM web (pdms serve) ──▶ PostgreSQL (metadata, permissions, audit, text index)
                         │   ▲
                 upload/ │   │ stream
                download ▼   │
                    Microsoft Graph ◀──── PADM worker (pdms worker)
                         │                 • mirror columns / move folders
                         ▼                 • department-folder access
                 SharePoint site           • change detection (delta)
                 "PADM Records"            • Entra user sync
                         ▲
 Read-only browsers ─────┘   (permissions set by PADM on top-level and department folders)

 Entra ID ──OIDC sign-in──▶ PADM web          Entra ID ──Graph /users/delta──▶ PADM worker
```

**New and changed units:**

| Unit | Responsibility | Depends on |
|---|---|---|
| `pdms-core::graph` (new) | Graph HTTP client: certificate-based app token with cache, JSON calls, Retry-After and backoff handling, paging. Shared by everything below. | `reqwest`, config |
| `pdms-storage::sharepoint::SharePointStore` (new) | File I/O only: upload (upload session, `conflictBehavior=fail`), download stream, item metadata (size, `quickXorHash`). | `graph` |
| `pdms-core::sharepoint` (new) | Placement and mirroring. It computes each document's desired folder path, file names, and columns, then applies them. It also manages department-folder access and runs change detection. | `graph`, `SharePointStore`, documents, directory |
| `pdms-core::directory::entra_sync` (new) | Reads users from Graph (`/users/delta`), maps the Department value to a PADM department, and creates, updates, or disables users. | `graph`, directory |
| `pdms-authz` (changed) | Adds the County-wide visibility level to `decide` and to the SQL predicate. | — |
| `pdms-core::documents` (changed) | Uploads and downloads go through the SharePoint placement. Visibility changes trigger folder moves. | `sharepoint` |
| `pdms-core::auth::oidc` (changed) | Matches users by Entra object ID only. A first sign-in by a not-yet-synced user creates them and looks up their department immediately. | `entra_sync` |

The existing `BlobStore` trait is keyed by content hash. SharePoint needs a
per-document location, so placement lives in `pdms-core::sharepoint`, and the
store is called with an explicit path. The local and S3 stores remain for
development and automated tests.

## 5. SharePoint site and layout

**Site.** A communication site, e.g. `PADM Records`. Use a separate
`PADM Records – Test` site for the test server. It uses the default
`Documents` library.

```
Documents
├── Departments/
│   ├── Road and Bridge/
│   │   └── CON-2026-0012 – Fuel Supply Agreement/
│   │       ├── CON-2026-0012 v1 – Original.pdf
│   │       ├── CON-2026-0012 v2 – Amendment.pdf
│   │       └── CON-2026-0012 v3 – Signed copy.pdf
│   └── County Auditor/
├── County-wide/
│   └── <Department>/<document folder>/...
└── Restricted/
    └── <Department>/<document folder>/...
```

**Naming**

- **Department folder:** the PADM department name.
- **Document folder:** `{document ID} – {title}`.
- **File:** `{document ID} v{n} – {version label}.{ext}`. The version label is
  "Original", "Revision", "Amendment", or "Signed copy" (the version kind),
  or the uploaded file name when the kind adds nothing.
- **Sanitising:** characters SharePoint forbids (`" * : < > ? / \ |`) are
  replaced, leading and trailing spaces and dots are trimmed, and names are
  shortened to keep the full path within SharePoint's limits. The document ID
  always stays intact, so names remain unique.

**Columns** (site columns on the library; PADM is the only writer):

| Column | Type |
|---|---|
| Document ID | Single line of text |
| Title | Single line of text |
| Document Type | Single line of text |
| Tags | Multiple lines of text (semicolon-separated) |
| Status | Single line of text |
| Visibility | Choice (Department, County-wide, Restricted) |
| Department | Single line of text |
| Owner | Single line of text (display name) |
| Effective Date / Review Date / Renewal Date / Expiration Date | Date |
| Version | Number |
| Current | Yes/No |
| Open in PADM | Hyperlink |

**Views.** The default view is "Current documents" (Current = Yes), sorted by
Review Date. A second view, "All versions", has no filter.

**Library versioning** stays on (major versions). PADM never overwrites a
file, so SharePoint versions accumulate only if someone edits a file outside
PADM. Those versions are what an admin restores from (§7.6).

## 6. Data model (migration `0008_sharepoint_entra.sql`)

**Document visibility.** Add `documents.visibility` as an enum
`('department','county_wide','restricted')` and remove `is_confidential` and
`inherit_acl` in the same migration.

- **Mapping from existing data:** `restricted` where
  `is_confidential OR NOT inherit_acl`; otherwise `department`.
- **Why one enum:** in the engine today both booleans have exactly the same
  effect (department and global grants stop applying). A third boolean would
  allow contradictory combinations. Code that reads the two booleans is
  updated: about 20 files, mainly authz, search, calendar, exports, planner
  and the document routes.

**Authorization rule changes** (in `pdms-authz`, mirrored in the SQL
predicate):

- `department`: unchanged from today's default behaviour.
- `county_wide`: as `department`, plus **view** (not edit or any other
  permission) for every active principal.
- `restricted`: only document-scope grants and the owner rule apply. This is
  today's confidential behaviour.

**New tables**

```sql
-- Where each file version lives in SharePoint.
CREATE TABLE sharepoint_files (
    document_version_id uuid PRIMARY KEY REFERENCES document_versions (id),
    document_id         uuid NOT NULL REFERENCES documents (id),
    drive_id            text NOT NULL,
    item_id             text NOT NULL UNIQUE,
    quick_xor_hash      text NOT NULL,          -- recorded at upload, compared by change detection
    size                bigint NOT NULL,
    last_known_path     text NOT NULL,
    state               text NOT NULL DEFAULT 'ok'
                        CHECK (state IN ('ok','changed_outside','missing')),
    uploaded_at         timestamptz NOT NULL DEFAULT now(),
    checked_at          timestamptz
);

-- Entra Department values → PADM department (many values may map to one department).
CREATE TABLE department_entra_values (
    value_normalized text PRIMARY KEY,           -- lower-case, trimmed, '&' → 'and', collapsed spaces
    value_display    text NOT NULL,
    department_id    uuid NOT NULL REFERENCES departments (id)
);

-- Per-person read grants PADM has made on department folders.
CREATE TABLE sharepoint_folder_access (
    department_id uuid NOT NULL REFERENCES departments (id),
    user_id       uuid NOT NULL REFERENCES users (id),
    permission_id text NOT NULL,                 -- Graph permission id, for removal
    granted_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (department_id, user_id)
);

-- Change-detection and sync cursors.
CREATE TABLE sync_cursors (
    name       text PRIMARY KEY,                 -- 'sharepoint.delta', 'entra.users'
    delta_link text,
    last_run   timestamptz,
    last_error text
);
```

**Changes to `users`**

- **Entra object ID:** the existing `(oidc_issuer, oidc_subject)` pair already
  holds it, because the configured subject claim is `oid`. The sync writes the
  same pair for users it creates, so there is no new identity column.
- **New columns:**
  - `entra_department` (raw attribute value);
  - `entra_synced_at`;
  - `source` (`'entra'` or `'local'`).

Department memberships created by the sync use the existing
`membership_source = 'idp'`, so manual memberships are never touched.
**When `entra_sync` is enabled, sign-in-time IdP group reconciliation is
switched off** (`reconcile_idp_memberships` is not called at login). The sync
is the only writer of `idp` memberships, so a token without group claims can
never remove a department.

**Grants** for `pdms_app`: SELECT, INSERT, UPDATE on the new tables (DELETE on
`sharepoint_folder_access` only). `sharepoint_files` is never deleted;
document versions are immutable.

## 7. Flows

### 7.1 Upload (new document or new version)

1. Existing pipeline: spool, sniff, virus scan, and text extraction source.
2. For a new document, allocate its document ID first (sequence), so the
   folder name is known before the upload.
3. Ensure the document folder exists under the area and department for the
   document's visibility.
4. Upload through a Graph upload session to the version's file name, with
   `@microsoft.graph.conflictBehavior=fail`. Record the returned `item_id`,
   `quickXorHash`, and size. Compute the quickXorHash locally too and compare.
5. Commit the `document_versions` row and its `sharepoint_files` row in one
   transaction with the audit entry, as today.
6. Enqueue `sharepoint.mirror_document` (§7.3).

**If the upload fails,** the user sees "File storage is unavailable; nothing
was saved." **If the database commit fails after a successful upload,** the
file is an orphan. Change detection (§7.6) lists files the PADM app created
that no row references, and an admin can remove them.

### 7.2 Download

1. `documents::access::authorize(view)`, unchanged.
2. Look up `sharepoint_files` for the version.
3. `GET /drives/{drive}/items/{item}/content`. PADM follows the redirect
   server-side and streams the bytes to the user with the existing headers
   (attachment, sniffed content type, `nosniff`). It computes SHA-256 while
   streaming. A mismatch with the recorded hash is logged as an integrity
   alarm, and the document is flagged for admin review.
4. The existing download audit entry is written.

### 7.3 Mirroring metadata and placement

Any committed change to a document enqueues `sharepoint.mirror_document`
(deduplicated per document). Changes include details, status, tags,
visibility, department, title, owner, and versions. The job:

1. Computes the desired folder path and the file names and columns of every
   file (pure function, unit-tested).
2. Moves or renames the document folder if its path differs. All moves are
   within one drive, using `PATCH parentReference/name`.
3. Updates each file's columns (`PATCH .../listItem/fields`) and sets
   `Current` on the newest file version only.

The job is idempotent and convergent. Re-running it after a crash or a manual
change restores the desired state.

**Visibility changes that tighten access** (to Restricted, or County-wide to
Department) **move the folder synchronously** as part of the request. If the
move fails, the change is refused. Loosening changes are left to the
background job. This keeps SharePoint from ever showing a document more
widely than PADM allows, beyond a failed request.

### 7.4 Department folder access

The `sharepoint.reconcile_access` job runs after each Entra sync, after any
department membership change, and nightly. For each `Departments/<Dept>/`
folder:

- **Desired readers:** the active members of that PADM department.
- **Add** missing readers with Graph `invite` (`roles: ["read"]`,
  `requireSignIn: true`, `sendInvitation: false`) and record the permission
  ID.
- **Remove** readers PADM granted who are no longer desired.
- **Never touch** permissions PADM did not create. These are reported if
  present.

`County-wide/` carries one grant to *Everyone except external users*, made by
an admin at setup (Appendix A). `Restricted/` and `Departments/` inherit
owners-only access from the site.

### 7.5 Entra user sync

The `directory.entra_sync` job runs nightly (default 02:30, before reconcile),
can be started from an admin "Sync now" button, and from the CLI with
`pdms sync-directory`.

- It calls `GET /users/delta` (`$select=id,displayName,mail,userPrincipalName,department,accountEnabled,userType`)
  with the stored delta link, and skips `userType ne 'Member'`.
- **New enabled user:** create them (`source='entra'`, `entra_object_id`,
  email = `mail` or the UPN).
- **Changed user:** update the name and email. If the normalised Department
  value maps to a department, set that single `idp` membership; otherwise
  remove any `idp` membership and list the user as unmapped.
- **Disabled or removed user:** set the status to `departed`, which revokes
  sessions, and audit it.
- **Not touched:** local accounts and manual memberships.
- **When it finishes:** write one `directory.synced` audit entry (counts), then
  enqueue `sharepoint.reconcile_access`.

**Sign-in.** OIDC matches `(issuer, oid)` only, and email-based account linking
is removed. A first sign-in by someone the sync has not yet seen creates the
user and fetches their Department value with one Graph call, so access is
correct immediately.

**Admin screens**

- A Department mapping list (value → department) with unmapped values and
  their user counts.
- A sync status panel (last run, counts, errors).

### 7.6 Change detection

The `sharepoint.delta` job runs every 5 minutes. It calls
`GET /drives/{drive}/root/delta` with the stored delta link.

- Items last modified by the PADM app are ignored.
- **A tracked file was modified** (hash or size differs): set `state =
  'changed_outside'`, flag the document, audit `sharepoint.changed_outside`
  with who changed it in SharePoint, and notify admins in-app.
- **A tracked file was deleted:** set `state = 'missing'` and handle it the
  same way.
- **A file appears that PADM didn't create, or that no row references:** list
  it under "Unexpected files".
- **A folder was moved or renamed by someone:** enqueue the mirror job, which
  puts it back.
- **Expired delta token** (`410 resyncRequired`): full resync.

**Admin resolution** for a flagged document:

- **Restore original:** restore the previous SharePoint version through Graph.
  For a deleted file, restore it from the site recycle bin, after which PADM
  re-verifies.
- **Accept as new version:** download the changed file and run it through the
  normal upload pipeline as a new PADM version, with scan, type check, text
  extraction, and audit.

### 7.7 Integrity verification

`pdms verify-integrity` checks SharePoint files by comparing the recorded size
and quickXorHash with Graph item metadata, with no download needed. With
`--rehash`, it downloads each file and compares SHA-256.

## 8. Search

Search keeps working the same way. Text is extracted from the bytes PADM
already holds at upload, and the full-text index and permission filter are
unchanged. When an admin accepts a change as a new version, that version is
extracted and indexed like any other.

## 9. Configuration

```toml
[storage]
backend = "sharepoint"            # "local" and "s3" remain for development and tests

[sharepoint]
tenant_id = "…"
client_id = "…"                   # the "PADM Service" app registration
certificate_file = "/run/secrets/sharepoint_cert"   # PEM: certificate + private key
site_url = "https://<tenant>.sharepoint.com/sites/PADMRecords"
library = "Documents"
delta_interval_minutes = 5

[entra_sync]
enabled = true
run_at = "02:30"                  # local time
attribute = "department"
include_guests = false
```

`install.sh --configure sharepoint` asks for these values and stores the
certificate as a Docker secret.

## 10. Error handling

- **Graph throttling** (`429`, `503`): honour `Retry-After`, otherwise use
  exponential backoff with jitter (via the job queue for background work).
- **Upload:** synchronous. A failure means nothing is saved, and the user is
  told to retry.
- **Download:** an error page reads "File storage is unavailable right now."
  The failure is logged.
- **Background jobs** (mirror, reconcile, delta, sync) are idempotent and
  retried. Repeated failures appear on an admin "SharePoint and directory
  sync" status panel, and the job ends as `dead` in the job queue.
- **Token or certificate failure:** logged, shown on the status panel, and
  raised once per hour as an admin in-app notice.
- **Job leases** must exceed the longest upload or download time. They are
  configured separately for SharePoint jobs.

## 11. Security

- **Least privilege:** `Sites.Selected` (one site, full control, which is
  needed to manage folder permissions) and `User.Read.All` (read-only).
  There are no other Graph permissions and no tenant-wide write.
- **Credential:** a certificate, not a client secret. The private key is
  mounted as a Docker secret. Rotation means adding the new certificate to the
  app registration, swapping the secret, restarting, then removing the old
  certificate.
- **Site hardening (Appendix A):**
  - only one or two named site owners;
  - no Members or Visitors;
  - external sharing off;
  - only owners can share.
- **PADM is authoritative.** SharePoint permissions are a coarse mirror that
  PADM converges to. Tightening changes are applied synchronously (§7.3).
- **No SharePoint links reach users.** Every download is authorised and
  audited by PADM.
- **Guests are excluded** from sync and sign-in, and identities are bound by
  object ID only.
- **New audit actions:**
  - `sharepoint.changed_outside`;
  - `sharepoint.access_granted`, `sharepoint.access_revoked`;
  - `sharepoint.change_resolved`;
  - `directory.synced`, `directory.user_created`, `directory.user_departed`;
  - `directory.department_mapping_changed`.
- **Retention:** SharePoint's recycle bin keeps deleted items for 93 days. A
  Purview retention policy on the site is recommended, so files cannot be
  permanently deleted by the site owners (Appendix A, step 6). PADM's
  database backups remain required.

## 12. Testing

- **Unit:**
  - path and file-name sanitising and truncation;
  - desired-state computation (folder, names, columns, `Current`);
  - Department value normalisation and mapping;
  - visibility rules in `pdms-authz`. County-wide grants view only, never
    edit. Restricted ignores department and global grants.
- **Property:** extend the SQL-predicate versus engine equivalence test to
  generate all three visibility levels.
- **Integration (mock Graph server,** extending the one used by the existing
  Graph tests):
  - upload session and conflict-fail;
  - download redirect;
  - column patch, folder move and rename;
  - invite and permission delete;
  - delta with an outside edit, a delete, an unexpected file, and
    `410 resyncRequired`;
  - `429` with `Retry-After`;
  - `/users/delta` with a new user, a department change, an unmapped value,
    a disabled user, a removed user, and an excluded guest.
- **Acceptance (automated where possible):**
  - A Department document is visible to its department and not to another
    one, in both PADM and SharePoint folder permissions.
  - A County-wide document is visible to every active account.
  - Changing a document to Restricted moves it before the request returns.
  - An outside edit is flagged within one delta interval.
  - A disabled Entra user loses PADM sessions and SharePoint folder access
    after the sync.
- **Live (opt-in, ignored by default):** run against `PADM Records – Test`.

## 13. Rollout

1. Develop against the mock Graph server and local storage. The SharePoint
   backend is off by default.
2. The county creates the test site (Appendix A, steps 1–6) and the app
   registration (step 7).
3. Reset the test server (`docker compose down -v`, fresh install with
   `backend = "sharepoint"`), seed demo data, and run the live tests and the
   ROADMAP testing plan.
4. Production: same steps against `PADM Records`.

There is no production data yet, so no storage migration tool is built.

## 14. Open questions

1. Who are the one or two SharePoint site owners?
2. What are the site URLs (test and production)?
3. Should accounts without a mailbox (no `mail`, UPN only) be synced? Proposed:
   yes; they can sign in, and notifications go in-app only.
4. Should departed users' department-folder grants be removed immediately at
   sync? Proposed: yes, since reconcile runs right after the sync.

## Appendix A: county setup steps

Steps 1–6 can be done now. Step 7 is needed only when PADM is ready to
connect.

1. **Create a communication site**, `PADM Records – Test` (and later
   `PADM Records`).
2. **Permissions:** site owners are one or two named admins. Leave the
   Members and Visitors groups **empty**.
3. **Sharing:** set external sharing to "Only people in your organization"
   or off, and restrict sharing to site owners.
4. **Library `Documents`:** versioning on, major versions; check-out not
   required.
5. **Create top-level folders** `Departments`, `County-wide`, `Restricted`.
   On `County-wide`, stop inheriting permissions and grant
   **Everyone except external users: Read**. Leave the other two inheriting
   (owners only). PADM creates the department and document folders itself.
6. **Optional, recommended:** a Purview retention policy on the site, so
   files can't be permanently deleted.
7. **When PADM is ready:**
   1. Register an app, `PADM Service`, with Microsoft Graph *application*
      permissions `Sites.Selected` and `User.Read.All`, and grant admin
      consent.
   2. Upload the public certificate PADM generates.
   3. Grant the app **full control on this site only.** This is done with a
      Graph `POST /sites/{site-id}/permissions` call or PnP PowerShell; the
      exact commands are in the setup guide produced during implementation.
8. **Data hygiene (can start now):** make sure every staff account has a
   correct **Department** value, and list the distinct values in use for the
   PADM mapping list.
