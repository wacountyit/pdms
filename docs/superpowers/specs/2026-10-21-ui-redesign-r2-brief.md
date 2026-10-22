# PADM UI redesign, round 2 (R2): brief

- **Date:** 2026-10-21
- **Status:** brief, not a spec. This is the starting point for the R2 design
  spec, which the project owner must approve before any build (ROADMAP §0.4).
- **For:** the contributor, and their Claude session, picking up R2. Read
  [`CLAUDE.md`](../../../CLAUDE.md) at the repository root first.
- **Who has it:** [ROADMAP §0.1a](../../ROADMAP.md#01a-who-is-working-on-what).
  Whoever does R2 keeps that row current.
- **Roadmap:** sub-project 5b in [ROADMAP §0.3](../../ROADMAP.md#03-sub-projects).
  On 2026-10-21 the owner started R2 in parallel with sub-projects 1–3.
  Decision D-092 had placed it after them.

## 1. What R2 is

R1 gave every page the new frame and visual system ("direction D", D-092) and
fully redesigned five screens: sign-in, dashboard, documents list, document
page, and the county overview under Reports. Every other page sits inside the
new shell and uses the new colors, fonts, buttons and tables. Its **layout is
still the old one.**

**R2 redesigns those remaining screens one by one, so they look and work like
the five R1 screens.** It uses R1's design system; it does not invent a new
one.

**R2 is done when:**

- every screen in §3 that the owner keeps in scope matches the approved R2
  spec;
- R1's constraints (§5) still hold everywhere;
- every page passes the screenshot and keyboard checks (§8, step 5);
- all existing tests pass, and the new behaviour has its own tests;
- ROADMAP, DECISIONS, and the admin guide are updated (§8, step 6).

## 2. Read first

1. [`CLAUDE.md`](../../../CLAUDE.md): rules, commands, and how two people
   take turns ("Working in turns"). Then ROADMAP §0.1a and §0.5 to §0.7:
   who has what, what can start next, which files other work touches,
   and the questions waiting for the owner.
2. [The R1 spec](2026-10-15-ui-redesign-r1-design.md):
   - §3 Constraints;
   - §4 Visual system (tokens, type scale, components);
   - §5 App shell;
   - §12 Other pages;
   - §15 Testing.

   **R2 inherits all of it.**
3. **The five R1 screens in the code** (paths under `crates/pdms-web/`).
   They are the reference implementation:
   - `templates/dashboard.html`;
   - `templates/documents/list.html`;
   - `templates/documents/show.html`;
   - `templates/reports/index.html`;
   - `templates/login.html`;
   - the matching sections of `crates/pdms-web/static/app.css`.

   The direction-D mockups the owner chose from are private and are not
   needed. The built screens replace them.
4. **Decisions:** [DECISIONS](../../DECISIONS.md) D-085 and D-090
   (visibility), and D-092 to D-095 (R1).
5. **The admin guide:** [ADMIN_GUIDE](../../ADMIN_GUIDE.md), for what people
   actually do on each screen.

## 3. Screens in scope

Paths are relative to `crates/pdms-web/`. "Handler" is the route file under
`src/routes/`. Each admin page is mounted under `/admin`.

### 3.1 Administration

The admin pages sit under a row of tabs: `<nav class="tabs admin-tabs">` in
`templates/base.html`, shown when `Layout.admin_tabs` is set. Today:

- **The row is crowded.** It has 18 tabs, in two rows at 1440 px.
- **The current tab is not marked** (an R1 follow-up; see §6).
- **Pages lack structure:** tables are in panels, but "add" forms sit loose
  below them, and page actions have no consistent place.

| Screen | Route | Template | Handler | Notes |
|---|---|---|---|---|
| Users, user detail | `/admin/users`, `/admin/users/{id}` | `admin/users.html`, `admin/user.html` | `admin.rs` | Usernames (D-094), status, and how SSO matches people (P2) |
| Departments, department | `/admin/departments`, `/admin/departments/{id}` | `admin/departments.html`, `admin/department.html`, `admin/grants_table.html` | `admin.rs` | |
| Groups, group | `/admin/groups`, `/admin/groups/{id}` | `admin/groups.html`, `admin/group.html` | `admin.rs` | |
| Roles | `/admin/roles` | `admin/roles.html` | `admin.rs` | |
| Access grants | `/admin/grants` | `admin/grants.html`, `admin/grant_form.html`, `admin/grants_table.html` | `admin.rs` | |
| Identity provider mappings | `/admin/idp` | `admin/idp.html` | `admin.rs` | Shows a notice (P2) that mappings are not applied while the Entra sync is on |
| Entra ID sync | `/admin/entra` | `admin/entra.html` | `admin.rs` | Added by P2 and already in R1 markup. P4 adds a "SharePoint folder access" section |
| Document types, type | `/admin/document-types`, `/admin/document-types/{id}` | `admin/document_types.html`, `admin/document_type.html`, `admin/type_form.html` | `doc_admin.rs` | Sub-project 3 renames "Partnership Agreement" to ILA (data, not layout) |
| Custom fields, field | `/admin/fields`, `/admin/fields/{id}` | `admin/fields.html`, `admin/field.html`, `admin/field_form.html` | `doc_admin.rs` | |
| Tags | `/admin/tags` | `admin/tags.html` | `doc_admin.rs` | Sub-project 3 makes tags admin-curated (D-088), so this page's role grows |
| Retention | `/admin/retention` | `admin/retention.html` | `doc_admin.rs` | |
| Deleted documents | `/admin/deleted` | `admin/deleted.html` | `doc_admin.rs` | |
| Notification rules | `/admin/notification-rules` | `admin/notification_rules.html`, `rule_form.html` | `schedule.rs` | **`rule_form.html` is shared** with the R1 document page (`documents/show.html` includes it), so a change here changes that screen too |
| Delivery log | `/admin/notifications` | `admin/deliveries.html`, `admin/deliveries_table.html` | `schedule.rs` | |
| Branding | `/admin/branding` | `admin/branding.html` | `branding.rs` | **Already built in R1.** Keep it, and align it only if the admin pattern changes |
| Webhooks, webhook | `/admin/webhooks`, `/admin/webhooks/{id}` | `admin/webhooks.html`, `admin/webhook.html` | `integrations.rs` | |
| API | `/admin/api` | `admin/api.html` | `integrations.rs` | |
| Outlook & OCR | `/admin/outlook` | `admin/outlook.html` | `collab.rs` | **Sub-project 3 removes the Outlook sync** (D-087). Don't redesign the Outlook part; the OCR settings need a new home |
| SharePoint | `/admin/sharepoint` | (P4) | (P4) | **Does not exist yet.** P4 will build it, following whatever admin pattern R2 sets |

### 3.2 Everyday screens

| Screen | Route | Template | Handler | Notes |
|---|---|---|---|---|
| Calendar | `/calendar` | `calendar.html` | `schedule.rs` | Internal only (D-087). It shows everything the viewer can see (D-090). `.agenda-day` was never styled |
| Calendar feeds | `/calendar/feeds` | `feeds.html` | `schedule.rs` | ICS subscription links |
| Appointments (boards) | `/positions`, `/positions/{id}` | `positions/list.html`, `positions/show.html`, `positions/person_fields.html` | `appointments.rs` | The dashboard's "Boards and commissions" panel (R1) links here, and its decision states must read the same here |
| Records requests | `/exports` | `exports.html` | `exports.rs` | Package builder and downloads |
| Notifications | `/notifications` | `notifications.html` | `schedule.rs` | Unread count in the sidebar |
| Audit log, verify | `/audit` (POST `/audit/verify`) | `audit.html`, `audit_verify.html` | `audit.rs` | Uses `.cat`, which was never styled. Exports are CSV and JSON |
| Account | `/account/tokens` | `account_tokens.html` | `integrations.rs` | API tokens and "Your access" (moved here in R1, D-095) |
| Report detail | `/reports/{key}` | `reports/show.html` | `exports.rs` | The R1 overview at the top of `/reports` stays |

### 3.3 Not named in the R2 scope line, and not redesigned either

Ask the owner whether R2 covers these:

- the new-document and edit forms (`/documents/new`, `/documents/{id}/edit`:
  `documents/new.html`, `documents/edit.html`, `documents/fields.html`,
  `documents/metadata_fields.html`);
- version history (`/documents/{id}/history`, `documents/timeline.html`,
  which uses `.cat`);
- comment history (`documents/comment_history.html`) and the comment itself
  (`documents/comment.html`, included by `documents/collab.html`);
- the workflow and comment fragments on the document page. These use `.steps`,
  `.submit-review` and `.replies`, which were never styled:
  - `documents/workflow.html`;
  - `documents/collab.html`;
  - `documents/signatures.html`;
- the error page (`error.html`), which R1 only restyled;
- `/source` and `/license`, which are inline HTML in `routes/ops.rs`.

`/api/docs` is a generated reference page and is out of scope.

## 4. What R1 already gives you

- **Shell:**
  - `templates/base.html` and `templates/partials/nav.html` give the
    sidebar, top bar, search box, skip link and admin tab row.
  - `Layout` (`src/views.rs`) is built with
    `Layout::new(&state, &user, flash, title, section)`. Every page in the
    shell takes `l: Layout`; sign-in and the error page don't. `section` picks
    the highlighted sidebar item, and `"admin"` also shows the admin tabs.
- **Tokens:** the custom properties on `:root` in `static/app.css`. The
  accent and its derived shades come from `/theme.css`, which is generated
  from Branding.
  - Never hard-code the accent or the sidebar colors; use the tokens.
  - Contrast rules are in R1 spec §4.
- **Components** in `static/app.css`, each section headed by a comment:
  - buttons (`.button`, `.primary`, `.danger`, `.small`);
  - form controls;
  - panels and tiles (`.panel`, `.panel-head`, `.tile`);
  - tables (`.table-wrap`);
  - status (`.status`, `.status-*`, `.badge`);
  - navigation (`.tabs`);
  - messages (`.flash-ok`, `.flash-info`, `.flash-error`);
  - the disclosure (`<details>`);
  - utilities (`.visually-hidden` is in the Base section);
  - facts (`dl.facts`);
  - forms (`.grid-form`, `.inline-form`, `.toolbar`);
  - tags;
  - comments;
  - audit changes;
  - calendar (`.cal-ev`);
  - charts;
  - the dashboard, documents list and document page sections (key numbers,
    key-dates timeline, side panel, "Manage this document").
- **Admin page examples:**
  - **`templates/admin/entra.html` (P2)** is the fullest example of an admin
    page in the new markup:
    - `.panel` sections with `.panel-head`;
    - tables in `.table-wrap`;
    - `dl.facts`;
    - notices as `<p class="flash flash-info" role="status">`;
    - problems as `role="alert"`;
    - `.toolbar`, `.inline-form` and `.grid-form`.
  - `templates/admin/branding.html` (R1) is a settings form, built from
    `.panel`, `.grid-form` and `.help`.
  - Nothing has fixed an admin page pattern yet. The R2 spec should set one.
- **Charts:** server-rendered SVG. The templates are in `templates/charts/`
  and the layout code is in `src/charts.rs`; colors come from CSS classes,
  never inline styles.
- **Icons:** a Lucide subset as Askama partials in `templates/icons/`.
  - To add one, copy it from Lucide (ISC license) in the same form.
  - Keep `THIRD_PARTY_NOTICES.md` current.
- **Tests to keep green and extend**, all in `crates/pdms-web/tests/`:
  - `shell.rs`: the shell;
  - `csp_guard.rs`: scans every template for `style=`, inline `<script>`
    and `on…=`;
  - `web.rs`, `documents.rs`, `schedule.rs`: page behaviour;
  - `phase3.rs`: positions, appointments, workflow panels;
  - `phase4.rs`: tokens, webhooks, exports, reports;
  - `phase5.rs`: comments, the Outlook/OCR admin page;
  - `branding.rs`, `entra.rs`, `oidc.rs`: Branding, Entra ID sync, sign-in.

  Shared helpers (a signed-in browser, CSRF) are in `support.rs`.
- **Screenshot tool:** [`tools/ui-screenshots/`](../../../tools/ui-screenshots/README.md).
  It shoots every page at desktop and phone widths, and flags sideways
  scrolling, doubled headings and console (CSP) errors.

## 5. Constraints (binding, carried over from R1 §3)

- **CSP stays strict:**
  - no `style="…"` attributes;
  - no inline `<script>`;
  - no `on…=` handlers;
  - no CDNs or web fonts from elsewhere.
- **No JavaScript is required.** HTMX keeps working where it is used today.
  Use `<details>` for disclosure.
- **WCAG 2.1 AA:**
  - text contrast at least 4.5:1;
  - UI marks at least 3:1;
  - everything reachable by keyboard, with visible focus;
  - status never shown by color alone;
  - one `<h1>` per page.
- **Permissions:** every count, list and chart shows only what the viewer may
  see (D-085, D-090). Nothing in R2 widens access.
- **Behaviour is preserved.** R2 is presentation:
  - Every form keeps its `action`, field names, CSRF token, and HTMX targets.
    R1 checked all 31 forms on the document page this way.
  - Handlers may gain view data (counts, grouping, "N more"), but what saves,
    who may do it, and what is audited do not change.
  - Any behaviour change must be in the approved spec.
- **Public repository:**
  - placeholder branding only;
  - no real seal, internal hostnames or addresses;
  - no security-review details (see `CLAUDE.md`).
- **Self-hosted assets only.** New fonts or icons ship in the binary, the same
  way the existing ones do.

## 6. R1 follow-ups to fold in

These were found while R1 was built and left for later. Take the ones on
screens you touch. The items on R2 screens were checked as still open on
2026-10-21. Re-check the R1-screen items in the code before you take them.

**On R2 screens:**

- The admin tab row doesn't mark the current tab. It needs a `Layout` field;
  use `aria-current="page"`.
- These classes are used but never styled:
  - `.agenda-day` (calendar);
  - `.cat` (audit log, version history);
  - `.steps` and `.submit-review` (workflow);
  - `.replies` (comments).
- Dead CSS: `.sig-declined`, `.sig-requested`, `.sig-signed` and
  `.sig-pending` are not used anywhere.
- The `.table-wrap > table > caption` padding rule also hits
  `caption.visually-hidden`.
- The chart container-query pixel sizes break if a chart's viewBox changes.
- `tests/csp_guard.rs` checks line by line. A `style` attribute split
  across lines (`style` on one line, `="…"` on the next) gets past it.
  Harden it if you work on it.

**On R1 screens (small; take them if they're cheap):**

- **Documents list:** no tab carries `aria-current` when the status filter is
  archived or superseded.
- **Dashboard:**
  - "Boards and commissions" (6 items) and "Coming up" (8) show no "N more"
    count.
  - An empty first name gives "Good morning, ".
  - `.date-block` hard-codes the navy and white values; use tokens.
- **Document page:**
  - "Start review" still shows for a review that has already started.
  - "Declined" appears twice in a declined signer's row.
  - The "Review & approval" link lands below the panel heading.
- **County overview:**
  - its tile links drop the department filter;
  - the "next date" column says "Overdue" for expired, draft and archived
    documents;
  - **owner question:** the status chart counts superseded documents under
    "Other", while the Documents tile excludes them. The overdue tile also
    counts appointments, but the chart's Overdue segment doesn't. So the
    totals can differ. Suggested fixes are a subtitle, or dropping
    superseded from "Other". **Ask the owner.**

## 7. Work in flight that touches the same files

| Work | State on 2026-10-21 | What it changes that R2 also touches |
|---|---|---|
| **P2, Entra user sync**, [PR #1](https://github.com/wacountyit/pdms/pull/1) | **Merged** to `main` on 2026-10-21 | It added `/admin/entra` (`admin/entra.html`) and its tab in `base.html`, a notice in `admin/idp.html`, and new help text in `admin/users.html`, plus migration `0010` and decision D-096. Nothing is pending; listed so you know where those pages came from |
| **P3/P4, SharePoint storage** (plans in `docs/superpowers/plans/`) | Planned, not started. P3 can run alongside R2; P4 waits until R2's admin pages are merged (ROADMAP §0.5) | P4 adds `/admin/sharepoint`, a section on `/admin/entra`, an admin tab, a `Layout` flag, and a notice on the document page. P3 changes the upload and download code under the document pages, but not their templates |
| **Sub-project 3, small changes** (D-087, D-088) | Not started. Agree with the owner who does it, and whether before or after R2's admin pages (ROADMAP §0.5) | Removes the Outlook sync (`admin/outlook.html`, config, installer). Renames the Partnership Agreement type to ILA. Makes tags admin-curated (`admin/tags.html`, tag entry on the document forms) |
| **Security fixes** (private findings; ROADMAP §0.3) | Not started; can run alongside | May touch the same handlers and templates. Before a large refactor of a route or template, ask the owner whether a fix is planned there |

This table was correct on 2026-10-21. For the current state, read ROADMAP
§0.1a and §0.6, and run `gh pr list`.

**How to coordinate:**

- **Branch from `main`.** P2 is merged, and `gh pr list` shows anything else
  in flight. Follow "Working in turns" in `CLAUDE.md`: open a draft PR as
  soon as you start each part, and push at the end of every session.
- **Set the admin page pattern early, and write it down in the R2 spec.** P4
  builds `/admin/sharepoint` after it. P4's plan was written before R1 and
  uses the old markup (`<section aria-labelledby>` without `.panel`, and
  `flash-error` with `role="status"`; P4 Task 7, the `admin/sharepoint.html` template). So when the
  pattern is set, add a line to the plan index
  (`docs/superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md`, under
  "Changes recorded when P4 was written") telling P4 to use it.
- **Don't polish what sub-project 3 removes** (the Outlook part of
  `/admin/outlook`).
- **Ship R2 in several small PRs, by area** (§8, step 4), so each one
  conflicts with less.
- **Numbers.** R2 shouldn't need a migration. If it does, or if it adds a
  decision, take the next number after the highest one on `main` **and** in
  open PRs. `main` ends at migration 0010 and D-096. P3 and P4 have
  planned 0011, 0012 and D-097 onward, and will renumber at their own
  pre-flight if R2 gets there first.

## 8. How to do it

1. **Settle scope and priorities with the owner** (§9). After you claim
   R2, open the spec's draft PR (step 3) and put the §9 questions at the top
   of its description. Then ask the person you work for to take them to the
   owner, or mention `@wc-sstevens` in a comment on the PR.
2. **Write the R2 design spec** at
   `docs/superpowers/specs/<date>-ui-redesign-r2-design.md`. Use R1's spec as
   the model: purpose, scope, constraints, then one section per screen
   group. For each screen, give:
   - its purpose and the people who use it;
   - its primary actions;
   - the layout (what sits where, and why);
   - its empty states;
   - permission notes;
   - any new components (added to `app.css` with tokens).

   Mockups for the owner are fine. Keep them out of the repository, as R1 did.
3. **Get the owner's approval.**
   - Put the spec in its own pull request, opened as a GitHub draft while
     you write it. Put the §9 questions, and the owner's answers, at the top
     of the PR description.
   - When the spec is complete, mark the PR ready for review and ask the
     owner (`@wc-sstevens`) to review it. Until the owner approves, the
     spec's Status line reads "Draft for owner review".
   - Once the owner approves, set the Status line to "Approved" with the
     date. In the same PR, record new decisions in `docs/DECISIONS.md`, and
     add a dated update to D-092 if the order changes again. The owner
     merges.
4. **Write the implementation plan** in `docs/superpowers/plans/`. Split it
   by area so each part can merge on its own. For example:
   - admin pages;
   - calendar and feeds;
   - appointments;
   - records requests;
   - notifications, audit and account;
   - report detail.
5. **Build each part on a branch, with tests.** Assert the key markup and the
   visibility rules; `csp_guard` stays green. Then run the screenshot tool:
   - every page at 1440×900, and key pages at 390×844;
   - no sideways page scroll, no cut-off text, exactly one `<h1>`, and no
     console errors;
   - a keyboard pass on every page you changed.

   The demo data (`crates/pdms-core/src/demo.rs`) doesn't cover every
   screen. It has no webhooks, so the webhook detail and delivery views are
   empty, and comment history needs a comment that was edited. Create those
   records by hand in the local instance, or add them to the demo data, and
   design every empty state on purpose. When you add a page to
   `tools/ui-screenshots/shoot.py`, add its records too.
6. **Open a PR to `main` for each part.** The owner merges. Update:
   - ROADMAP §0.3 row 5b (status, links);
   - DECISIONS;
   - ADMIN_GUIDE, wherever a screen's wording or steps changed;
   - the README, if it describes old screens.
7. **The owner deploys** to the test server: backup first, then
   `git pull && ./install.sh`.
8. **Keep the R2 row in ROADMAP §0.1a current** as you go. List the spec,
   the plan and each build part, marked Not started, In progress, In review
   or Done with its PR number ("Working in turns" in `CLAUDE.md`).
9. **When R2 is done:**
   - in the last R2 PR, mark row 5b **Done** in ROADMAP §0.3, and update
     §0.5 and §0.6;
   - after it merges, add R2 to §0.1 and remove its row from §0.1a (on
     `main`);
   - pick the next unclaimed item from §0.5.

## 9. Questions to settle with the owner before the spec

- **Scope:** are the screens in §3.3 in R2? They are the document forms,
  version and comment history, and the workflow and comment fragments.
- **Admin navigation:** keep one tab row, or group the admin pages into
  sections? For example:
  - People and access;
  - Documents and records;
  - Notifications;
  - Integrations;
  - System.
- **Order:** which screens matter most for the next leadership showing?
- **OCR settings:** once sub-project 3 removes the Outlook part of
  `/admin/outlook`, where do the OCR settings live, and does R2 or
  sub-project 3 move them?
- **Tags page:** design it for today's free-form tags, or for the
  admin-curated tags that D-088 calls for?
- **Phone width:** which pages are "key" and get the 390 px check? R1 checked
  its five screens.
- **The overview totals question** in §6.
- **Leftovers:** confirm that dark mode, an activity feed and a browser-tab
  icon stay "later" (R1 spec §18), unless the owner wants them now.
