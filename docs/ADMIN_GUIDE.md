# Administrator Guide

This guide covers what is available as of Phase 2: installation, sign-in, users
and access, documents, reminders and the calendar, the audit log, and
operations. It grows with each phase.

## 1. Install

On a Linux host with Docker Engine, the Docker Compose plugin, and `openssl`:

```sh
./install.sh
```

All questions come first; then the build and start run unattended (the first
build takes several minutes).

| Step | What it asks and does |
|---|---|
| How people reach PDMS | **1) Bundled Caddy:** automatic HTTPS. A domain gets a Let's Encrypt certificate (ports 80/443 reachable from the internet); without a domain, Caddy uses an internal certificate for the server's IP (browsers warn until it is trusted). **2) Your own reverse proxy:** enter the HTTPS address users open and the proxy's IP; PDMS then listens on all interfaces at its port (firewall it to the proxy). **3) Local testing:** plain HTTP on `localhost`. The host port defaults to 8080 or the next free port; Caddy needs 80 and 443 free. |
| Database for host tools | Optional: publishes PostgreSQL on `127.0.0.1` only (5432 or the next free port) through `docker-compose.override.yml`. Not needed for normal operation or backups. |
| Name and time zone | Shown in titles, emails, and calendars; the time zone (detected from the host) drives reminder scheduling. |
| Source code URL | Where users can get this installation's source code, as the AGPL-3.0 requires for software used over a network (defaults to the git remote). Linked from every page as **Source code**. |
| Secrets | Generated once and never overwritten: PostgreSQL superuser, owner, and app passwords (256-bit, hex), and the 512-bit master key. One file each in `secrets/` (directory `0700`), mounted as Docker secrets, never written to `.env` or the configuration. |
| Microsoft Entra ID | Optional (needs HTTPS): tenant ID, client ID, client secret (hidden), and the redirect URI to register: `<public URL>/auth/oidc/callback`. If Microsoft cannot be reached, local sign-in keeps working and the problem is logged. |
| Email | Optional: SMTP host, security (STARTTLS 587, TLS 465, or none for a trusted relay), port, username, password (hidden), from address. |
| Outlook calendar | Optional: shared mailbox; can reuse the sign-in app registration (add `Calendars.ReadWrite`). See section 10. |
| OCR, virus scanning | Optional extra containers (each about 1 GB). |
| First administrator | Email, username (default `sysadmin`; preset `PDMS_INSTALL_ADMIN_USERNAME`; checked against the username rules below and shown in the summary), and name. The password is generated and shown once at the end, with the **authenticator (TOTP) secret**: add it to an authenticator app right away. |

Files it writes:

| File | Contents |
|---|---|
| `.env` (`0600`) | Deployment settings for Docker Compose: proxy mode, host port and bind address, public URL, database port, enabled profiles. Docker Compose reads it automatically. |
| `config/pdms.toml` | Application settings. Blocks between `# >>> install.sh:<name>` markers are managed by the installer; edit anything else freely. |
| `config/Caddyfile` | With the bundled proxy. |
| `secrets/` | Passwords and keys. Back them up securely (see BACKUP.md). |

**Later changes.** `./install.sh --configure <part>` redoes one part (comma-separated
for several): `network`, `database-port`, `sso`, `smtp`, `outlook`, `ocr`,
`clamav`, or `admin` (resets the break-glass administrator). The services are
restarted only if a setting actually changed. Running `./install.sh` with no
options asks only about parts that are not set up yet, so it is also the
upgrade step. Settings written by hand (or by an earlier installer) are kept;
only `--configure` replaces them.

**Unattended installs:** `./install.sh --non-interactive` with
`PDMS_INSTALL_*` variables (`./install.sh --help` lists them).

### Reverse proxy and TLS

With your own proxy, terminate HTTPS there, forward to `http://<this host>:<port>`,
pass `X-Forwarded-For`, and allow request bodies of at least 110 MB. Samples:
`deploy/Caddyfile`, `deploy/nginx.conf` (both block `/metrics` from the outside).
PDMS believes `X-Forwarded-For` only from addresses in `server.trusted_proxies`;
the installer adds your proxy's IP, `127.0.0.1`, and the Docker range
`172.16.0.0/12`.

## 2. Sign-in

### Microsoft Entra ID (recommended for everyone)

1. In the Entra admin center, go to **App registrations → New registration**.
   - Name: your system name. Supported account types: this organization only.
   - Redirect URI (Web): `https://<your-public-url>/auth/oidc/callback`
2. **Certificates & secrets → New client secret.** Copy the value into
   `secrets/oidc_client_secret` (no trailing newline needed).
3. **Token configuration → Add groups claim → Security groups**, emitted as
   **Group ID** in the ID token. This drives automatic department and role
   assignment.
4. **API permissions:** `openid`, `profile`, `email` (Microsoft Graph,
   delegated). Grant admin consent.
5. Optional: under **Enterprise applications → (your app) → Properties**, set
   *Assignment required* to Yes and assign the groups that may use the system.
6. Edit `config/pdms.toml`:

   ```toml
   [auth.oidc]
   enabled = true
   issuer_url = "https://login.microsoftonline.com/<tenant-id>/v2.0"
   client_id = "<application (client) ID>"
   display_name = "Microsoft"
   ```

7. `docker compose up -d app` to restart. The sign-in page now shows **Sign in
   with Microsoft**.

Users are identified by their Entra **object ID** (`oid`), so renaming an email
address does not create a duplicate account. On first sign-in, a user is
linked to a pre-created account with the same email, or created with no access
(`auth.oidc.auto_provision = false` makes pre-creation mandatory).

If Entra is unreachable when the app starts, single sign-on is disabled until
the next restart (the log says so) and break-glass sign-in keeps working.

**Users in more than 200 groups:** Entra then omits the groups claim. PDMS
leaves those users' synced memberships unchanged and logs a warning. Graph-based
group lookup arrives in Phase 4.

### Break-glass local administrators

Local accounts are for emergencies, such as an Entra outage or
misconfiguration. They sign in with a username or an email, and require a password (12+ characters), and an
authenticator code. Five failed attempts lock the account for 15 minutes, and
sign-in attempts are rate-limited per IP.

```sh
docker compose run --rm app create-admin --email it-admin@example.org --name "IT Admin" --username sysadmin
```

Running this again for an existing email resets the password and TOTP secret,
reactivates the account, and signs out its sessions. To turn local sign-in off
entirely after SSO works, set `[auth.local] enabled = false`. The sign-in
page then shows only the Microsoft button.

### Sign-in page and usernames for local accounts

The sign-in page asks for a username and password first, with **Sign in with
Microsoft** always shown below. If Microsoft sign-in is not configured,
clicking it returns to the sign-in page with "Microsoft sign-in isn't set up
yet. Contact your administrator."

A local account may have a **username**, which is optional:

- 2–64 characters of letters, digits, `.`, `_`, and `-`; never `@`;
- unique ignoring case (`SysAdmin` and `sysadmin` are the same);
- the sign-in **Username** field accepts either the username or the email.

Set or clear it:

- **Admin page:** Administration → Users → the user. Only accounts with local
  sign-in can have one. Removing an account's local sign-in also clears its
  username.
- **CLI:** `pdms create-admin --email <email> --name <name> --username <name>`
  for a new account, or `pdms set-username --email <email> --username <name>`
  for an existing one. In Docker, run them like the other commands:
  `docker compose run --rm app set-username --email <email> --username <name>`.
- **Installer:** `install.sh` asks "Administrator username" (default
  `sysadmin`; preset with `PDMS_INSTALL_ADMIN_USERNAME`), validates it, and
  prints it in the final summary.

A failed sign-in always shows one generic message, so it never reveals whether
an account exists: "Username or password is incorrect.", or "Username,
password, or authenticator code is incorrect." when authenticator codes are
required. Setting a username is audited as `user.username_set`.

## 3. Users, departments, and access

Everything here is under **Administration** (requires the Administrator role).
Every change is recorded in the audit log with before and after values.

### The access model in brief

- **Read-only by default.** Creating a department also grants its members
  *view* on that department's documents. A new user in a department can read
  that department's documents, plus County-wide documents, and nothing else.
- **Grants add access.** A grant gives a *subject* (a user, group, every member
  of a department, or holders of a role) a *permission* (`view`, `create`,
  `edit`, `delete`, `approve`, `manage_permissions`, `export`) on a *scope*
  (all documents, one department's documents, or one document). Every
  permission except `create` includes `view`. Grants can expire.
- **Restricted documents** ignore organization-wide and department grants;
  they need explicit per-document grants (set on the document page).
- **County-wide documents** may be viewed and downloaded by every active
  account; nothing else (no edit, approve, or export). Side effects: (1) their
  review, renewal, and expiration dates appear on every employee's
  dashboard, calendar, and "everything you can see" ICS/Outlook feed, because
  those views list everything a person can view (decided, D-090); (2) only
  the document's owner and PADM administrators can comment on them, though
  everyone who can see them can read the comments (decided, D-091);
  (3) webhook service accounts count as active accounts, so they receive
  events about County-wide documents.
- **Changes apply immediately.** Removing a grant, role, or membership affects
  the user's very next request. Disabling a user signs them out everywhere.

### Day-to-day tasks

| Task | Where |
|---|---|
| Add a department | Administration → Departments |
| Pre-create a user (they then sign in with SSO) | Administration → Users → Add a user |
| Put a user in a department | User page → Departments |
| Make someone a department head for one department | User page → Roles → *Department Head* for *that department* |
| Give a department access to another's documents | Department page → Access, or Administration → Access grants |
| Set a local account's username | Administration → Users → the user (see section 2) |
| Change the county name, accent color, or seal | Administration → Branding (see below) |
| Offboard someone | User page → Account status → *departed* (keeps their history and ownership) |
| Rename a role for your organization | Administration → Roles (e.g. *Legal Counsel* → "General Counsel's Office") |

### Branding

Administration → **Branding** (requires the settings permission) sets how
PADM presents your county. No config edit or restart is needed.

| Field | Rules |
|---|---|
| County name | 1–120 characters. Leave blank to use `[org] name` from the config file. |
| Accent color | Pick a color or type a hex code. White text on it must reach a 4.5:1 contrast ratio, or the page refuses it with a message (choose a darker color). Hover, link, and tint shades are derived automatically. |
| Seal | PNG or JPEG, at most 512 KB, 16–2048 pixels per side. The real file type is checked, and SVG is refused. It is stored in PADM's database, so backups include it. |

Two more actions: **Remove seal**, and **Reset to defaults** (asks you to
confirm on a separate page).

- **Public:** the seal and county name appear on the sign-in page, so anyone
  who can reach PADM sees them. Do not upload anything you would not put on
  your public website.
- **Where the name is used:** page titles, the sidebar, notification emails,
  calendar feeds, webhooks, and records-request packages.
- **When changes apply:** immediately in the process that saved them, and
  within 60 seconds in any other process (the web app and the worker are
  separate processes).
- **Audit entries:** `branding.updated`, `branding.seal_set`,
  `branding.seal_removed`, `branding.reset`.

### Preparing a demo for leadership

To show PADM to county leadership on a test or demo server, after it is
installed or updated:

1. **Give the administrator a username**, so they can sign in as `sysadmin`:
   `docker compose run --rm app set-username --email <admin email> --username sysadmin`
2. **Brand it.** Administration → Branding: set the county name, accent color,
   and seal. Placeholders are fine until the county decides.
3. **Let leadership see the whole county.** The county overview (top of
   **Reports**) and the dashboard show only what the viewer can see. For each
   leadership account:
   - **Documents.** Administration → Access grants → **Grant access**. Set
     *Applies to* to **All documents except Restricted ones**, *Grant to* to
     the account (under *Users*), *Permission* to `view`, and give a reason.
     Restricted documents stay hidden unless granted per document (on the
     document's page).
   - **Boards and commissions.** That summary appears only for accounts that
     can see positions. An account sees a position if it belongs to the
     position's department or manages it. To see every position, it needs an
     organization-wide **Auditor**, **Appointments Clerk** or
     **Administrator** role (these hold the "view all appointments"
     capability).
     - Auditor is read-only for boards, but it also opens the audit log and
       the access review reports.
     - Appointments Clerk can also change boards and appointments.
     - To assign one: Administration → Users → the account → **Assign role**,
       for *Whole organization*.

### Roles

| Role | What it allows |
|---|---|
| Administrator | Manage users, departments, groups, roles, grants, and IdP mappings; view and verify the audit log. Sees non-Restricted documents through a seeded, editable grant. |
| Auditor | View, export, and verify the audit log; access review and audit reports; read every position and every export package. |
| Records Officer | Builds records request packages. Can view and export non-Restricted documents org-wide (seeded grant); Restricted documents need a document grant. |
| Legal Counsel | Gives the legal review step in workflows. Can view non-Restricted documents org-wide (seeded grant). |
| Department Head | Department-scoped notification recipient and default approver: can approve the non-Restricted documents of the department it is assigned for (seeded grant; assign it org-wide for a presiding officer who approves everything). |
| Appointments Clerk | Manages boards, positions, requirement checklists, and appointments; receives appointment reminders. |

A role assigned *for a department* applies only to that department. System
capabilities (administration, auditing) come only from organization-wide role
assignments. The last active administrator cannot be removed, and
administrators cannot deactivate themselves.

### Mapping Entra groups

**Administration → Identity provider mappings** maps an Entra group object ID to
a department membership, a group membership, or a role, optionally limited to
one department. At every sign-in, PDMS adds and removes the memberships it
manages this way. Memberships an administrator added by hand are never changed.

## 4. Documents

### Working with documents

**Documents** in the top navigation lists everything you can see. You can
search by words in the title, ID, description, tags, or field values, and
filter by type, department, status, tags (any or all), a date range, or a
custom field. The list can be sorted and exported to CSV; the export includes
only documents you have `export` permission on, and is recorded in the audit
log. The export's former "Confidential" yes/no column is now a "Visibility"
column (Department / County-wide / Restricted).

**New document** appears for people who may create documents in at least one
department.
- **Basics:** choose the type (its additional fields appear), enter dates,
  choose the department and primary owner, and optionally attach the file.
- **No expiration:** leave the expiration date empty for documents that do not
  expire. They stay active.
- **Document ID:** each document gets a permanent ID such as `PAG-2026-000123`.

On a document's page:

| Action | Needs |
|---|---|
| View, download any version | `view` (department members have it by default) |
| Edit details, tags; upload a new version (revision, amendment, signed copy); restore an earlier file or earlier details | `edit` |
| Move between statuses | `edit`; approving out of In Review needs `approve` |
| Share with a person, group, department, or role; transfer ownership | `manage_permissions` |
| Change who can see it (Department, County-wide, Restricted) | the owner, or `manage_permissions` |
| Legal hold, retention override | Administrator or Records Officer |
| Delete | `delete`; blocked while on legal hold or within the retention period |
| Audit history | The owner, `manage_permissions`, or Administrator/Auditor |

Every change keeps the earlier versions: files are stored by content hash and
never overwritten. Deleted documents are hidden but kept. Administrators can
restore them under **Administration → Deleted documents**.

Renewal Due, Overdue, and Expired are set automatically from dates (see
chapter 5). People cannot set them by hand.

### Document types and custom fields

**Administration → Document types**:
- **Seeded types:** Policy, Contract, Partnership Agreement, and Other.
- **Each type sets:** its ID prefix, default review cycle, whether legal review
  or signatures are required (used by the Phase 3 workflow), and its retention
  policy.

**Administration → Custom fields** adds fields to every type or to one type:
text, long text, number, date, yes/no, single or multi select, user,
department, or link. You can set a label, help text, required, options, limits,
a pattern, and a default.

| Change | Effect on existing documents |
|---|---|
| Rename the label, edit help, options, or rules | Saved as a new version of the field. Existing values are untouched. Removed options still show, marked "(retired option)" |
| Add a field (even required) | Existing documents are unaffected until someone edits them; the editor is then asked for the value |
| Deprecate | Hidden from forms. Existing values stay visible, searchable, and in exports |

A field's key and type cannot change. To change a type, add a new field and
deprecate the old one.

### Tags

Anyone who can edit a document can add tags, and new names become free-form
tags. Turn this off with `[documents] free_form_tags = false`.
Administrators manage curated tags and colors under
**Administration → Tags**.

### Retention and legal holds

**Administration → Retention** (Administrators and Records Officers):
- **Policies:** each one keeps documents permanently or for N years after
  record creation, the effective date, the expiration date, or archiving.
- **Seeded policies:** placeholders only. Confirm them against your records
  retention schedule.
- **Per-document overrides:** a document can override its type's policy, and
  a legal hold blocks deletion regardless of policy.

### File storage and virus scanning

| Setting (`config/pdms.toml`) | Default | Notes |
|---|---|---|
| `[storage] backend` | `local` | Files in the `pdmsdata` Docker volume (`/var/lib/pdms/blobs`). Set `s3` to use object storage |
| `[storage] max_upload_bytes` | 100 MB | |
| `[storage.s3] endpoint, bucket, region, access_key_id, prefix` | | Any S3-compatible service: AWS, SeaweedFS, MinIO, and appliance S3 endpoints. Put the secret in a file and set `PDMS_STORAGE__S3__SECRET_ACCESS_KEY_FILE`. Enable bucket versioning or object lock if available |
| `[storage] clamav_addr` | empty (off) | e.g. `clamav:3310` with `docker compose --profile clamav up -d`. The bundled `deploy/clamav/clamd.conf` raises clamd's size limits to match uploads |
| `[storage] clamav_required` | `true` | When scanning is on and clamd is unreachable, uploads are refused (fail closed) |

Accepted file types are decided from the file's content: PDF, Word, Excel,
PowerPoint (current and 97–2003), OpenDocument, RTF, images, text/CSV, and
email. Other files are refused, and the refusal is logged.

## 5. Reminders, reviews, and the calendar

### How dates drive the system

Each document can have a **next review date**, a **renewal date**, and an
**expiration date**.

**Next review date:** leave it empty to have it computed from the last review
(or the effective/execution date) plus the review interval. The interval is
the document's own, else its type's default: Policy annual, Partnership Agreement five years.

**Status changes:** every 15 minutes the worker:

| When | Status becomes |
|---|---|
| The renewal date is within 90 days | Renewal Due |
| A review or renewal date has passed | Overdue |
| The expiration date has passed | Expired |
| The review or renewal is recorded, or the dates are corrected | Active again |

Documents with no expiration date never expire. Drafts, documents in review,
superseded, and archived documents are left alone.

**Tasks:** a review or renewal task appears 90 days before the date. The
document's page shows it under **Reviews and renewals**:
- **Start** marks it in progress.
- **Record the completed review** sets the last review date and schedules the
  next one.
- **Recording a renewal** asks for the new renewal or expiration date.
- **Approval** is optional, and must come from someone other than the person
  who completed it.

The page also shows how many review cycles were missed. The dashboard lists
overdue items with their missed cycles, your open tasks, and what's coming
up in the next 30, 60, 90, or 365 days.

### Who gets reminded, and when

**Default rules:** remind 90, 30, and 7 days before and on the due date, then
every 7 days while overdue (expirations don't repeat). Reminders go to the
document's owner and the Department Head of its department, by email and in
the in-app notification center (the bell count in the menu).

**Where to change them:**
- **Administration → Notification rules:** the organization defaults, or
  defaults for one document type.
- **The document's Reminders section:** rules for a single document, which
  replace the defaults for that event. Anyone who can edit the document can do
  this.

**Recipients** can be the owner, everyone in the document's department, a
role (holders org-wide or for the document's department), groups,
departments, or people. Someone who cannot open a document is never told
about it. That delivery is recorded as skipped.

**Timing and history:**
- Reminders go out after 07:00 local time (`[notifications] send_hour`).
- If the system was down, it sends the most recent reminder that was due, not
  a burst of old ones.
- **Administration → Delivery log** shows every delivery, its attempts, and
  any errors. Failed emails are retried automatically, up to 6 times.

### Email setup

Until email is configured, reminders still appear in the in-app notification
center. For Exchange Online, edit `config/pdms.toml`:

```toml
[email]
transport = "smtp"
from = "Example Org PDMS <pdms@yourdomain.org>"
smtp_host = "smtp.office365.com"
smtp_port = 587
smtp_security = "starttls"
smtp_username = "pdms@yourdomain.org"
```

Put the mailbox password in `secrets/smtp_password` and restart:
`docker compose up -d app worker`.
- **SMTP AUTH:** the mailbox needs SMTP AUTH enabled in Exchange Online.
- **Relay connector:** if you relay through an Exchange connector or a local
  relay instead, leave `smtp_username` empty and use the relay's host and
  port (`smtp_security = "none"` only on a trusted network).

To run planning immediately (it runs every 15 minutes anyway):
`docker compose run --rm app run-scheduler`.

### Calendar and Outlook

**Calendar** shows reviews, renewals, and expirations by month or as an
agenda, filtered by department, type, status, tags, or "owned by me".
- **Dashed entries:** projected future review cycles.
- **Overdue items:** pinned to today.

**Calendar → Subscribe in Outlook…** creates a private calendar address for
Outlook (**Add calendar → Subscribe from web**) or any calendar app:
- **Address:** shown only once. Treat it like a password and revoke it if it
  leaks.
- **Content:** shows only what the subscriber can currently see, and never
  Restricted documents.
- **Review cycles** appear as recurring events.

## 6. Review, approval, and signatures

**Submitting.** On a document's page, **Review and approval → Submit for
review** (anyone who can edit it). Choose:
- **Reviewers** (optional): people or roles who check it first, all at once.
- **Approvers** (at least one): each must approve. They need the *approve*
  permission on the document (Department Heads have it for their department).
- **Legal review:** added automatically for types that require it (Contract
  and Partnership Agreement by default; change it per type), or tick the box for any document.
  Any holder of the Legal Counsel role can give it.

The document moves to **In Review**. People are told when it is their turn,
in the app and by email; their dashboard lists it under **Awaiting my review
or approval**.

**Deciding.** Each step can be approved, sent back with **Request changes**,
or **Rejected** (both need a comment). When everyone has approved, the
document becomes **Active**. A rejection or change request ends the round and
returns the document to Draft (or Active, for a revision of an active
document); the submitter and owner are told. Fix it and submit again. Every
round, decision, and comment stays on the page and in the audit log.

**Separation of duties.** The submitter cannot approve or give the legal
sign-off on their own submission, even if they hold the role. (An
organization can allow self-approval with `[workflow] allow_self_approval =
true` in `config/pdms.toml`; it is off by default.) If a
new file is uploaded during review, approval is refused until the review is
cancelled and resubmitted, so approvers always see the current file.

**Signatures.** The **Signatures** section lists who must sign. Add people
here (with an account) or outside signers, **Request** their signature
(people here are notified), then **Record signature** with the date and the
signed copy, which is saved as a *Signed copy* version. Declines are recorded
too. This tracks signatures; it does not collect electronic signatures (an
e-signature service can be connected later through the signature provider
interface).

## 7. Boards and appointments

**Appointments** in the menu lists positions: seats on boards, committees,
and commissions, or appointed roles. Appointments clerks and administrators
manage them; a position's manager and the members of its department can see
it; auditors see all.

**Setting up a position:** body and position name, appointing authority, term
length (blank for no fixed term), number of seats, department, the staff
member responsible (manager), and how many days before a term ends the
reappointment decision is due. Then add its **requirements checklist**
(for example "Appointment approved by board resolution", "Acceptance letter
on file", "Requires legal counsel review"). Retiring an item keeps the
records already made against it.

**Appointing:** record the person (optionally linked to a user here), the
reference (resolution or meeting minutes), and the start date. The end date and the
reappointment decision date default from the position. For each current
appointee you can:
- **tick checklist items** (the manager can too). Who and when are recorded;
- **Reappoint**: a new term for the same person; the checklist starts over;
- **Replace**: a successor takes the seat;
- **End without successor**: resignation, removal; the seat becomes vacant;
- **Correct details**.

Nothing is deleted. The position page shows the full history of appointees.
Someone whose term ended with no successor recorded is shown as a
**holdover**.

**Reminders:** term ends are reminded 90, 30, and 7 days before and on the
day; reappointment decisions 60, 30, 7 days before, on the day, then weekly
until a decision is recorded. They go to the position's manager and the
Appointments Clerks. Change the defaults under **Administration →
Notification rules** (events *Term end* and *Reappointment*). Term ends and
decisions also appear on the calendar, the dashboard, and Outlook
subscriptions of everyone who can see the position.

## 8. Records requests and reports

**Records requests.** Records officers use **Records requests** in the menu:
1. search for the documents (only documents you may *export* appear;
   Restricted ones need an explicit grant);
2. tick the documents, enter the **request reference** (for example
   `REQ-2026-014`), choose the files (current, every version, or none), whether
   to include each document's audit history, and which details to include;
3. **Build package**, then **Download**.

The ZIP contains a README, `manifest.json` (the request, the options, every
document and file), each document's `metadata.json` and files, the optional
audit history (JSON and CSV), and `SHA256SUMS` (check with
`sha256sum -c SHA256SUMS`). Every file is checked against its stored hash while
the package is built. Building and downloading are recorded in the audit log
and on each document's history; the package's SHA-256 is kept, so you can
later prove what was released. Auditors can see and download every package.

**Reports.** **Reports** in the menu lists what you may run; every report can
be downloaded as CSV (downloads are recorded):

| Report | Who |
|---|---|
| Upcoming reviews, renewals, and expirations (and appointment dates); Overdue items; Documents by department, owner, or tag; Ownership (flags owners who left) | Everyone, over the documents they can see |
| Access review: grants (flags grants older than N days, expired, or held by departed users); role assignments; orphaned and departed accounts | Administrators and auditors |
| Audit summary; Denied access attempts; Records requests | Auditors and administrators |

Run the access review reports quarterly: revoke what is no longer needed and
transfer documents owned by people who have left.

## 9. API, webhooks, and integrations

**REST API.** Every user can create **API tokens** (link next to your name):
choose a name, scopes, and a lifetime (up to `[api] max_token_days`). The
token is shown once. A token acts as you and can never do more than you can;
revoking it, or deactivating you, stops it immediately. The reference is at
`/api/docs` (and `/api/v1/openapi.json` for code generators). Under
**Administration → API**, administrators see every token (and can revoke any)
and the most recent requests; every request is logged and each token is rate
limited (`[api] requests_per_minute`). Turn the API off with
`[api] enabled = false`.

For an integration (for example a records platform or a records-request portal),
create a dedicated user with only the access the integration needs, sign in as
it once, and create its token there.

**Webhooks.** **Administration → Webhooks → Add a webhook** sends events to
another system as they happen:
- **Events:** exact names (`workflow.approved`), families (`document.*`,
  `workflow.*`, `signature.*`, `appointment.*`, `review_task.*`), or `*`.
  Views and downloads are never sent.
- **Service user:** the webhook only receives events about documents and
  positions this user can see. Use a dedicated account.
- **Signing secret:** shown once; the receiver verifies
  `X-PDMS-Signature: t=<unix time>,v1=<hex HMAC-SHA256 of "<t>.<body>">`.
  Rotate it from the webhook's page.
- **Delivery:** HTTPS only (`allow_http_webhooks` is for testing); any 2xx
  answer is success; failures are retried with growing delays up to
  `webhook_max_attempts`; every attempt is listed on the webhook's page, where
  you can **Redeliver**. The event's `id` is the same on every retry, so the
  receiver can ignore duplicates.

**Integration providers.** Future connectors (for example to a records
management or records-request system) plug in as providers and are enabled by key
in `[integrations] providers`. The built-in `log` provider writes every event
to the application log, which shows exactly what a connector would receive.

**Audit forwarding.** To keep an independent copy of the audit trail in your
SIEM or log platform, set `[audit_forward] target = "file"` (JSON lines; point
your log shipper at it) or `"syslog"` with `syslog_addr`. Every entry is sent
once, in order, including its hash; denied actions are sent at warning
severity.

## 10. Comments, file text, and Outlook

**Comments.** Every document has a **Comments** section. Anyone who can see
the document can comment and reply (set `[comments] viewers_can_comment =
false` to require edit access), except on County-wide documents, where only
the owner and administrators can comment (everyone who can see it still reads
the comments). Mention people with `@` and their email address, or the part
before the `@` when it is unique (`@carol`); they are notified if they can see
the document. Replies notify the person replied to.
- Authors can edit their comments (while they may still comment); the
  earlier text is kept and shown under **History** to the author, the
  document's permission managers, and auditors.
- On every document, only the author can delete a comment, not
  administrators, owners, or permission managers (D-091). It shows as
  deleted, and its text stays in the history and the audit log.

**Text in files and dates found.** When a file is uploaded, PDMS reads its
text in the background: text in ordinary PDFs, Word (`.docx`), and text files
is always read; scanned PDFs and photos need OCR (below). The text is added
to search, so you can find a document by words inside its file. PDMS also
looks for dates and terms ("effective March 1, 2013", "expires on…", "a term
of five (5) years", "reviewed annually") and lists them under **Dates found
in the file** on the document page, with the sentence they came from. The
owner is told when there are dates to confirm.
- **Nothing changes until someone with edit access clicks Use this date.**
  That records an ordinary, audited change; other suggestions for the same
  field are set aside. **Dismiss** leaves the document as it is.
- Suggestions are hints, often right but sometimes not (an amendment's
  signing date, a date in a recital). Always check them against the file.
- **Read the file again** re-runs extraction (for example after OCR is turned
  on).

**OCR for scanned documents (optional).** Because Rust has no mature OCR
engine, OCR runs in a separate container (OCRmyPDF with Tesseract):
1. `docker compose --profile ocr up -d` (builds and starts `ocr`);
2. in `config/pdms.toml` set `[ocr] sidecar_url = "http://ocr:8080"`;
3. `docker compose up -d app worker`.
English is installed; other Tesseract languages can be added by extending
`deploy/ocr/Dockerfile` and setting `PDMS_OCR_LANGUAGES` (for example
`eng+spa`). Without OCR everything else works; scanned files simply have no
searchable text. **Administration → Outlook & OCR** shows whether OCR is on.

**Outlook calendar through Microsoft Graph (optional).** Calendar
subscriptions (section 5) work without any setup. If you prefer real Outlook
events in a shared calendar, PDMS can create one all-day event per review,
renewal, and expiration date (and appointment term ends and reappointment
decisions), update it when the date changes, and remove it when the date no
longer applies. Restricted documents are never included.
1. Create a shared mailbox, for example `pdms-calendar@yourdomain.org`, and
   share its calendar with the staff who should see it (or create one per
   department).
2. In the Entra admin center: **App registrations → New registration**
   ("PDMS calendar"). Under **API permissions** add Microsoft Graph
   **Application** permission `Calendars.ReadWrite` and grant admin consent.
   Under **Certificates & secrets** create a client secret.
3. Limit the app to those mailboxes (Exchange Online PowerShell), for example
   `New-ApplicationAccessPolicy -AppId <client id> -PolicyScopeGroupId
   pdms-calendars@yourdomain.org -AccessRight RestrictAccess` (or an
   equivalent RBAC for Applications assignment), so it cannot touch any other
   mailbox.
4. Put the secret in `secrets/graph_client_secret`, and in
   `config/pdms.toml` set `[graph_calendar] enabled = true`, `tenant_id`,
   `client_id`, `mailbox`, and optionally `[graph_calendar.department_mailboxes]`
   (department code = mailbox). Restart: `docker compose up -d app worker`.
5. **Administration → Outlook & OCR → Synchronize now** runs the first sync;
   afterwards it runs every `sync_interval_minutes`. Problems are listed there.
Events are owned by PDMS: edits made to them in Outlook are overwritten at the
next change, and events deleted in Outlook are recreated when their date
changes.

## 11. The audit log

**Audit log** in the top navigation (Administrators and Auditors).

- Filter by action prefix (`auth.`, `acl.`, `department.`), category, outcome,
  user, object, and date range. Each row shows who, when (in your time zone),
  how they signed in, from which IP, and a field-by-field before/after table.
- **Export CSV / JSON** (Auditors only). The export itself is logged. CSV cells
  that begin with formula characters are neutralized for spreadsheet safety.
- **Verify integrity** recomputes the hash chain. The worker also verifies it
  nightly and logs an `AUDIT CHAIN BREAK` error if anything is wrong.

Command line (exit code 2 if the chain is broken, useful for monitoring):

```sh
docker compose run --rm app verify-audit
docker compose run --rm app verify-audit --json
```

**Nobody can edit or delete audit entries through the application.** The
application's database account can only add and read rows, and database
triggers block changes even by the schema owner. If verification reports a
break, someone with database superuser access changed the data. Preserve the
database (take a copy before anything else), record the reported sequence
numbers, and investigate.

## 12. Operations

| Need | How |
|---|---|
| Health | `GET /healthz` (process up), `GET /readyz` (database reachable) |
| Metrics | `GET /metrics` (Prometheus; block it at the proxy for outside users) |
| Logs | `docker compose logs -f app worker`: JSON lines with request IDs |
| Validate configuration | `docker compose run --rm app check-config` (secrets redacted) |
| Verify files and audit chain | `docker compose run --rm app verify-integrity` (exit code 2 on problems; see BACKUP.md) |
| Back up / restore | `deploy/backup/pdms-backup.sh`, `deploy/backup/pdms-restore.sh` (see BACKUP.md) |
| Upgrade | `git pull && ./install.sh`. The installer keeps your settings and secrets, asks only about new options, rebuilds, applies database migrations (the `migrate` service runs before `app` and `worker`; the app refuses to start against an outdated schema), and restarts what changed. Back up first. |

### Backups

See [BACKUP.md](BACKUP.md): what to protect (the database, the file store,
`secrets/`, `config/`), `deploy/backup/pdms-backup.sh` for a consistent backup
(database first, then files), backup software setup, `deploy/backup/pdms-restore.sh`,
and the restore drill. After any restore, and whenever you want assurance, run:

```sh
docker compose run --rm app verify-integrity
```

It re-hashes every stored file, checks every export package and version, and
verifies the audit chain; it exits with code 2 if anything is wrong.

## 13. Configuration reference

See `config/pdms.example.toml` for every setting with its default. Any setting
can be overridden with an environment variable, `PDMS_<SECTION>__<KEY>` (for
example `PDMS_AUTH__SESSION_IDLE_MINUTES=20`). Append `_FILE` to read the value
from a file.
