# Roadmap: testing, go-live, and what comes next

Every phase in the original specification (0–5) is built and covered by
automated tests (about 230 tests, including the eight acceptance tests). What
remains is to prove it in your environment with your people and documents,
close the gaps listed below, and decide which additions matter most.

This document is the plan for that: **1** test it, **2** pilot it, **3** go
live, **4** grow it.

---

## 1. Testing plan

### 1.1 Set up a test server

| Step | How |
|---|---|
| Server | A VM like production: Linux, 4 vCPU, 8 GB RAM (12 GB with OCR and ClamAV), 100 GB disk to start. |
| Install | `./install.sh`: choose the bundled Caddy proxy with a test DNS name (or your reverse proxy), turn on SMTP, OCR, and ClamAV so they get tested too. |
| Sign-in | Register a **test** Entra ID app (`./install.sh --configure sso`). Map a few test Entra groups to departments and roles (Administration → Identity provider mappings). |
| Data | Two options, ideally both: `docker compose run --rm app seed-demo` on a throwaway install (sample documents, boards, users), and a copy of real documents from one department on the real test install. |
| Test accounts | One person per role: administrator, auditor, records officer, legal counsel, department head, appointments clerk, an ordinary staff member in each of two departments, and someone with no department. |
| Backups | Run `deploy/backup/pdms-backup.sh` nightly on the test server from day one. |

### 1.2 What to test

Work through each area; tick it off when it behaves as expected for **every
role** that should (and should not) be able to do it. A denied action
should show "You do not have permission" and appear under **Reports →
Denied access attempts**.

**Sign-in and access**
- [ ] Entra sign-in creates the user with the right departments and roles from group mappings; removing someone from a group changes their access at next sign-in.
- [ ] Break-glass administrator signs in with password + authenticator code; five wrong passwords lock it temporarily.
- [ ] A new staff member sees only their department's documents (search, calendar, dashboard, ICS feed, API, direct links all agree).
- [ ] Sharing one document with another department exposes only that document; revoking a grant takes effect on the next click.
- [ ] Confidential documents are invisible to everyone without an explicit grant, including administrators, exports, and Outlook.

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
- [ ] Optional: Outlook calendar sync (`--configure outlook`) creates, moves, and removes events in the shared calendar.

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
- [ ] Production Entra app registration with the production redirect URI; group mappings for every department and role.
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
| Migration | **No bulk import tool.** Documents are created one at a time (or via the API without files). | Go-live with hundreds of existing documents needs it. | L |
| Identity | Department/role changes from Entra apply at the user's next sign-in; departed staff are not disabled automatically. | Offboarding relies on someone marking the account departed. | M |
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

Ordered by likely value; adjust after the pilot.

### Next (after testing feedback)

1. **Bulk import.** A CSV of document details plus a folder of files, with a dry run that reports problems before anything is created, and an audit entry per document. Essential for go-live.
2. **Fixes and polish from testing.** This always comes first once testing starts.
3. **Entra lifecycle sync** through Microsoft Graph: nightly group-membership sync and automatic deactivation of disabled Entra accounts.
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
- **Database changes:** add a new migration file (`migrations/0008_…`); never edit an applied one.
- **Decisions:** record anything a future maintainer would ask "why?" about in `docs/DECISIONS.md` (next number after D-079).
- **Versions:** tag releases (`v1.0.0` for go-live), keep a `CHANGELOG.md`, and upgrade servers with `git pull && ./install.sh` after a backup.
