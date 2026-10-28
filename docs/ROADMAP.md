# Roadmap: testing, go-live, and what comes next

Every phase in the original specification (0–5) is built and covered by
automated tests, including the eight acceptance tests (§0.1 has the current
count). What
remains is to prove it in your environment with your people and documents,
close the gaps listed below, and decide which additions matter most.

This document is the plan for that: **1** test it, **2** pilot it, **3** go
live, **4** grow it. **Section 0 is the current status and the plan of
record**. Read it first if you are picking up the work.

---

## 0. Where things stand

Last updated 2026-10-28.

### 0.1 Done

- **Original phases 0–5:** built and tested (see the Status table in the
  README).
  - At commit `cbc9c70`, `cargo fmt --check` and `cargo clippy -D warnings`
    are clean, and `cargo test --workspace` gives 228 passed, 0 failed, and
    2 ignored.
  - On `main` on 2026-10-21, after P1, UI redesign R1 and P2, it gives 391
    passed, 0 failed, and 2 ignored.
  - On `main` on 2026-10-22, after UI redesign R2 (with sub-project 3), it
    gives 408 passed, 0 failed, and 2 ignored.
  - On `main` on 2026-10-22, after plan P3, it gives 455 passed, 0 failed,
    and 3 ignored.
  - On `main` on 2026-10-23, after plan P4, it gives 485 passed, 0
    failed, and 5 ignored.
  - The ignored ones are the opt-in real-OCR, real-ClamAV and (since P3 and
    P4) real-SharePoint tests.
- **Test deployment:** installed on the county's development server with
  `install.sh`.
  - It uses the bundled Caddy (HTTPS with an internal certificate), OCR,
    ClamAV, and the demo data.
  - Only the owner deploys to it (see "Good to know" in
    [`CLAUDE.md`](../CLAUDE.md)).
- **Security and code review** of the initial release, 2026-10-14. The
  findings are kept privately, **not in this public repository**; ask the
  project owner for them. Fixing the High and Medium findings is part of
  go-live (§3).
- **Design spec for sub-projects 1 and 2:**
  [2026-10-14-sharepoint-storage-entra-sync-design.md](superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md).
  It was approved on 2026-10-14. Step-by-step implementation plans P1–P4 were
  written on 2026-10-15; start with the
  [plan index](superpowers/plans/2026-10-14-pdms-sharepoint-entra-index.md).

- **UI redesign R1** (2026-10-21): sidebar shell and visual system on every
  page; redesigned sign-in, dashboard, documents list, document page, and
  county overview; branding admin page; usernames for local accounts
  (D-092 to D-095). The
  [admin guide](ADMIN_GUIDE.md) explains branding, usernames, and demo setup.
- **Plan P2, Entra user sync** (2026-10-21, merged in
  [PR #1](https://github.com/wacountyit/pdms/pull/1)):
  - the certificate-based Graph client;
  - the nightly and on-demand Entra sync, with Department mapping and
    departures;
  - SSO by Entra object ID only;
  - the Administration → Entra ID sync page.

  It adds migration `0010` and decision D-096. It is installed on the test
  server with the sync off; the admin guide's "Entra ID user sync" section
  explains how to turn it on.
- **UI redesign R2** (2026-10-22, by WACountyIT; [PR #2](https://github.com/wacountyit/pdms/pull/2)
  spec, [PR #3](https://github.com/wacountyit/pdms/pull/3) R1 fixes,
  [PR #4](https://github.com/wacountyit/pdms/pull/4) the build):
  - every remaining screen redesigned, including the document forms;
  - the grouped admin menu and one admin page pattern (P4 builds on both);
  - dark mode as the default, with a per-person switch;
  - the dashboard's activity feed;
  - the browser-tab icon;
  - the county overview's counts corrected.

  It adds migrations `0011` and `0012` and decisions D-097 to D-101. The
  [R2 spec](superpowers/specs/2026-10-22-ui-redesign-r2-design.md) and
  [plan](superpowers/plans/2026-10-22-ui-redesign-r2.md) describe it.
- **Sub-project 3, small changes** (2026-10-22, built inside R2, D-087 and
  D-088 done):
  - "Partnership Agreement" renamed to Interlocal Agreement (ILA);
  - admin-curated tags;
  - the Outlook calendar sync removed;
  - the OCR status on its own admin page.
- **Plan P3, SharePoint document storage** (2026-10-22, by WACountyIT;
  [PR #5](https://github.com/wacountyit/pdms/pull/5) the build,
  [PR #7](https://github.com/wacountyit/pdms/pull/7) the example config,
  live test and docs):
  - `[storage] backend = "sharepoint"` keeps document files in one SharePoint
    library, set up on first use;
  - uploads checked by QuickXorHash, and downloads, text extraction, records
    packages and restores checked against the recorded SHA-256;
  - the mirror job for file names, library columns and folder placement,
    with a nightly sweep;
  - changes that narrow access move the folder first, and folder moves are
    audited;
  - a SharePoint outage keeps the person's form, with a message.

  It adds migration `0013` and decisions D-102 and D-103. It is off unless
  configured: the admin guide's "SharePoint storage" section explains turning
  it on (start with an empty system). Folder permissions and change
  detection follow in P4.
- **Installer:** Caddy can use other ports when 80 or 443 is taken
  ([PR #6](https://github.com/wacountyit/pdms/pull/6)), and the exact Entra
  redirect URI is shown ([PR #8](https://github.com/wacountyit/pdms/pull/8)).
- **Plan P4, SharePoint access, change detection and operations**
  (2026-10-23, by WACountyIT, built directly on `main`):
  - department staff get read access on their `Departments/` folder, kept up
    to date after membership changes, the Entra sync, and nightly;
  - a change check every few minutes flags files edited or deleted directly
    in SharePoint and lists files PDMS doesn't track; administrators are
    notified;
  - Administration → SharePoint to restore the original, accept a change as
    a new version, re-check, or dismiss; a notice on the document page;
  - `verify-integrity` checks SharePoint files (`--rehash-sharepoint`
    downloads them);
  - `./install.sh --configure sharepoint`, the Compose certificate secret,
    and [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md) for the site and app
    registration.

  It adds migration `0014` and decisions D-104 to D-110. The live test on a
  real SharePoint site is the next step (§0.5).
- **S1, settings in Administration** (2026-10-23, by WACountyIT, built
  directly on `main` in seven phases):
  - Microsoft sign-in, email, the Microsoft 365 connection (with the
    "PDMS Service" certificate made in PDMS), SharePoint storage, the Entra
    ID user sync, OCR and virus scanning are set up, tested and changed on
    admin pages, and a save applies at once in both processes, without a
    restart;
  - values in `config/pdms.toml` or `PDMS_*` variables still win and show
    read-only; secrets are stored encrypted with the master key;
  - the rules that keep files readable (SharePoint on only while no file is
    stored locally, the site fixed while SharePoint holds files) and the
    sign-in lockout rule;
  - `pdms settings set|set-secret|test|import` as the fallback;
  - the installer reduced to first-install steps, and an upgrade offers once
    to move the settings an earlier installer wrote into Administration.
- **Fix after S1** (2026-10-26, commit `dc92d92`): **Test** and **Save** on
  Administration → SharePoint keep the site and library set in
  `config/pdms.toml`. The page shows those fields disabled, browsers don't
  send disabled fields, and Test then failed with "enter the site's full
  address". 541 passed, 0 failed, 5 ignored.
- **SharePoint live-test follow-ups** (2026-10-26, by WACountyIT): the four
  setup-flow problems the first live test found. **Test** now checks that
  the PDMS Service app has full control of the site (only full control may
  list the site's permissions), so a `write` grant fails Test and turning
  storage on, instead of the first upload. With no grant, Test says the
  grant is missing instead of "Libraries found: none". After a passing Test
  the page no longer asks to turn storage on when it is already on.
  SHAREPOINT_SETUP.md B4b is corrected, B5 has the Graph Explorer steps, and
  C2 lists the new messages. 544 passed, 0 failed, 5 ignored.

  It adds migration `0015` and decisions D-111 to D-116. On `main` on
  2026-10-23 it gives 540 passed, 0 failed, and 5 ignored. The
  [spec](superpowers/specs/2026-10-23-admin-settings-design.md) and
  [plan](superpowers/plans/2026-10-23-admin-settings.md) describe it.
- **Upgrading after S1:** `git pull && ./install.sh` as usual. It offers to
  move the sign-in, email, SharePoint, OCR and virus-scanning settings into
  Administration (back up first: they then live in the database, encrypted
  with the master key). Answer yes on the test server, so the SharePoint
  live test is set up from the admin pages.
- **Upgrading after P4:** run `./install.sh` after `git pull`, as usual. It
  creates `secrets/graph_certificate` (empty unless Microsoft Graph is set
  up), which Docker Compose now expects; `docker compose up` alone stops with
  "secret file not found" until it exists.
- **Before upgrading the test server to this `main`:** back it up, and read
  the admin guide's note "The Outlook calendar sync was removed". Everyone
  sees dark mode until they switch (Your account → Appearance).

### 0.1a Who is working on what

Two people work on PDMS, often in turns, each with their own Claude session.
This table shows who has each piece of work and how far it has got, so
either person can see what is being done before starting anything.
Everyone works directly on `main` (since 2026-10-23), so this table is the
only place that shows work in flight.

**Keep your row current** ("Working in turns" in [`CLAUDE.md`](../CLAUDE.md)):

- **Before you start:** add your row, and push it straight away.
- **Whenever a part changes state, and at the end of every session:**
  update the State and Updated columns. Mark each part **Not started**,
  **In progress** or **Done**, and when you stop partway, add a short
  "Where I stopped" note.
- **When the whole item is done:** add it to §0.1 with the date, and remove
  its row here.

| Work | Who | State | Where I stopped | Updated |
|---|---|---|---|---|
| §0.5 item 1: SharePoint live test, continued | WACountyIT | In progress (2026-10-27): upgrade and Test with the full-control check; then B6 to B9 and §1.2 | | 2026-10-27 |

§0.5 lists what can start next.

### 0.2 Direction (decided 2026-10-14)

- **SharePoint holds the files; PDMS is the application.**
  - Every file is stored in one dedicated SharePoint site.
  - Files are organised by department and can be browsed read-only there.
  - Document details, workflow, reminders, appointments, permissions, audit,
    and search stay in PDMS (D-081 to D-084, D-089).
- **Three visibility levels per document**, replacing today's "confidential"
  and "inherit permissions" switches (D-085):
  - Department (default);
  - County-wide (everyone in the county can view);
  - Restricted.
- **Users come from Entra ID.**
  - Their department is taken from the **Department field** on the Entra
    account; groups are not used, because AD groups are not synced to Entra.
  - Roles are assigned in PDMS.
  - Sign-in is SSO (D-086).
- **The calendar is internal to PDMS.** The optional Outlook calendar sync
  (Phase 5) is removed (D-087).
- **Document types and tags** (D-088):
  - "Partnership Agreement" becomes **Interlocal Agreement (ILA)**;
  - admins can still add their own types;
  - tags are maintained by admins.

### 0.3 Sub-projects

Each one gets its own spec and plan. §0.5 sets the order in which they are
taken up.

| # | Sub-project | Scope | Status | Spec / plan |
|---|---|---|---|---|
| 1a | **Document visibility** (plan P1) | Department / County-wide / Restricted | **Done** | [P1 plan](superpowers/plans/2026-10-14-p1-document-visibility.md) |
| 1b | **Visibility follow-ups** | Decided 2026-10-15: calendars show what you can see (D-090); County-wide comments owner/admin only, authors-only deletes (D-091) | **Done** | D-090, D-091 |
| 1 | **SharePoint document storage** | SharePoint store on P2's Graph client (app-only, `Sites.Selected`); folder layout and metadata columns; downloads streamed through PDMS; County-wide visibility; folder-level access; detecting and resolving changes made directly in SharePoint | **Built; live test under way** (P1, P3 and P4 done; P4 finished 2026-10-23: migration `0014`, D-104 to D-110). On 2026-10-26 the test server connected to a real SharePoint test site: **Test** passes and the first document upload landed in SharePoint. The four setup-flow fixes it found are done (§0.1). Still to do: the rest of §1.2 ([SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md), "Rollout on the test server") | [spec](superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md), [plan index](superpowers/plans/2026-10-14-pdms-sharepoint-entra-index.md) |
| 2 | **Entra user sync** | Nightly and on-demand `/users/delta` sync; Department-field mapping list; deactivate departed staff; SSO by Entra object ID only; guests excluded; certificate app-only Graph client (D-096) | **Done** (2026-10-21; plan P2, [PR #1](https://github.com/wacountyit/pdms/pull/1)). SharePoint folder access built with P4 (D-104) | [P2 plan](superpowers/plans/2026-10-14-p2-graph-client-entra-sync.md) |
| 3 | **Small changes** | Rename the ILA type; admin-curated tags (free-form off); remove the Outlook calendar sync (code, config, installer step, docs) | **Done** (2026-10-22, built inside R2, [PR #4](https://github.com/wacountyit/pdms/pull/4); migration `0011`). The OCR status moved to a "Text extraction (OCR)" admin page | D-087, D-088; [R2 plan](superpowers/plans/2026-10-22-ui-redesign-r2.md) part 4 |
| 4 | **Bulk import** | CSV of document details plus a folder of files, with a dry run, into the SharePoint layout (see §5 "Next" item 1) | Not started; needs a spec. Depends on 1 | — |
| 5a | **UI redesign R1** | New app shell and visual style on every page; redesigned sign-in, dashboard, documents list, document page, county overview; branding admin page; username sign-in for local accounts | **Done** (2026-10-21). The owner moved R1 ahead of 1–3 so there is a shell to show leadership (D-092 to D-095) | [spec](superpowers/specs/2026-10-15-ui-redesign-r1-design.md), [plan](superpowers/plans/2026-10-15-ui-redesign-r1.md) |
| 5b | **UI redesign R2** | Page-by-page redesign of the remaining screens (admin, calendar, appointments, records requests, notifications, audit log, account, report detail), plus the document forms and sections, dark mode (the default, with a per-person switch), an activity feed and a browser-tab icon | **Done** (2026-10-22; [PR #4](https://github.com/wacountyit/pdms/pull/4); D-097 to D-101; migrations `0011` and `0012`) | [R2 brief](superpowers/specs/2026-10-21-ui-redesign-r2-brief.md), [R2 spec](superpowers/specs/2026-10-22-ui-redesign-r2-design.md), [R2 plan](superpowers/plans/2026-10-22-ui-redesign-r2.md) |
| S1 | **Settings in Administration** | Set up, change, test and check Microsoft sign-in, email, the Microsoft 365 connection (with the Graph certificate made in PDMS), SharePoint storage, the Entra ID user sync, OCR and virus scanning on admin pages instead of `./install.sh --configure`; the installer is for the first installation | **Done** (2026-10-23; migration `0015`, D-111 to D-116) | [spec](superpowers/specs/2026-10-23-admin-settings-design.md), [plan](superpowers/plans/2026-10-23-admin-settings.md) |
| GS | **Entra sync limited to one group** | When a group is set, only its members (nested groups count) get PDMS accounts; people removed from it are departed; sign-in refused for non-members. Lets IT control who uses PDMS with one Entra group (cloud, or an AD group synced to Entra). Amends D-086 and D-096 | **Designed:** spec approved 2026-10-28. The owner's answers are in its §9: people back in the group are reactivated automatically; a refused sign-in says why; the P2 mapping race is fixed here. Next: the plan (§0.5) | [spec](superpowers/specs/2026-10-27-entra-sync-group-scope-design.md) |
| — | **Security fixes** | Findings from the private 2026-10-14 review | Not started. Can run alongside 1–5. High and Medium items must be fixed before go-live. The owner holds the findings and hands them out | private |
| later | Public records (FOIA/PIA) requests | Build on the existing records-request packages | Idea | — |
| — | **First administrator account** | Change how the installer creates the first administrator | Decided 2026-10-28: email becomes optional for the break-glass administrator the installer creates. Not started; needs a short spec, because every account needs an email today (`users.email` is required and unique) | — |

**Waiting on the county** (needed before sub-projects 1–2 can be tested live;
the steps are in [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md)):

- ~~a SharePoint test site, set up as Part A describes~~: ready 2026-10-26;
- a correct **Department** value on every staff account, plus the list of
  distinct values in use (A7);
- ~~the `PDMS Service` app registration and the one-site grant (Part B)~~:
  ready 2026-10-26, with the grant at `fullcontrol`.

### 0.4 How work moves through this list

1. **Claim:** add your row to §0.1a and push it to `main` straight away,
   so the other person can see what you have taken
   ([`CLAUDE.md`](../CLAUDE.md), "Working in turns"). Keep the row's State
   current as the work moves.
2. **Design:** a spec in `docs/superpowers/specs/`, reviewed and approved by
   the project owner.
3. **Plan:** an implementation plan in `docs/superpowers/plans/`.
4. **Build:** directly on `main`, in small commits that each build and pass
   the tests. Before each push, run `cargo fmt`, `clippy -D warnings`,
   `cargo test`, and `cargo sqlx prepare` (see §6).
5. **Record:** in the same commits:
   - update the status column above and your §0.1a row;
   - update §0.5 and §0.6 if what can start next has changed;
   - add any new decisions to [DECISIONS.md](DECISIONS.md);
   - correct the README, ARCHITECTURE and ADMIN_GUIDE if behaviour changed.

   When the whole item is done, move it from §0.1a to §0.1.

Status values: Not started → Designing → Designed (spec approved) → Planned →
In progress → Done.

### 0.5 What can start next

Whoever is free takes the first unclaimed item below whose prerequisites are
met, unless the owner says otherwise. Claim it (§0.1a) before you start.

1. **SharePoint live test, continued.** The site, the app and the grant are
   ready, the first upload works, and the setup-flow fixes it found are in
   (§0.1). First, after upgrading the test server, choose **Test**: it
   should say "PDMS has full control of the site". The new check relies on
   an app with a `fullcontrol` site grant being allowed to list the site's
   permissions, which only the real tenant can confirm; if Test says the
   grant is below full control although it is `fullcontrol`, report it.
   Then work through [SHAREPOINT_SETUP.md](SHAREPOINT_SETUP.md) B6 to B9
   and §1.2. Set
   sub-projects 1 and 2 to **Done** when they pass.
2. **Security fixes.** The owner holds the findings and hands them out one
   area at a time. High and Medium items must be fixed before go-live (§3).
3. **Sub-project 4, bulk import.** It needs a spec first. P3, which it
   imports into, is merged.
4. **GS, Entra sync limited to one group.** The
   [spec](superpowers/specs/2026-10-27-entra-sync-group-scope-design.md)
   was approved on 2026-10-28; write the plan, then build. It changes the
   Entra sync (`directory/entra_sync.rs`, `entra_values.rs`),
   `auth/oidc.rs`, `settings.rs`, and the Entra ID sync admin page. If the
   live test (item 1) needs fixes in the same code, agree the order with the
   owner.
5. **First administrator account.** Email becomes optional for the
   break-glass administrator (§0.3). It needs a short spec first.

### 0.6 Which work touches which files

Two pieces of work that share a row can conflict. When you claim something,
add a column for it with what it changes in each row, and check the other
columns first. If your work and claimed work share a row, agree the order
with the owner first. (P3, P4 and S1 are done, so their columns were removed
on 2026-10-23; nothing else is claimed.) Paths are under `crates/pdms-web/`
unless they start with `crates/` or name a top-level folder.

| Files | (no claimed work) |
| --- | --- |
| Admin pages: `templates/admin/*` and their handlers in `src/routes/` (`admin.rs`, `doc_admin.rs`, `schedule.rs`, `integrations.rs`, `collab.rs`, `branding.rs`, `sharepoint_admin.rs`, `microsoft365.rs`, `sign_in.rs`, `system_settings.rs`) | |
| Shell: `templates/base.html`, `templates/partials/nav.html`, `static/app.css`, and `Layout` and `admin_home` in `src/views.rs` | |
| Route table: `src/lib.rs`, `src/routes/mod.rs` | |
| Document page and forms: `templates/documents/*`, `src/routes/documents.rs` | |
| Web errors and API: `src/error.rs`, `src/routes/api.rs` | |
| Web tests: `crates/pdms-web/tests/` | |
| `crates/pdms-core`: documents, storage, OCR, exports, integrity, `sharepoint/` | |
| `crates/pdms-core/src/config.rs`, `settings.rs`, `settings_import.rs` and `maintenance.rs` (the worker's jobs) | |
| `migrations/` | |
| `.sqlx/` | |
| `config/`, `install.sh`, `docker-compose.yml` | |

- **Everyone edits** ROADMAP §0, [DECISIONS.md](DECISIONS.md), the README
  and ADMIN_GUIDE. In this table, change only your own column. Take
  migration and decision numbers as `CLAUDE.md` says.
- **Security fixes** can touch any row. Before you refactor a route or
  template, ask the owner whether a fix is planned there.

### 0.7 Questions waiting for the owner

When the owner answers one, record the answer where it belongs (in
DECISIONS, or in the spec or plan) and remove the question from this table.
A question that blocks you goes to the owner straight away ("Working in
turns" in `CLAUDE.md`); this table is for the ones that can wait.

| Question | Raised by | What it holds up |
|---|---|---|
| (none: the owner answered every waiting question on 2026-10-28) | | |

---

## 1. Testing plan

### 1.1 Set up a test server

| Step | How |
|---|---|
| Server | A VM like production: Linux, 4 vCPU, 8 GB RAM (12 GB with OCR and ClamAV), 100 GB disk to start. |
| Install | `./install.sh`: choose the bundled Caddy proxy with a test DNS name (or your reverse proxy), and include the OCR service and the virus scanner. Then set up email, OCR and virus scanning under Administration, so they get tested too. |
| Sign-in | Register a **test** Entra ID app and set it up under Administration → Microsoft 365 → Sign-in. Turn on the Entra ID user sync and map the Department values in use to PDMS departments (Administration → Entra ID sync; see the admin guide). Assign roles in PDMS. |
| Data | Two options, ideally both: `docker compose run --rm app seed-demo` on a throwaway install (sample documents, boards, users), and a copy of real documents from one department on the real test install. |
| Test accounts | One person per role: administrator, auditor, records officer, legal counsel, department head, appointments clerk, an ordinary staff member in each of two departments, and someone with no department. |
| Backups | Run `deploy/backup/pdms-backup.sh` nightly on the test server from day one. |

### 1.2 What to test

Work through each area; tick it off when it behaves as expected for **every
role** that should (and should not) be able to do it. A denied action
should show "You do not have permission" and appear under **Reports →
Denied access attempts**.

**Sign-in and access**
- [ ] With the Entra ID sync on, a new staff member is created with the department from their Entra **Department** field before their first sign-in; changing the field in Entra changes their department after the next sync; disabling the account marks them departed.
- [ ] Break-glass administrator signs in with password + authenticator code; five wrong passwords lock it temporarily.
- [ ] A new staff member sees only their department's documents plus County-wide ones (search, calendar, dashboard, ICS feed, API, direct links all agree).
- [ ] A County-wide document is visible to every active account; changing it to Restricted hides it on the next click.
- [ ] Sharing one document with another department exposes only that document; revoking a grant takes effect on the next click.
- [ ] Restricted documents are invisible to everyone without an explicit grant, including administrators, exports, and Outlook.

**Documents**
- [ ] Create each document type with its custom fields; upload PDFs, Word files, scans, images; uploads of other file types are refused.
- [ ] Edit details, transfer ownership, add versions (revision, amendment, signed copy), restore an old version; history shows every change with before/after values.
- [ ] Search by title, ID, tag, custom field, and **words inside the file**; export results to CSV.
- [ ] Retention: deletion is blocked while retention or a legal hold applies; deleted documents can be restored by an administrator.

**Dates, reminders, calendar**
- [ ] Review/renewal/expiration dates produce tasks, statuses (Renewal Due, Overdue, Expired), and reminders at 90/30/7/0 days, then weekly while overdue.
- [ ] Emails arrive through your Exchange/SMTP setup; the in-app notification count matches.
- [ ] The partnership-agreement scenario (2013 original, 2014 amendment, five-year review, never reviewed) shows Overdue with 2 missed cycles.
- [ ] Calendar month/agenda views; subscribe in Outlook (ICS); revoke the subscription.
- [ ] ~~Optional: Outlook calendar sync~~: being removed (D-087); skip.

**Workflow, signatures, appointments**
- [ ] Submit a contract: reviewers → Legal counsel → approver; each is notified; rejection and "request changes" return it; the submitter cannot approve their own submission.
- [ ] Track signatories; record a signature with the signed copy.
- [ ] Boards: create positions with requirement checklists, appoint, reappoint, replace; term-end and reappointment reminders reach the manager and appointments clerks.

**Comments and dates found in files**
- [ ] Comment, reply, @mention (the person is notified); edit and delete keep the original text in History.
- [ ] Upload a contract and a scanned agreement: dates found are listed for confirmation and **nothing changes until "Use this date"**. Note how often the suggestions are right; this guides tuning.

**Records requests, reports, API, integrations**
- [ ] A records officer builds a package for a request number; the ZIP's `SHA256SUMS` checks out (`sha256sum -c SHA256SUMS`); the export shows on each document's history.
- [ ] Every report matches what you expect for a few known documents; CSV opens cleanly in Excel.
- [ ] Create an API token with read scope; `GET /api/v1/documents` returns only what that user can see; revoking the token stops it.
- [ ] A webhook to a test endpoint (e.g. a temporary request bin on your network) receives signed events.

**Settings in Administration**
- [ ] Each settings page's **Test** reports a working setup as OK and a wrong value (tenant, site, SMTP server, a stopped OCR or ClamAV container) as a problem in plain words, and changes nothing.
- [ ] A save applies at once in the web server and the worker, without a restart (for example, a test email after changing the SMTP settings; a new reminder email after turning email on).
- [ ] A value in `config/pdms.toml` shows read-only on its page; deleting it there and restarting makes it editable.
- [ ] The upgrade offer moves the old settings: afterwards everything works as before, and `docker compose run --rm app settings show` lists them as `database`.
- [ ] Renewing the Microsoft 365 certificate on the Connection page (make, upload, **Use this certificate**) has no outage.

**Audit and operations**
- [ ] The audit log shows every action above with who/when/what; **Verify** reports the chain intact; CSV/JSON export works.
- [ ] Backup and **restore drill** on the test server (docs/BACKUP.md): restore ends with "Integrity verified: no problems found."
- [ ] Upgrade drill: install the next build over the test server with `git pull && ./install.sh`.
- [ ] Accessibility spot check: keyboard-only navigation, a screen reader on the document page, 200% zoom.

### 1.3 Recording what you find

For each problem, note: what you did (steps), what you expected, what
happened, the time, the user, and a screenshot. Two things make problems much
faster to fix:
- the **request ID** (`x-request-id` response header, also in `docker compose logs app`), and
- the **audit sequence number** of the action (Audit log).

Suggested severity: **Blocker** (data wrong, access wrong, nothing works) ·
**Major** (a feature fails, workaround exists) · **Minor** (wording, layout) ·
**Idea** (works as designed, could be better). Access-control or audit
problems are always Blockers.

---

## 2. Pilot

Pick one department and one board, for example Operations's partnership
agreements and contracts plus one appointed board, for 4–6 weeks.

| Before | During | Exit criteria |
|---|---|---|
| Import that department's current documents (see "Bulk import" below; a few dozen can be entered by hand) and set real dates and owners. Short training (30 minutes) for the department and legal counsel's office. | Weekly 15-minute check-in: problems, confusing screens, missing fields. Watch the reminder emails: right people, right time, not too many. | No open Blockers or Majors; reminders and approvals ran on real documents; a backup restore drill passed on production-like data; users would rather use it than the old way. |

---

## 3. Go-live checklist

**Infrastructure**
- [ ] Production VM sized for growth (files grow fastest: plan for several years of scans).
- [ ] DNS name and TLS (bundled Caddy with your domain, or your reverse proxy).
- [ ] Host firewall: only 443 (and 80 for redirects/certificates) from users; the app port only from the proxy.
- [ ] Time synchronization (NTP) on the host: reminders and audit timestamps depend on it.

**Identity and email**
- [ ] Production Entra app registration with the production redirect URI; the Entra ID user sync on, with every Department value in use mapped to a PDMS department; roles assigned in PDMS.
- [ ] Break-glass administrator password and authenticator secret stored in the IT vault; test it quarterly.
- [ ] SMTP through Exchange Online (a dedicated mailbox, or a connector) with SPF/DKIM so reminders are not marked as spam.

**Records and legal**
- [ ] Records officer reviews **retention policies** against your records retention schedules and assigns them to document types.
- [ ] Legal counsel confirms the records request package contents and the audit-history option fit how requests are answered.
- [ ] Decide who holds Legal Counsel, Records Officer, Auditor, and Appointments Clerk.

**Protection and monitoring**
- [ ] Backups: a pre-script (or scheduled job) runs `deploy/backup/pdms-backup.sh` into a protected directory nightly; first **restore drill** done and recorded.
- [ ] Monitoring: `GET /healthz` and `/readyz` checked by your monitoring system; `/metrics` scraped by Prometheus if you have one; disk space alerts.
- [ ] Audit forwarding to your SIEM or log platform (`[audit_forward]`), if you have one.
- [ ] Security review: an internal or third-party penetration test of the test server; run `cargo audit` and a container image scan; review the admin and API surfaces.
- [ ] The findings of the private security review are fixed; the owner tracks them.

**License**
- [ ] Publish the source (your repository, including local changes) and set `[org] source_url` (`./install.sh --configure source`); check that the **Source code** link in the footer works for users (AGPL-3.0 section 13).

**People**
- [ ] One-page quick guides per role (owner, approver, records officer, appointments clerk).
- [ ] Support path: who users contact, and who can read logs and the audit log.

---

## 4. Known gaps (honest list)

Things that work but are limited, or were deliberately left for later:

| Area | Gap | Why it matters | Effort |
|---|---|---|---|
| Migration | **No bulk import tool.** Documents are created one at a time (or via the API without files). | Go-live with hundreds of existing documents needs it. Planned as sub-project 4 (§0.3). | L |
| Identity | With the Entra ID sync on (off by default), departed staff are marked departed at the next nightly sync, but the sync never reactivates anyone and holds back a mass departure for an administrator to check. With it off, department/role changes apply at the next sign-in and offboarding is manual (sub-project 2, D-086, D-096). | Turn the sync on after setting up the app registration (Administrator guide). | S |
| Appointments | Reminder rules per position exist in the engine but have no screen (organization defaults are editable). | Boards with unusual timing need the admin to edit defaults. | S |
| API | Read-mostly: no file upload, edits, workflow, or appointment changes through the API. | Integrations that write back (e.g. a records platform) need more. | M |
| Workflow | Fixed stage order (reviewers → legal → approvers); no conditional routing or delegation while someone is away. | Some approvals need a different order or a backup approver. | M |
| Signatures | Tracking only; no e-signature service connected (the provider interface is ready). | Signed copies are uploaded by hand. | M |
| Testing | Browser end-to-end tests (Playwright) from the original plan were not built; the web is tested at the HTTP level. | Catches front-end regressions earlier. | M |
| Scaling | Login and API rate limits live in memory per app instance; one app instance is assumed. | Fine for a single organization; matters only for multiple app instances. | M |
| OCR | English only; date suggestions are keyword-based and miss unusual wording. | Spanish documents, odd contract language. | S–M |
| Settings | Sign-in, email, Microsoft 365, SharePoint, the Entra sync, OCR and virus scanning are set up in Administration (S1). The time zone, upload size limit, API and rate limits, S3 storage and audit forwarding are still set in `config/pdms.toml`, and the address, ports and database in the installer. | Rarely changed, but they need shell access; the same pattern can bring them to the pages later. | S |
| SharePoint setup | **`sharepoint.site_url` in `config/pdms.toml` must be the site's root address** (`https://<tenant>.sharepoint.com/sites/<Name>`). A copied link such as `…/sites/<Name>/Shared Documents/Forms` fails the configuration check, and then every command fails, so `migrate` exits 1 and the app never starts; `docker compose logs migrate` shows the error. The message doesn't show which part of the address is wrong. | Easy to get wrong by pasting a browser link. | S |
| SharePoint storage | **Orphans are listed, not removed.** If a save fails after its file was uploaded, the file stays in SharePoint; the change check lists it under "Files PDMS does not track" after ten minutes, and an administrator deletes it there (D-102, D-106). A folder moved by a change that then failed is put back by the next mirror run. | An administrator has to clean up the rare failed save by hand. | S |
| SharePoint storage | **No migration between storage backends.** Turning SharePoint on doesn't move files already stored locally or in S3, and they can't be read afterwards. | Turn it on before real documents are added; the test server is reset when it is switched (D-102). | M |
| Audit | No archival of old audit entries (nothing is purged; forwarding gives an external copy). | Only matters after many years of heavy use. | S |

---

## 5. Future additions

Ordered by likely value; adjust after the pilot. Where these overlap the sub-projects in §0.3, §0.3 takes precedence.

### Next (after testing feedback)

1. **Bulk import.** A CSV of document details plus a folder of files, with a dry run that reports problems before anything is created, and an audit entry per document. Essential for go-live. **Now sub-project 4 (§0.3).**
2. **Fixes and polish from testing.** This always comes first once testing starts.
3. ~~**Entra lifecycle sync** through Microsoft Graph: nightly group-membership sync and automatic deactivation of disabled Entra accounts.~~ **Replaced by sub-project 2 (§0.3), done**: sync from the Department field, not groups (D-086, D-096).
4. **Position reminder rules screen**, and per-user notification preferences (daily digest instead of individual emails).
5. **Saved searches and "watch this document"**, so people can follow documents they do not own.
6. **Release engineering:** versioned releases and a changelog, a published container image, `cargo audit`/`cargo deny` and an image scan in CI, and Playwright smoke tests.
7. ~~**Admin settings screen** for the common settings (email, reminder defaults, OCR on/off), so routine changes need no shell access.~~ **Done by S1** (2026-10-23), except reminder defaults, which were already under Administration → Notification rules.

### Later

- **Records retention disposition:** a "due for destruction" report, approval by the records officer, and a destruction certificate with the audit trail, matching your retention schedules.
- **Connectors** built on the integration provider interface: an ECM system (file approved documents into the repository) and a records-request portal to attach export packages to requests.
- **E-signature** provider (DocuSign or Adobe Acrobat Sign) using the existing signature interface: send for signature, record completion automatically, store the signed copy.
- **Microsoft Teams** notifications (approval requests and overdue reminders in a channel or chat).
- **Board meeting link:** connect approvals and appointments to board resolutions and minutes.
- **Version comparison** (what changed between two versions of a policy) and document templates.
- **High availability:** multiple app instances behind a load balancer with shared rate limiting, plus S3-compatible object storage with object lock (WORM) for files.
- **More OCR languages** and better date recognition, tuned from the pilot's accepted/dismissed suggestions.

### Ideas to evaluate

- A public page listing adopted policies (read-only, explicitly published documents only).
- AI-assisted summaries or clause finding, only with an approved model hosted in your environment, shown as suggestions like the dates, never applied automatically.
- A phone-friendly layout for approvers on the go.

---

## 6. Making changes

- **Code:** Rust workspace; see README "Develop". Before handing over a change: `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, and after query or migration changes `cargo sqlx prepare --workspace -- --all-targets`.
- **Database changes:** add a new migration file; never edit one that has been pushed to `main`. Pull first, then take the next number after the highest in `migrations/`. On 2026-10-26, `main` ends at `0015`, so the next is `0016`.
- **Decisions:** record anything a future maintainer would ask "why?" about in `docs/DECISIONS.md`. Pull first, then take the next number after the highest there. On 2026-10-26, `main` ends at D-116, so the next is D-117.
- **Working with Claude:** [`CLAUDE.md`](../CLAUDE.md) at the repository root has these rules and the commands in one place.
- **Versions:** tag releases (`v1.0.0` for go-live), keep a `CHANGELOG.md`, and upgrade servers with `git pull && ./install.sh` after a backup.
