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

Last updated 2026-10-21.

The product is now called **PADM**. The code, crates, binary, and Docker
images are still named `pdms`.

### 0.1 Done

- **Original phases 0–5:** built and tested (see the Status table in the
  README).
  - At commit `cbc9c70`, `cargo fmt --check` and `cargo clippy -D warnings`
    are clean, and `cargo test --workspace` gives 228 passed, 0 failed, and
    2 ignored.
  - On `main` on 2026-10-21, after P1, UI redesign R1 and P2, it gives 391
    passed, 0 failed, and 2 ignored.
  - The ignored ones are the opt-in real-OCR and real-ClamAV tests.
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
  [plan index](superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md).

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

### 0.1a Who is working on what

Two people work on PADM, often in turns, each with their own Claude session.
This table shows who has each piece of work and how far it has got, so
either person can see what is being done before starting anything. Open
pull requests (`gh pr list`, drafts included) show the branches in flight.

**Keep your row current.** This table changes only through small commits
pushed straight to `main` ("Working in turns" in [`CLAUDE.md`](../CLAUDE.md)),
never inside a pull request:

- **Before you start:** add your row.
- **Whenever a part changes state, and at the end of every session:**
  update the State and Updated columns. Mark each part **Not started**,
  **In progress**, **In review** (its PR is waiting for the owner) or
  **Done** (merged), with its PR number.
- **When the whole item is done:** add it to §0.1 with the date, and remove
  its row here.

| Work | Who | State | Branches and PRs | Updated |
|---|---|---|---|---|
| **UI redesign R2** (5b) | WACountyIT | Spec: **Not started**; the brief §9 questions go to the owner first. Plan: **Not started**. Build parts, all **Not started**: admin pages; calendar and feeds; appointments; records requests; notifications, audit and account; report detail | Branch `r2-spec` (spec); later parts on their own `r2-…` branches. PR titles start with `R2:` | 2026-10-22 |

Nothing else is claimed. §0.5 lists what can start next.

### 0.2 Direction (decided 2026-10-14)

- **SharePoint holds the files; PADM is the application.**
  - Every file is stored in one dedicated SharePoint site.
  - Files are organised by department and can be browsed read-only there.
  - Document details, workflow, reminders, appointments, permissions, audit,
    and search stay in PADM (D-081 to D-084, D-089).
- **Three visibility levels per document**, replacing today's "confidential"
  and "inherit permissions" switches (D-085):
  - Department (default);
  - County-wide (everyone in the county can view);
  - Restricted.
- **Users come from Entra ID.**
  - Their department is taken from the **Department field** on the Entra
    account; groups are not used, because AD groups are not synced to Entra.
  - Roles are assigned in PADM.
  - Sign-in is SSO (D-086).
- **The calendar is internal to PADM.** The optional Outlook calendar sync
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
| 1 | **SharePoint document storage** | SharePoint store on P2's Graph client (app-only, `Sites.Selected`); folder layout and metadata columns; downloads streamed through PADM; County-wide visibility; folder-level access; detecting and resolving changes made directly in SharePoint | **Planned**: spec approved; plans P1 (visibility, done), P3 (storage) and P4 (access, change detection, operations). P3 and P4 were written before P2 and R1 were built, so each needs a pre-flight check against `main` first (§0.5). Not started | [spec](superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md), [plan index](superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md) |
| 2 | **Entra user sync** | Nightly and on-demand `/users/delta` sync; Department-field mapping list; deactivate departed staff; SSO by Entra object ID only; guests excluded; certificate app-only Graph client (D-096) | **Done** (2026-10-21; plan P2, [PR #1](https://github.com/wacountyit/pdms/pull/1)). SharePoint folder access follows with P4 | [P2 plan](superpowers/plans/2026-10-14-p2-graph-client-entra-sync.md) |
| 3 | **Small changes** | Rename the ILA type; admin-curated tags (free-form off); remove the Outlook calendar sync (code, config, installer step, docs) | Not started. Needs a short plan. The owner does it, before R2 reaches the Tags and Outlook & OCR pages, and moves the OCR status to a new "Text extraction (OCR)" admin page (decided 2026-10-22) | D-087, D-088 (no separate spec needed) |
| 4 | **Bulk import** | CSV of document details plus a folder of files, with a dry run, into the SharePoint layout (see §5 "Next" item 1) | Not started; needs a spec. Depends on 1 | — |
| 5a | **UI redesign R1** | New app shell and visual style on every page; redesigned sign-in, dashboard, documents list, document page, county overview; branding admin page; username sign-in for local accounts | **Done** (2026-10-21). The owner moved R1 ahead of 1–3 so there is a shell to show leadership (D-092 to D-095) | [spec](superpowers/specs/2026-10-15-ui-redesign-r1-design.md), [plan](superpowers/plans/2026-10-15-ui-redesign-r1.md) |
| 5b | **UI redesign R2** | Page-by-page redesign of the remaining screens (admin, calendar, appointments, records requests, notifications, audit log, account, report detail) | **Designing**: the spec is drafted (2026-10-22) and waits for the owner's approval; it adds dark mode (the default, with a per-person switch), an activity feed and a browser-tab icon to the scope. On 2026-10-21 the owner started R2 in parallel with 1–3 instead of after them, so R2 coordinates with them (§0.6, brief §7) | [R2 brief](superpowers/specs/2026-10-21-ui-redesign-r2-brief.md), [R2 spec](superpowers/specs/2026-10-22-ui-redesign-r2-design.md) (draft); the plan comes next |
| — | **Security fixes** | Findings from the private 2026-10-14 review | Not started. Can run alongside 1–5. High and Medium items must be fixed before go-live. The owner holds the findings and hands them out | private |
| later | Public records (FOIA/PIA) requests | Build on the existing records-request packages | Idea | — |
| later | **First administrator account** | Change how the installer creates the first administrator | Idea: the owner will describe what they want | — |

**Waiting on the county** (needed before sub-projects 1–2 can be tested live;
the steps are in Appendix A of the spec):

- a `PADM Records – Test` SharePoint site, set up as described there;
- a correct **Department** value on every staff account, plus the list of
  distinct values in use;
- later, the `PADM Service` app registration and the site grant.

### 0.4 How work moves through this list

1. **Claim:** add your row to §0.1a and push that change straight to
   `main`, then open a draft pull request, so the other person can see what
   you have taken ([`CLAUDE.md`](../CLAUDE.md), "Working in turns"). Keep
   the row's State current on `main` as the work moves.
2. **Design:** a spec in `docs/superpowers/specs/`, reviewed and approved by
   the project owner.
3. **Plan:** an implementation plan in `docs/superpowers/plans/`.
4. **Build:** on a branch, with tests. Before handing over, run
   `cargo fmt`, `clippy -D warnings`, `cargo test`, and `cargo sqlx prepare`
   (see §6).
5. **Record:** in the same pull request:
   - update the status column above (§0.1a is kept current separately,
     on `main`);
   - update §0.5 and §0.6 if what can start next has changed;
   - add any new decisions to [DECISIONS.md](DECISIONS.md);
   - correct the README, ARCHITECTURE and ADMIN_GUIDE if behaviour changed.

   The owner merges. Then delete the branch, and mark that part **Done**
   in §0.1a. When the whole item is done, move it from §0.1a to §0.1.

Status values: Not started → Designing → Designed (spec approved) → Planned →
In progress → Done.

### 0.5 What can start next

Whoever is free takes the first unclaimed item below whose prerequisites are
met, unless the owner says otherwise. Claim it (§0.1a) before you start.

1. **UI redesign R2.** Claimed (§0.1a).
2. **Plan P3, SharePoint document storage.** It can start now, alongside R2.
   Nearly all of it is in `pdms-core`, and it needs no SharePoint site,
   because its tests use a mock Graph server. Only its final live test waits
   on the county.
   - **Pre-flight first.** The plan was written on 2026-10-15, before P2 and
     R1 were built.
     - Check every name it consumes against the code on `main`. The plan
       index lists what P2 changed.
     - Renumber the migrations. P2's is `0010` (the plan says `0009`). P3's
       takes the next free number on `main` and in open PRs: `0011` on
       2026-10-21 (the plan says `0010`).
     - Correct the plan text in the same branch, before Task 1.
3. **Plan P4, SharePoint access, change detection and operations.** It starts
   once P3 **and R2's admin pages** are merged. An approved R2 spec is not
   enough, because P4 and R2 both change the admin tab row in `base.html`,
   `Layout` and `admin_home` in `src/views.rs`, and `admin/entra.html`.
   - **Same pre-flight:** its migration takes the next free number after
     P3's (`0012` if nothing else takes one; the plan says `0011`), and its
     decisions take the next free numbers (the plan creates D-091 to D-097,
     but D-091 to D-096 are already taken).
4. **Sub-project 3, small changes** (D-087, D-088). It needs no spec; write a
   short plan first.
   - It changes two admin pages that R2 redesigns (`admin/tags.html` and
     `admin/outlook.html`), and the document forms.
   - The owner does it, before R2 reaches those pages (decided
     2026-10-22). It also moves the OCR status to its own "Text extraction
     (OCR)" admin page, which R2 then styles.
5. **Security fixes.** The owner holds the findings and hands them out one
   area at a time. High and Medium items must be fixed before go-live (§3).
6. **Sub-project 4, bulk import.** It needs a spec first, and it needs P3
   merged.

### 0.6 Which work touches which files

Two pieces of work that share a row can conflict. Check this table before
you claim something. If your work and claimed work share a row, agree the
order with the owner first. Paths are under `crates/pdms-web/` unless they
start with `crates/` or name a top-level folder.

| Files | R2 | P3 | P4 | Sub-project 3 |
|---|---|---|---|---|
| Admin pages: `templates/admin/*` and their handlers in `src/routes/` (`admin.rs`, `doc_admin.rs`, `schedule.rs`, `integrations.rs`, `collab.rs`, `branding.rs`) | Redesigns them; Branding gains the dark-mode accent check | No | Adds `admin/sharepoint.html` and its handler, and a section in `admin/entra.html` | Changes `admin/tags.html`. Removes the Outlook part of `admin/outlook.html` (`collab.rs`), and moves the OCR status to a new "Text extraction (OCR)" page |
| Shell: `templates/base.html`, `templates/partials/nav.html`, `static/app.css`, and `Layout` and `admin_home` in `src/views.rs` | The admin tab row becomes a grouped admin menu (`partials/admin_nav.html`, a `Layout.admin_page` field), new components, dark-mode tokens and the `data-theme` attribute, the dark-mode switch in the user block, the tab icon link | No | One admin tab and its `Layout` flag | The "Outlook & OCR" tab changes |
| Route table: `src/lib.rs`, `src/routes/mod.rs` | `/static/icon.svg` (the tab icon) and `POST /account/appearance` (the dark-mode switch) | No | A new route and module | Removes the Outlook sync routes |
| Document page and forms: `templates/documents/*`, `src/routes/documents.rs` | The R1 follow-ups on the document page (brief §6), the shared `rule_form.html`'s wrapper, the new and edit forms, and the workflow, signature and comment sections (R2 spec §6.8) | No templates. The upload and download code changes underneath | A notice on `documents/show.html`, and `ShowPage` in `documents.rs` | Tag entry on the forms. The ILA rename |
| Web errors and API: `src/error.rs`, `src/routes/api.rs` | No | A new 503 "Temporarily unavailable" response | No | No |
| Web tests: `crates/pdms-web/tests/` | `shell.rs`, and each page's tests | Possibly the 503 responses | `shell.rs`, and new SharePoint admin tests | `phase5.rs` (the Outlook & OCR page), and the tag tests |
| `crates/pdms-core`: documents, storage, OCR, exports, integrity | The activity feed's event list and query (a new module); the dark-mode accent derivation in `branding/color.rs` | Most of P3 | Most of P4 | Tags. Removing the Outlook sync |
| `crates/pdms-core/src/config.rs` and `maintenance.rs` (the worker's jobs) | No | `[sharepoint]` settings and the mirror jobs | Its jobs | Removes the Outlook settings and job |
| `migrations/` | One, in its last part: the per-person color scheme (the next free number when it is built) | The next free number | The next free number after P3's | Possibly one |
| `.sqlx/` | Yes (the activity feed, the color scheme, view data) | Yes | Yes | Likely |
| `config/`, `install.sh`, `docker-compose.yml` | No | `[sharepoint]` in the example config | The installer, and the Compose secret for the certificate | Removes the Outlook settings and its installer step |

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
| Confirm: an Entra-linked account that also has a local password is departed like any other when Entra disables it (D-096) | The P2 build | Nothing; it is built that way |
| Confirm: identity provider group mappings are not applied while the Entra sync is on (D-096) | The P2 build | Nothing; it is built that way |
| Two timing cases in how Department mapping changes and the sync interact were left as they are in the P2 build; the owner has the details. Fix them now, or keep the admin guide's advice (press **Full resync** after changing mappings during a sync)? | The P2 build | Nothing |
| How should the installer create the first administrator account? | The owner (2026-10-15) | That change |

---

## 1. Testing plan

### 1.1 Set up a test server

| Step | How |
|---|---|
| Server | A VM like production: Linux, 4 vCPU, 8 GB RAM (12 GB with OCR and ClamAV), 100 GB disk to start. |
| Install | `./install.sh`: choose the bundled Caddy proxy with a test DNS name (or your reverse proxy), turn on SMTP, OCR, and ClamAV so they get tested too. |
| Sign-in | Register a **test** Entra ID app (`./install.sh --configure sso`). Turn on the Entra ID user sync and map the Department values in use to PADM departments (Administration → Entra ID sync; see the admin guide). Assign roles in PADM. |
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
- [ ] Production Entra app registration with the production redirect URI; the Entra ID user sync on, with every Department value in use mapped to a PADM department; roles assigned in PADM.
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
| Settings | Most settings live in `config/pdms.toml` (installer or a text editor), not in an admin screen. | IT staff need shell access for some changes. | M |
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
7. **Admin settings screen** for the common settings (email, reminder defaults, OCR on/off), so routine changes need no shell access.

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
- **Database changes:** add a new migration file; never edit one that is on `main`. Take the next number after the highest on `main` **and** in open PRs. On 2026-10-21, `main` ends at `0010`. Plans P3 and P4 expect `0011` and `0012`, and renumber at their own pre-flight if other work takes those numbers first.
- **Decisions:** record anything a future maintainer would ask "why?" about in `docs/DECISIONS.md`. Take the next number after the highest on `main` and in open PRs. On 2026-10-21, `main` ends at D-096. Plan P4 expects D-097 onward and renumbers the same way.
- **Working with Claude:** [`CLAUDE.md`](../CLAUDE.md) at the repository root has these rules and the commands in one place.
- **Versions:** tag releases (`v1.0.0` for go-live), keep a `CHANGELOG.md`, and upgrade servers with `git pull && ./install.sh` after a backup.
