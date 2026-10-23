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
| How people reach PDMS | **1) Bundled Caddy:** automatic HTTPS. A domain gets a Let's Encrypt certificate (ports 80/443 reachable from the internet); without a domain, Caddy uses an internal certificate for the server's IP (browsers warn until it is trusted). **2) Your own reverse proxy:** enter the HTTPS address users open and the proxy's IP; PDMS then listens on all interfaces at its port (firewall it to the proxy). **3) Local testing:** plain HTTP on `localhost`. The host port defaults to 8080 or the next free port. Caddy's ports default to 80 and 443; if something else on the server already uses them, the installer suggests the next free ones (for example 81 and 444), and the address users open then includes the port (`https://10.0.0.5:444`). Let's Encrypt still checks a domain on ports 80 and 443, so with other ports a domain's certificate is only issued if your firewall forwards 80 and 443 to them; for testing, leave the domain blank. |
| Database for host tools | Optional: publishes PostgreSQL on `127.0.0.1` only (5432 or the next free port) through `docker-compose.override.yml`. Not needed for normal operation or backups. |
| Name and time zone | Shown in titles, emails, and calendars; the time zone (detected from the host) drives reminder scheduling. |
| Source code URL | Where users can get this installation's source code, as the AGPL-3.0 requires for software used over a network (defaults to the git remote). Linked from every page as **Source code**. |
| Secrets | Generated once and never overwritten: PostgreSQL superuser, owner, and app passwords (256-bit, hex), and the 512-bit master key. One file each in `secrets/` (directory `0700`), mounted as Docker secrets, never written to `.env` or the configuration. |
| OCR service, virus scanner | Whether to include each optional container (each about 1 GB). Turning them on and off is done under Administration (section 5). |
| First administrator | Email, username (default `sysadmin`; preset `PDMS_INSTALL_ADMIN_USERNAME`; checked against the username rules below and shown in the summary), and name. The password is generated and shown once at the end, with the **authenticator (TOTP) secret**: add it to an authenticator app right away. |

Files it writes:

| File | Contents |
|---|---|
| `.env` (`0600`) | Deployment settings for Docker Compose: proxy mode, host port and bind address, Caddy's ports, public URL, database port, enabled profiles. Docker Compose reads it automatically. |
| `config/pdms.toml` | Application settings. Blocks between `# >>> install.sh:<name>` markers are managed by the installer; edit anything else freely. |
| `config/Caddyfile` | With the bundled proxy. |
| `secrets/` | Passwords and keys. Back them up securely (see BACKUP.md). |

**Then, in the browser.** Sign in with the first administrator and set up
the rest under **Administration**: Microsoft sign-in (section 2), email,
the Microsoft 365 connection, SharePoint storage and the Entra ID user sync,
OCR and virus scanning. Each page has a **Test** and applies a save at once,
without a restart. `docker compose run --rm app settings show` lists every
such setting and where its value comes from; for a server whose admin pages
can't be reached (for example when sign-in is broken and no break-glass
account is left), the same command has `set KEY VALUE`, `set-secret KEY`
(the value on standard input) and `test AREA`, with the same checks and
audit as the pages.

**Later changes.** `./install.sh --configure <part>` redoes one part (comma-separated
for several): `network`, `database-port`, `source`, `ocr` or `clamav` (which
containers to include), `admin` (resets the break-glass administrator), or
`move-settings` (below). The services are restarted only if a setting
actually changed. Running `./install.sh` with no options asks only about
parts that are not set up yet, so it is also the upgrade step. Settings
written by hand are kept. `--configure sso`, `smtp` and `sharepoint` are no
longer part of the installer: they say which admin page has the setting.

**Upgrading an install made before the admin pages.** Earlier installers
wrote sign-in, email, SharePoint, OCR and virus-scanning settings into
blocks of `config/pdms.toml` and into `secrets/`. Settings in the file win
and show read-only on the pages (D-111), so on the first upgrade the
installer offers once to **move them into Administration**. On yes, after
the database migration it runs `pdms settings import`, which copies what
those blocks set into the database (passwords, the client secret and the
Graph certificate's key encrypted with the master key; the certificate file
becomes the active certificate on the Connection page). Then it removes the
blocks from `config/pdms.toml` (keeping a copy as
`config/pdms.toml.before-move`), empties `secrets/oidc_client_secret`,
`secrets/smtp_password` and `secrets/graph_certificate`, and restarts. If
the import fails, nothing is removed. Settings you wrote by hand outside the
blocks stay in the file and stay read-only; the import lists them. On no,
nothing changes; `./install.sh --configure move-settings` moves them later
(D-116).

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
   - Redirect URI (Web): `https://<your-public-url>/auth/oidc/callback`.
     It must match PDMS's address exactly, including any port: with Caddy
     on port 444, for example, it is
     `https://10.0.0.5:444/auth/oidc/callback`. **Administration → Microsoft
     365 → Sign-in** shows the exact value to copy, and the installer prints
     it at the end.
2. **Certificates & secrets → New client secret.** Copy the value; you enter
   it in step 6.
3. **Token configuration → Add groups claim → Security groups**, emitted as
   **Group ID** in the ID token. This drives automatic department and role
   assignment through identity provider mappings. Skip this step if you will
   use the Entra ID user sync (below): while the sync is on, groups in the
   token are not used.
4. **API permissions:** `openid`, `profile`, `email` (Microsoft Graph,
   delegated). Grant admin consent.
5. Optional: under **Enterprise applications → (your app) → Properties**, set
   *Assignment required* to Yes and assign the groups that may use the system.
6. Sign in to PDMS with a break-glass local account and open
   **Administration → Microsoft 365 → Sign-in**. Enter the directory (tenant)
   ID, the application (client) ID and the client secret, and tick **Staff
   can sign in with Microsoft**. **Test** reads Microsoft's sign-in
   configuration for the tenant and checks the request PDMS would send,
   without saving anything or signing anyone in. Then choose **Save**: it
   applies at once, without a restart.
7. Choose **Try sign-in**. It opens Microsoft's sign-in in a new tab; sign in
   with your work account to prove the whole flow, including the redirect
   URI and the secret, which only Microsoft can check. The sign-in page shows
   **Sign in with Microsoft** (the **Button label** sets the last word).

The client secret is stored encrypted in the database and never shown again.
Client secrets expire (at most two years in Entra): add the new one on the
page before the old one runs out; leaving the field empty keeps the saved
one. The same values can still be set in `[auth.oidc]` in `config/pdms.toml`
and `secrets/oidc_client_secret`; there they win and show read-only on the
page. For another OpenID Connect provider, enter its issuer URL in the
tenant field.

**Lockout rule (D-115):** while break-glass local sign-in is on, any change
can be saved, because local sign-in keeps working if Microsoft sign-in
breaks. With `[auth.local] enabled = false`, Microsoft sign-in can't be
turned off on the page, and a change is saved only if it passes the test.

Users are identified by their Entra **object ID** (`oid`), never by email, so
renaming an email address does not create a duplicate account. An existing
account is never linked to an Entra account because the email addresses match.
On first sign-in, a person is created with no access
(`auth.oidc.auto_provision = false` turns that off). If the Entra ID user sync
is on, people are created by the sync before they ever sign in, and a first
sign-in for an Entra account whose email belongs to an existing, unlinked
account is refused (see "Entra ID user sync" in section 3).

With single sign-on on, `auto_provision = false`, and the sync off, only people
who are already linked to their Entra account can sign in. PDMS logs a warning
at startup in that setup. Turn on the sync, or allow auto-provisioning, so new
people can sign in.

**"AADSTS50011: The redirect URI … does not match"** means the app
registration does not list the address PDMS sent. Copy the URI from the
error message (or from the Sign-in page) into **App registrations →
(your app) → Authentication → Web → Redirect URIs**, save, and sign in again.
It changes whenever PDMS's address changes (`./install.sh --configure
network`), for example when Caddy moves to another port.

If Entra is unreachable when the app starts, or after a change, Microsoft
sign-in is unavailable and the Sign-in page shows why; PDMS tries again every
5 minutes, and break-glass sign-in keeps working.

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

**Finding an admin page.** Administration has a menu of its own, beside the
page (on a phone, behind **Administration: …** at the top), in five groups:
- *People and access:* Users, Departments, Groups, Roles, Access grants,
  Identity provider mappings;
- *Microsoft 365:* Connection, Sign-in, Entra ID sync, SharePoint;
- *Documents and records:* Document types, Custom fields, Tags, Retention,
  Deleted documents;
- *Notifications:* Notification rules, Delivery log;
- *Integrations:* Webhooks, API;
- *System:* Branding, Email, Text extraction (OCR), Virus scanning.

You see only the pages your roles allow.

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
  the document's owner and PDMS administrators can comment on them, though
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
PDMS presents your county. No config edit or restart is needed.

| Field | Rules |
|---|---|
| County name | 1–120 characters. Leave blank to use `[org] name` from the config file. |
| Accent color | Pick a color or type a hex code. White text on it must reach a 4.5:1 contrast ratio, or the page refuses it with a message (choose a darker color). Hover, link, and tint shades are derived automatically, for light and for dark mode; an accent with no readable dark-mode shades is refused too. |
| Seal | PNG or JPEG, at most 512 KB, 16–2048 pixels per side. The real file type is checked, and SVG is refused. It is stored in PDMS's database, so backups include it. It is also the browser-tab icon; without a seal the tab shows a generic PDMS icon. A square seal works best there. |

Two more actions: **Remove seal**, and **Reset to defaults** (asks you to
confirm on a separate page).

- **Public:** the seal and county name appear on the sign-in page, so anyone
  who can reach PDMS sees them. Do not upload anything you would not put on
  your public website.
- **Where the name is used:** page titles, the sidebar, notification emails,
  calendar feeds, webhooks, and records-request packages.
- **When changes apply:** immediately in the process that saved them, and
  within 60 seconds in any other process (the web app and the worker are
  separate processes).
- **Audit entries:** `branding.updated`, `branding.seal_set`,
  `branding.seal_removed`, `branding.reset`.

### Dark and light mode

PDMS is **dark by default**. Each person chooses for themselves, and nobody
else can change it for them (D-098):
- **Your account → Appearance:** Dark, Light, or Match my device (follows the
  computer's or phone's setting).
- **The switch at the bottom of the sidebar** flips between dark and light in
  one click and stays on the same page.

Things to know:
- The choice is saved with the account, so it follows the person to another
  computer.
- The sign-in page is always dark, and printed pages are always light.
- Each change is recorded in the audit log as `user.color_scheme_set`.

### Preparing a demo for leadership

To show PDMS to county leadership on a test or demo server, after it is
installed or updated:

1. **Give the administrator a username**, so they can sign in as `sysadmin`:
   `docker compose run --rm app set-username --email <admin email> --username sysadmin`
2. **Brand it.** Administration → Branding: set the county name, accent color,
   and seal. Placeholders are fine until the county decides.
3. **Let leadership see the whole county.** The county overview (top of
   **Reports**) and the dashboard (including its **Recent activity** panel)
   show only what the viewer can see. For each
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
one department. While the Entra ID user sync is off, PDMS adds and removes the
memberships it manages this way at every sign-in. Memberships an administrator
added by hand are never changed.

While the sync is on, these mappings are not applied (the page says so), and
groups in the token are ignored. Grants made by earlier mappings stay until an
administrator removes them: roles on the person's page, group memberships on
the group's page. Review them before you turn the sync on.

### Entra ID user sync

With the sync on, people and their departments come from Entra ID instead
of token group claims. Each night (at `[entra_sync] run_at`, default 02:30 in
the `[org]` timezone) PDMS picks up the changes made in Entra ID since the
last run (the first run reads every account):

- new people are created, so they have access before their first sign-in;
- names, emails, and departments are updated;
- people whose account is disabled or deleted in Entra are marked
  **departed** and signed out. The sync never reactivates anyone: if someone
  returns, an administrator reactivates the account;
- guest (B2B) accounts are skipped (unless `include_guests = true`). An
  account with no user type counts as a member.

**Settings.** On **Administration → Entra ID sync**, under **Settings**:
turn the sync on, the time it runs, the department field, and whether to
include guest accounts. The same values can be set in `[entra_sync]` in
`config/pdms.toml`; there they win and show read-only. Turning the sync on
needs the Microsoft 365 connection and Microsoft sign-in with `oid` as the
subject claim; PDMS refuses with the reason otherwise. **Preview** reads the
first ten accounts from Entra ID and shows, for each, the department value,
the PDMS department it maps to, and whether the sync would skip it (guest,
disabled, no email address). It changes nothing.

The department comes from the Entra account property named by
`[entra_sync] attribute` (default **Department**). Under **Administration →
Entra ID sync**, map each value in use ("Road & Bridge", "R&B", ...) to a PDMS
department; case, spacing, and `&` versus "and" do not matter. People with a
blank or unmapped value can sign in but see only what is shared with them;
the page lists those values with counts. Make the Department field part of
account creation. A department membership you add by hand in the mapped
department counts as satisfying the mapping. A mapping you add, change, or
remove applies at once to the people already synced with that value; you do
not need a full resync. The one exception: if you change mappings while a
sync is running (during the long first run, the nightly run at
`[entra_sync] run_at`, or just after **Sync now** or **Full resync**), wait
until **Last run** shows a time after your change, then press **Full
resync**, so nobody is left on the old mapping.

Roles (administrator, legal counsel, records officer, auditor, appointments
clerk, department head) are still assigned in PDMS. Memberships you add by
hand are never changed by the sync. While the sync is on, the sign-in token
never sets a person's name or groups.

**Departures and safeguards.**

- A full round reads every account: the first run, an expired delta token,
  **Full resync**, or a changed `[entra_sync] attribute` or `include_guests`.
  It also departs people from this identity provider whom Entra no longer
  lists (not people whose account was created while the round was running).
  Turning `include_guests` off departs the guest accounts synced earlier.
- The round holds back and reports instead of departing anyone if Entra
  returns no member accounts, or if it would depart more than 20% of linked
  accounts (at least 5). The report names the accounts it would have departed
  (up to 20). Check Entra and the Graph permissions. If the departures are
  real, depart those people by hand on their pages (Administration → Users).
- An account that also has a local password is departed like any other, so
  the password no longer signs in; it is listed under **Needs attention** so
  you know. The break-glass administrator has no Entra link, so the sync never
  touches it.
- The last administrator is never departed automatically; it is listed under
  **Needs attention** instead.

**Sign-in.** People are matched by Entra object ID, never by email. An Entra
account whose email equals an existing unlinked PDMS account (for example the
break-glass administrator) is not linked: the sync reports it, and sign-in is
refused (`email_in_use`) until you rename or remove the other account. With
the sync on, a first sign-in asks Entra about the person; if Entra cannot be
reached, sign-in is refused rather than trusting the token alone.

**The status page.** **Administration → Entra ID sync** shows the last run, a
summary, the last error (shown only when the last run failed), and the
certificate thumbprint. The summary counts people without a mapped department
only among those the run read; the **Values with no department** table covers
everyone. Problems with individual accounts are listed under **Needs
attention**. **Sync now** queues a run; if a sync is already running, the
queued run is skipped. **Full resync** reads every account again.

**Setup.**

1. In Entra ID, register an app **PDMS Service**, add the Microsoft Graph
   **application** permission `User.Read.All`, and grant admin consent. There
   is no client secret: PDMS signs in with a certificate.
2. On **Administration → Microsoft 365 → Connection**, choose **Make a new
   certificate**, then **Download** it and upload the `.crt` to the app
   registration under **Certificates & secrets → Certificates**. The private
   key never leaves PDMS (it is stored encrypted in the database). Enter the
   **Directory (tenant) ID** and **Application (client) ID**, choose **Test
   these IDs**, then **Save**. PDMS uses the connection at once, without a
   restart.

   A certificate in `secrets/graph_certificate`, or IDs in `[graph]` in
   `config/pdms.toml`, win over the page and show read-only there. Docker
   Compose always mounts `secrets/graph_certificate`; an empty file means
   "not set in the file". (`pdms gen-graph-cert --out secrets/graph_certificate`
   still makes a file certificate for installs that prefer it.)

   **If you followed an earlier version of this guide** and added the
   certificate to `docker-compose.yml` by hand, undo that edit before
   upgrading (`git checkout docker-compose.yml`), or `git pull` stops with a
   conflict. The file now has those lines itself.
3. Turn the sync on. Set:

   ```toml
   [entra_sync]
   enabled = true
   ```

   The sync needs `[graph]`, `auth.oidc.issuer_url`, and
   `auth.oidc.subject_claim = "oid"`; PDMS refuses to start otherwise. Restart,
   then press **Sync now**. `pdms sync-directory` runs a sync from the command
   line; `pdms sync-directory --full` reads every account again. Both refuse
   to run while `enabled = false`.
4. Renew the certificate before it expires (administrators get a reminder
   30 days before): on the Connection page choose **Make a new certificate**,
   upload its `.crt` to the app registration, then choose **Use this
   certificate**. The old one keeps working until then, so there is no
   outage. Then remove the old certificate from the app registration. The
   page shows both thumbprints: Entra lists the SHA-1 one; **Administration →
   Entra ID sync** shows the SHA-256 one.

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
- **Seeded types:** Policy, Contract, Interlocal Agreement (ILA), and Other.
  Interlocal Agreement was called Partnership Agreement before D-088; the
  upgrade renamed it and changed its ID prefix from `PAG` to `ILA`. Documents
  keep the IDs they already have (`PAG-…`); new ones are numbered `ILA-…`.
  If you had already renamed the type or its prefix, your choice was kept.
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

Tags are kept by administrators (D-088), so the list stays short and
consistent for everyone who filters by it:
- **Administration → Tags** (under *Documents and records*) adds a tag, with
  a color and a description that says when to use it, and edits or renames
  existing ones.
- People who edit a document **choose** tags from that list, on the
  new-document form and in the document's **Tags** panel. They cannot type a
  new one. Tag managers can still add a new name there, which creates it.
- **Needs review** lists tags typed on documents while free-form tags were
  allowed (before D-088, or when the setting below is on). Tick **Curated**
  to keep one for everyone, or rename it to the name of an existing tag.
- To let editors add their own tags again, set `[documents] free_form_tags =
  true` and restart.

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
| `[storage] backend` | `local` | Files in the `pdmsdata` Docker volume (`/var/lib/pdms/blobs`). Set `s3` to use object storage. SharePoint storage is switched on Administration → SharePoint (below) |
| `[storage] max_upload_bytes` | 100 MB | |
| `[storage.s3] endpoint, bucket, region, access_key_id, prefix` | | Any S3-compatible service: AWS, SeaweedFS, MinIO, and appliance S3 endpoints. Put the secret in a file and set `PDMS_STORAGE__S3__SECRET_ACCESS_KEY_FILE`. Enable bucket versioning or object lock if available |
| `[storage] clamav_addr` | empty (off) | Set on **Administration → Virus scanning** (below). The bundled `deploy/clamav/clamd.conf` raises clamd's size limits to match uploads |
| `[storage] clamav_required` | `true` | When scanning is on and clamd is unreachable, uploads are refused (fail closed) |

**Virus scanning.** ClamAV runs in its own container (about 1 GB of memory),
which the installer includes when you answer yes to virus scanning
(`./install.sh --configure clamav`). On **Administration → Virus scanning**,
turn scanning on (PDMS uses the container at `clamav:3310`) and choose
whether uploads are refused while ClamAV can't be reached. **Test** sends
the EICAR test file, which is harmless and which every virus scanner
reports, and expects ClamAV to find it; it says so when the container isn't
running. A save applies to the next upload, without a restart.

Accepted file types are decided from the file's content: PDF, Word, Excel,
PowerPoint (current and 97–2003), OpenDocument, RTF, images, text/CSV, and
email. Other files are refused, and the refusal is logged.

### SharePoint storage

With `[storage] backend = "sharepoint"`, every document file uploaded through
PDMS is stored in one SharePoint document library, so people can also browse
the files there, read-only. PDMS stays the system people use: details,
permissions, workflow, reminders, search and the audit log stay in PDMS, and
PDMS wins when the two disagree (D-081). Export packages (records requests)
stay in `local_path`.

| Setting (`config/pdms.toml`) | Default | Notes |
|---|---|---|
| `[sharepoint] site_url` | empty | The site, for example `https://contoso.sharepoint.com/sites/PDMSRecords` |
| `[sharepoint] library` | `Documents` | The document library's name |
| `[sharepoint] upload_chunk_bytes` | 10 MiB | Files are sent in pieces of this size: a multiple of 320 KiB, at most 60 MiB |
| `[sharepoint] delta_interval_minutes` | 5 | How often the change check reads SharePoint's change feed |

**Setting it up:** follow [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md). In short:
1. Set up the site: owners only, sharing restricted, versioning on, and
   `County-wide` readable by **Everyone except external users**. If PDMS has
   to create `County-wide` itself, it logs a warning, because that read
   permission is then missing.
2. Register the `PDMS Service` app with the Graph application permission
   `Sites.Selected`, and give it full control on this one site (the guide has
   the exact commands).
3. Set up the Microsoft 365 connection (**Administration → Microsoft 365 →
   Connection**), then, on **Administration → SharePoint** under
   **Settings**, enter the site address and library and choose **Save**.
   **Test** checks the site without saving or changing anything: the site,
   the library, the PDMS columns, the three folders, and whether PDMS can
   read the library's sharing. It is in the audit log as `settings.tested`.
4. Choose **Turn SharePoint storage on**. PDMS tests the site again first and
   stays off if the test fails. **Start with an empty system:** it refuses
   while any document file is stored on this server (or in S3), because those
   files are not moved to SharePoint and can't be read once the backend
   changes. On the test server, reset it first.
5. To switch back, type the site address under **Storage** and choose **Turn
   SharePoint storage off**. Files already in SharePoint stay there, but PDMS
   can no longer open them until storage is on again. While SharePoint holds
   PDMS files, the site and library can't be changed, because PDMS tracks each
   file by its ID in that library (D-114).

The page needs the `manage_settings` capability for the Settings panel; the
rest of the page needs `manage_sharepoint`. Values set in `config/pdms.toml`
show read-only and win over the page (D-111); an install made by an earlier
installer moves them into Administration when it upgrades (section 1). PDMS
refuses to start with `backend = "sharepoint"` and no Microsoft 365
connection.

**How it works:**
- **Layout.** `Departments/`, `County-wide/` or `Restricted/` (from the
  document's visibility), then the department's folder, then one folder per
  document (`{ID} – {title}`) with one file per version. Files are never
  overwritten. Deleted documents wait in `Restricted/`.
- **Uploads.** A file is uploaded while the person waits, before anything is
  saved. If SharePoint can't be reached, nothing is saved and the form comes
  back with "File storage (SharePoint) is unavailable right now, so nothing
  was saved". Empty files are refused, because SharePoint can't store them.
- **Names and columns.** A new file first has a temporary name,
  `{ID} (uploading …)`. A background job (`sharepoint.mirror_document`), which
  runs after every change, then gives it its final name, such as
  `CON-2026-000012 v2 – Amendment.pdf`, fills in the PDMS columns (type,
  tags, status, dates, owner, Current, "Open in PDMS"), and moves or renames
  folders. A nightly sweep queues it for every document, which picks up
  renamed departments, types and people.
- **Changes that narrow access move the folder first**: tightening
  visibility, moving a document to another department, and deleting it. If
  SharePoint can't be reached, the change is refused ("…so the change was not
  made") and nothing changes. Every folder move is in the audit log as
  `sharepoint.folder_moved`.
- **Downloads** are streamed through PDMS and checked against the SHA-256
  recorded at upload. If someone changed the file in SharePoint, the download
  stops with an error, the file is marked as changed,
  `sharepoint.changed_outside` is written to the audit log, and
  administrators are notified. Restoring an earlier version copies its file
  into a new file.
- **Department folder access.** Members of a department whose account comes
  from Entra ID get read access to `Departments/<department>` in SharePoint,
  one person at a time and with no invitation email. PDMS updates it about a
  minute after a membership changes (including changes the Entra ID user sync
  makes) and every night, and removes it when someone leaves the department
  or is departed. It removes only access it granted; anything else on the
  folder is reported, never changed. Local accounts, such as the break-glass
  administrator, never get SharePoint access. Without the Entra ID user sync,
  memberships come from sign-in or are set by hand.
- **The change check** reads the site's change feed every
  `delta_interval_minutes`. A file whose size or content hash no longer
  matches what PDMS uploaded, or that was deleted, is flagged and audited,
  and administrators get one notice per check that finds changes. Who made
  the change is shown, but the hashes decide. Renamed or moved files and
  folders are put back in PDMS's layout without a flag. Files PDMS does not
  track are listed once they are ten minutes old (an upload in progress has
  no record yet), and PDMS never deletes them.
- **Administration → SharePoint** (administrators only) shows when the
  change check and the folder-access run last ran and any problem, files
  changed outside PDMS with their actions, files PDMS does not track
  (**Dismiss** takes one off the list; PDMS never deletes it), and access on
  department folders that PDMS did not grant. A document you cannot open is
  listed only as "A Restricted document" and its version. People who can see
  a flagged document see a notice on its page, and its downloads stop until
  it is resolved. Every action is audited:

  | Action | What it does | Who |
  |---|---|---|
  | **Restore original** | Puts back the SharePoint version whose content matches PDMS's record (checked by SHA-256, never by its label) | administrators |
  | **Accept as new version** | Takes the file as it is now through the normal upload checks (file type, virus scan) and adds it as a new version with a note saying who changed it; then puts the original back | administrators with edit permission on the document |
  | **Re-check** | Compares the file again, for example after a site owner restored a deleted file from the recycle bin, and clears the flag if it matches | administrators |

  If PDMS cannot sign in to Microsoft Graph (expired certificate, removed
  consent or site grant), administrators get a notice at most once an hour.
  See [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md) C2.
- **Integrity check.** `verify-integrity` checks every SharePoint file
  through Microsoft Graph: present, the right size, and the QuickXorHash
  recorded at upload (`--skip-files` checks presence and size only). Add
  `--rehash-sharepoint` to download every file and compare its SHA-256 too;
  that is slow on a large library, so run it occasionally, for example after
  restoring a backup. Files still flagged by the change check are reported
  as problems.
- **If a save fails** after its file was uploaded, the file stays in
  SharePoint and the change check lists it under "Files PDMS does not track"
  as "PDMS (a save that did not finish; safe to delete)". If a change fails
  after its folder already moved, the next mirror run or the nightly sweep
  puts the folder back.

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

Until email is on, reminders still appear in the in-app notification
center; emails are not queued while it is off. Set email up on
**Administration → Email** (under *System*): the SMTP server, port,
security, username, password (stored encrypted, never shown again), From and
Reply-to. For Exchange Online: `smtp.office365.com`, port 587, STARTTLS, and
the mailbox as the username. **Send test email** sends one message to the
address you type (your own by default) with the values on the page, without
saving them; the audit log records the result, not the address. **Save**
applies at once, without a restart.

The same values can be set in `[email]` in `config/pdms.toml` and
`secrets/smtp_password`; there they win and show read-only on the page.
- **SMTP AUTH:** the mailbox needs SMTP AUTH enabled in Exchange Online.
- **Relay connector:** if you relay through an Exchange connector or a local
  relay instead, leave the username empty and use the relay's host and
  port (security "None" only on a trusted network).

To run planning immediately (it runs every 15 minutes anyway):
`docker compose run --rm app run-scheduler`.

### Calendar and calendar subscriptions

**Calendar** shows reviews, renewals, and expirations by month or as an
agenda, filtered by department, type and status (tags and "owned by me" are
under **More filters**).
- **Dashed entries, marked "projected":** future review cycles worked out
  from the review interval.
- **Overdue items:** today shows how many there are as one **N overdue**
  link, which opens the overdue report.
- **A busy day** shows three items and **+N more**, which opens the agenda
  from that day.
- **On a phone** the month is a list of the days that have something on them.

**Calendar → Subscribe** creates a private calendar address for
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
  and Interlocal Agreement by default; change it per type), or tick the box for any document.
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

**Records requests.** Records officers use **Records requests** in the menu.
**New package** takes three numbered steps:
1. **Find documents:** search by title, ID or text. Only documents you may
   *export* appear; Restricted ones need an explicit grant.
2. **Choose documents:** tick the ones the request covers.
3. **Package options:** the **request reference** (for example
   `REQ-2026-014`), the files (current, every version, or none), whether to
   include each document's audit history, a note, and which details to
   include. Then **Build package**.

The package appears under **Your packages**, with a **Download** button. Its
SHA-256 checksum is shortened in the table; click it to see the full value.

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

**REST API.** Every user can create **API tokens** under **Your account** (the
link under your name in the sidebar):
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

## 10. Comments and file text

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
engine, OCR runs in a separate container (OCRmyPDF with Tesseract), which
the installer includes when you answer yes to OCR (`./install.sh
--configure ocr`). Then, on **Administration → Text extraction (OCR)**
(under *System*), tick **Read scanned PDFs and images** under **Settings**
and save; PDMS uses the container at `http://ocr:8080`. **Test** checks that
PDMS can reach it, and says so when the container isn't running. The same
panel turns reading text in files and date suggestions on and off. Changes
apply to files uploaded from then on, without a restart.
English is installed; other Tesseract languages can be added by extending
`deploy/ocr/Dockerfile` and setting `PDMS_OCR_LANGUAGES` (for example
`eng+spa`). Without OCR everything else works; scanned files simply have no
searchable text.

On installs set up before these pages, the installer wrote these settings
into `config/pdms.toml` (`[email]`, `[ocr] sidecar_url`, `[storage]
clamav_addr`); the upgrade offers to move them into Administration
(section 1). Until then they show read-only on the pages.

**The Outlook calendar sync was removed (D-087).** The calendar is PDMS's own
look-ahead. People who want the dates in Outlook subscribe to their calendar
feed (section 5), which needs no setup. If an earlier version kept events in
a shared mailbox:
- the upgrade stops the sync and forgets which events it made;
- the events it created stay in that mailbox until someone deletes them, so
  delete them, or the whole calendar, in Outlook;
- re-running `./install.sh` removes the `[graph_calendar]` settings and
  `secrets/graph_client_secret`;
- you can remove the `Calendars.ReadWrite` permission and the mailbox access
  policy from that app registration in Entra.

## 11. The audit log

**Audit log** in the sidebar, under *Manage* (Administrators and Auditors).

- Filter by action prefix (`auth.`, `acl.`, `department.`), category, outcome
  and date range; the user and object ID filters are under **More filters**.
  Each row shows who, when (in your time zone), how they signed in, from which
  IP, and a field-by-field before/after table. **Older entries** pages back
  through the log.
- **Export → CSV or JSON** (Auditors only). The export itself is logged. CSV cells
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
verifies the audit chain; it exits with code 2 if anything is wrong. Files
stored in SharePoint are checked through Microsoft Graph by size and
QuickXorHash; `--rehash-sharepoint` also downloads and re-hashes them (see
"SharePoint storage").

## 13. Configuration reference

See `config/pdms.example.toml` for every setting with its default. Any setting
can be overridden with an environment variable, `PDMS_<SECTION>__<KEY>` (for
example `PDMS_AUTH__SESSION_IDLE_MINUTES=20`). Append `_FILE` to read the value
from a file.
