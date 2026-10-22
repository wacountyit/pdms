# PADM UI redesign, round 2 (R2): design

- **Date:** 2026-10-22
- **Status:** Draft for owner review.
- **Builds on:** the [R2 brief](2026-10-21-ui-redesign-r2-brief.md) and the
  [R1 spec](2026-10-15-ui-redesign-r1-design.md). R2 inherits R1's
  constraints (§3), visual system (§4), app shell (§5) and testing (§15).
- **Roadmap:** sub-project 5b in
  [ROADMAP §0.3](../../ROADMAP.md#03-sub-projects). Who has it and how far it
  has got: [ROADMAP §0.1a](../../ROADMAP.md#01a-who-is-working-on-what).
- **Owner's answers:** the questions in brief §9 were answered by the owner
  on 2026-10-22. §2 records them, and the rest of this spec follows them.
  The follow-up questions O-1 to O-3 were answered the same day by the
  contributor (`WACountyIT`); the owner confirms or changes those answers in
  this spec's review.

## 1. Purpose

R1 gave every page the new frame and fully redesigned five screens. R2
redesigns the rest, so every screen looks and works like those five, and
adds three things R1 left for later: dark mode, an activity feed, and a
browser-tab icon.

**R2 is done when:**

- every screen in §4 matches this spec;
- R1's constraints (brief §5) hold everywhere, in light and dark mode;
- every changed page passes the screenshot and keyboard checks (§11);
- all existing tests pass, and the new behaviour has its own tests;
- ROADMAP, DECISIONS and the admin guide are updated (§12).

## 2. The owner's answers (2026-10-22)

| # | Question | Answer |
|---|---|---|
| 1 | Scope beyond the brief's §3.1 and §3.2 | **In:** the new-document and edit forms; the workflow, signature and comment sections of the document page. **Out:** version and comment history, the error page, `/source` and `/license` |
| 2 | Admin navigation | Grouped sections: People and access; Documents and records; Notifications; Integrations; System (§5.1) |
| 3 | Order | Appointments, calendar, report detail, then the admin pages, records requests, notifications, the audit log and the account page. No date yet for the leadership showing |
| 4 | Sub-project 3 | The owner does it **before** R2 reaches the Tags and Outlook & OCR pages |
| 5 | OCR settings | Their own **"Text extraction (OCR)"** page under System. Sub-project 3 moves them there (it is one status paragraph, not a form); R2 styles it |
| 6 | Tags page | Designed for admin-curated tags (D-088) |
| 7 | Phone width (390 px) | Calendar, notifications, appointments and the account page. Admin pages get the desktop check only |
| 8 | Overview totals | Drop superseded documents from the status chart's "Other", so the chart's total matches the Documents tile. Label the overdue tile to say it counts documents and board terms |
| 9 | Dark mode, activity feed, tab icon | **All three are in R2** (§7, §8, §9). Dark mode needs its own checked palette, and the accent contrast check must hold in both modes. The feed filters audit events for each viewer, follows D-085 and D-090, and widens nothing. The tab icon is made from the seal |
| 10 | An early PR for the small R1 fixes | Yes (§3) |

Follow-up questions raised by this spec, answered by the contributor on
2026-10-22, **for the owner to confirm in review:**

| # | Question | Answer |
|---|---|---|
| O-1 | Dark mode by device setting only, or a per-person switch? | **A per-person switch, with dark mode the default** (§7) |
| O-2 | Activity feed on the dashboard only, or also its own page? | **The dashboard only** (§8) |
| O-3 | Order of parts 7 to 9 | As this spec recommends: after part 6, dark mode last (§4) |

## 3. Early PR: R1 fixes (before this spec is approved)

The owner allowed these to go ahead of the spec. They change no behaviour
and add no screens. Branch `r2-r1-fixes`, PR title `R2: R1 fixes`.

1. **Admin tab marks the current page.** Add `Layout.admin_page:
   &'static str` (a key such as `"users"`), set by each admin handler, and
   render `aria-current="page"` on the matching tab. §5.1 later replaces the
   row, keeping the same field.
2. **Styles for classes that were never styled:** `.agenda-day`, `.cat`,
   `.steps`, `.submit-review` and `.replies`, built from existing tokens and
   components (a dated heading row; a neutral category badge; the compact
   table; the disclosure; an indented thread with a left rule).
3. **Remove dead CSS:** `.sig-declined`, `.sig-requested`, `.sig-signed`,
   `.sig-pending`.
4. **Caption padding:** the `.table-wrap > table > caption` rule no longer
   applies to `caption.visually-hidden`.
5. **`tests/csp_guard.rs` checks whole tags, not lines,** so an attribute
   split across lines (`style` on one line, `="…"` on the next) is caught.
   The test gets a fixture proving it.
6. **Dashboard:**
   - "Boards and commissions" and "Coming up" show "N more" with a link
     (to `/positions` and `/calendar`) when items were cut off;
   - an empty first name gives "Good morning" with no trailing comma;
   - `.date-block` uses tokens, not hard-coded navy and white.
7. **Documents list:** a tab carries `aria-current` when the status filter is
   archived or superseded (or a "More" state is marked).
8. **Document page:** "Start review" is hidden when a review is already
   under way; "Declined" appears once in a declined signer's row; the
   "Review & approval" link lands on the panel heading.
9. **"Next date" on the documents list** (the brief lists it under the
   county overview) says "Overdue" only for documents the overdue report
   also counts: not for expired, superseded or archived ones. Drafts keep
   "Overdue", because the dashboard and the overdue report count them. The
   overview's tile links already keep the department filter on `main`.

Each item is re-checked in the code before it is fixed (brief §6), and each
gets a test where it can have one.

## 4. Screens and the order they are built in

Each part is its own branch and PR, in the owner's order (§2, answer 3).

| Part | Screens | Branch | Phone check |
|---|---|---|---|
| 1 | Appointments: `/positions`, `/positions/{id}` (§6.1). Also adds the shared components in §5.2 | `r2-appointments` | Yes |
| 2 | Calendar and feeds: `/calendar`, `/calendar/feeds` (§6.2) | `r2-calendar` | Calendar |
| 3 | Report detail `/reports/{key}`, and the overview totals (§6.3) | `r2-reports` | No |
| 4 | Admin navigation and every admin page (§5) | `r2-admin-pages` | No |
| 5 | Records requests `/exports` (§6.4) | `r2-exports` | No |
| 6 | Notifications, audit log and verify, account (§6.5–§6.7) | `r2-notifications-audit-account` | Notifications, account |
| 7 | Document forms, and the workflow, signature and comment sections (§6.8) | `r2-document-forms` | No |
| 8 | Browser-tab icon (§9) and activity feed (§8) | `r2-feed-icon` | No |
| 9 | Dark mode (§7). Last, because it must cover every finished screen | `r2-dark-mode` | All four |

Answer 3 did not place parts 7 to 9; this spec puts them after part 6
(O-3). Dark mode comes last because it checks every finished screen in both
modes once, instead of re-checking each screen as it is redesigned. Until
part 9 merges, PADM stays light; from then on it is dark by default. Every
part before it uses only tokens (§10), so part 9 changes no layouts. Part 4
is also what plan P4 waits for (ROADMAP §0.5), so P4 starts after parts 1 to
4 are merged.

## 5. Administration

### 5.1 Navigation: grouped sections

The single row of 18 tabs becomes a grouped **admin menu**.

- **From 900 px up:** a column, 220 px wide, at the left of the content
  area, beside the page. It is a `<nav aria-label="Administration">` holding
  one heading per group (`.nav-group-label` style, as in the sidebar) and a
  list of links. The current page has `aria-current="page"` and the
  `--accent-tint` background with a 3 px accent bar, so it is not marked by
  color alone.
- **Below 900 px:** the same menu sits above the page inside a `<details>`
  whose summary reads "Administration: {current page}". No JavaScript.
- **Groups and items.** Each item keeps today's permission check; a group
  whose items are all hidden is not shown.

| Group | Items (permission) |
|---|---|
| People and access | Users, Departments, Groups, Roles, Access grants, Identity provider mappings, Entra ID sync (`can_directory`) |
| Documents and records | Document types, Custom fields (`can_schema`); Tags (`can_tags`); Retention (`can_retention`); Deleted documents (`can_restore`) |
| Notifications | Notification rules (`can_schema`); Delivery log (`can_settings`) |
| Integrations | Webhooks, API (`can_integrations`); SharePoint (added by P4) |
| System | Branding (`can_settings`); Text extraction (OCR) (`can_integrations`, once sub-project 3 adds it) |

- Until sub-project 3 is merged, "Outlook & OCR" stays under Integrations as
  it is today.
- **Code:** the menu moves out of `base.html` into
  `templates/partials/admin_nav.html`, driven by `Layout.admin_page` (§3,
  item 1). `admin_home()` is unchanged. The page `<h1>` stays in the content
  area. No admin route moves.
- P4 adds its SharePoint item to Integrations, using the same pattern.

### 5.2 Admin page pattern (also used by the everyday screens)

Every admin page uses these parts, in this order. `admin/entra.html` is
already close to it.

1. **Page header** (`.page-head`): breadcrumb on detail pages (`Users ›
   Jane Doe`); the `<h1>`; a one-sentence purpose line (`.page-lede`,
   `--muted`); page actions on the right (`.page-actions`), with at most one
   primary button.
2. **Notices:** `<p class="flash flash-info" role="status">` for state
   ("Entra sync is off"); `role="alert"` with `flash-error` only for problems
   that need action.
3. **The main list** in a `.panel`: `.panel-head` with the `<h2>`, the count
   ("12 users"), and a filter or search form if the page has one. The table
   sits in `.table-wrap`.
4. **Empty states** (`.empty`, a new component): one sentence saying what
   would appear and one action ("No webhooks yet. Add one to send document
   events to another system."). They replace "No … yet." table rows.
5. **"Add" forms** in their own panel below the list, headed "Add a …", as
   `.grid-form`. When the list is long, the form is a `.disclosure` that is
   open when the list is empty. A page action "Add a …" links to it.
6. **Edit in place** stays in a `.disclosure` per row only where it is used
   today (tags, retention policies, rules). Detail pages put each editable
   group in its own panel with its own save button.
7. **Danger zone** last: destructive actions (remove credentials, delete) in
   a panel headed "Remove" or "Delete", with `.button.danger` and the
   consequence written next to it.

**New components** in `static/app.css`, each in its own commented section and
built from tokens: `.page-head`, `.page-lede`, `.page-actions`, `.empty`,
`.admin-layout` (menu column plus page), `.admin-nav`. They arrive in part 1,
where the everyday screens use `.page-head` and `.empty` first.

When this pattern is merged, add a line to the plan index
(`docs/superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md`, under
"Changes recorded when P4 was written") telling P4's `admin/sharepoint.html`
to use it (brief §7).

### 5.3 Pages

All forms keep their `action`, field names, CSRF token and HTMX targets.

- **Users** (`admin/users.html`): page action "Add a user"; the search moves
  into the panel head; status becomes `.status` with its word (Active good,
  Disabled neutral, Departed neutral); the SSO help text moves into the
  "Add a user" panel as `.help`.
- **User** (`admin/user.html`): header with breadcrumb, name as `<h1>`, and
  status badge. Panels: Overview (facts: status, sign-in, last sign-in, and
  the status form); Profile; Departments; Groups; Roles; Emergency sign-in
  (only when local credentials exist); Remove (the "Remove local
  credentials" button). "View audit history" becomes a page action, shown
  only to people who can see the audit log.
- **Departments, Department, Groups, Group, Roles:** list-plus-add pattern;
  detail pages get the header, a facts panel, and the grants table in a
  panel.
- **Access grants** (`grants.html`, `grant_form.html`, `grants_table.html`):
  the shared grants table gets `.panel` and `.empty`; the form becomes its
  own "Add a grant" panel.
- **Identity provider mappings:** the D-096 notice stays, as `flash-info`
  `role="status"`, at the top.
- **Entra ID sync:** gains the page header; its "Department mapping" panel
  gets a `.panel-head`. Otherwise unchanged. P4 adds its section here.
- **Document types, type; Custom fields, field:** list-plus-add; the detail
  pages put the type form and its rules in separate panels. The ILA rename is
  data and needs nothing here.
- **Tags** (admin-curated, D-088, after sub-project 3): one table of tags
  (swatch and name, description, documents using it) with an "Edit"
  disclosure per row and an "Add a tag" panel. If sub-project 3 leaves any
  uncurated tags to review, they appear in a "Needs review" panel above the
  list. What exactly sub-project 3 ships decides the controls; this page
  follows it.
- **Retention:** each policy is a panel (`.card` is retired) with its
  summary in the `.panel-head` and its form in an "Edit" disclosure; "Add a
  policy" panel last.
- **Deleted documents:** a list panel; "Restore" stays a per-row button.
- **Notification rules:** each rule is a row in one table (scope, event,
  when, channels, recipients, state) with an "Edit" disclosure, instead of
  one card per rule. `rule_form.html` is shared with the document page, so
  only its wrapper changes, not its fields; it is checked on both pages.
- **Delivery log:** the status filter moves into the panel head; status
  becomes `.status` with its word.
- **Branding:** gets the page header only (R1 built it).
- **Webhooks, Webhook:** list-plus-add; the detail page has panels for
  Settings, Signing secret (with "Rotate"), and Deliveries.
- **API:** "All tokens" and "Recent requests" as table panels; the API-off
  notice becomes `flash-info` with `role="status"`.
- **Outlook & OCR / Text extraction (OCR):** R2 does not touch the Outlook
  part. After sub-project 3 adds the OCR page, R2 gives it the header and
  one facts panel (state, service address, what is read without OCR).

## 6. Everyday screens

### 6.1 Appointments (part 1)

People: the appointments clerks, position managers, department staff.
Decision states must read the same here as on the dashboard's "Boards and
commissions" panel: one shared mapping from seat state to label and status
class, used by both.

**List** (`/positions`):

- Header: `<h1>` "Boards and appointments", the purpose line, and the page
  action "Add a position" (managers only).
- **Count tiles** (`.tile`, as on the overview): seats filled and
  current; decision due or overdue; holdover; term ending soon; vacant. Same
  counts and visibility as the overview's board summary.
- **Table panel**, grouped by board or body (a row heading per body),
  columns: Position, Held by, Seats, Term, Next term end, Status. Status uses
  `.status` with the shared label. "Inactive" stays a neutral badge.
- Empty state: for managers, "No positions yet" with the add action; for
  others, the existing sentence on who can see positions.
- "Add a position" is its own panel (disclosure, open when the list is
  empty).

**Position** (`/positions/{id}`):

- Header: breadcrumb `Appointments › {body}`, `<h1>` "{position}", status
  badge, and page actions "Appoint someone" (when a seat is vacant) and "Edit
  position" (managers).
- **Two columns** (`.doc-layout` pattern from the document page):
  - **Main:** "Current appointees", one panel per appointee: name and
    standing badge, the term as a short key-dates line, requirement progress
    ("3 of 5 satisfied"), the checklist, and a "Manage" disclosure holding
    Reappoint, Replace, End and Correct (each its own form, unchanged). Then
    "History" as a table panel.
  - **Side:** "Position" facts; "Reminders"; "Requirements" (list plus "Add
    a requirement", managers only).
- Empty: "This position is vacant." with "Appoint someone".

### 6.2 Calendar and feeds (part 2)

**Calendar** (`/calendar`):

- Header: `<h1>`, page actions "Subscribe" (to feeds) and the Month/Agenda
  tabs.
- Filters: one row in a panel (Department, Type, Status, Tag, Owned by me),
  with "More" as a disclosure below 900 px.
- **Month view:** month name with previous/next buttons (visible text plus
  `aria-label`); a 7-column grid in which each day shows up to 3 events and
  "+N more" linking to that day in the agenda view. Event chips keep a word
  for their kind ("Review", "Expires") and "Overdue" in text; the legend
  stays and its dashed style is explained in words.
- **Agenda view:** `.agenda-day` sections with a date heading; each event is
  a row: kind badge, title link, ID and department. Range choices (30, 90,
  365 days) become a `.tabs` row.
- **Below 900 px** the month grid becomes the agenda list for that month,
  so the page never scrolls sideways.
- Empty: "Nothing scheduled in this period" plus a link to clear filters if
  any are set.

**Feeds** (`/calendar/feeds`): header with breadcrumb `Calendar › Feeds`; a
"How to subscribe" panel (the Outlook steps as a numbered list, and the
warning to treat the address like a password); "Your subscriptions" table
panel with status as `.status`; "New subscription" panel.

### 6.3 Report detail and the overview totals (part 3)

**Report detail** (`/reports/{key}`):

- Header: breadcrumb `Reports › {name}`, `<h1>`, the description as the
  purpose line, page action "Download CSV".
- Options in a panel above the table ("Run" button); the row count and
  subtitle in the table panel's head.
- Long reports get `.pager` at 100 rows (view data only; CSV still has every
  row).
- Empty: "Nothing to report" with the options shown.

**Overview totals** (owner's answer 8):

- The status chart's "Other" counts Draft and Expired only. Superseded
  documents are left out, as the Documents tile already does, so the chart's
  total equals the tile.
- The overdue tile is relabelled "Overdue dates", with the sub-line
  "Documents and board terms · oldest {N} days". The chart's "Overdue"
  segment keeps counting documents in Overdue status.
- A test asserts the chart total equals the Documents tile for a fixture
  with superseded, archived, draft and expired documents.

### 6.4 Records requests (part 5)

- Header: `<h1>`, the purpose line (shortened; the details about audit move
  into `.help`), page action "New package" (builders only).
- **"New package" panel**, in three numbered steps with headings: 1 Find
  documents (search); 2 Choose documents (the candidate table, with a count of
  selected rows in text); 3 Package options (reference, files, audit history,
  note, metadata). One primary button, "Build package".
- **Packages table panel:** reference and note, built, by, documents, files,
  size, a shortened SHA-256 with the full value in a `<details>`, and
  "Download".
- Empty states for "no search yet", "no documents match" and "no packages
  yet".

### 6.5 Notifications (part 6)

- Header: `<h1>` with the unread count in text, page actions "Acknowledge
  all" and the "Unread / All" tabs.
- A list, not a table, below 900 px: each notice is a row with its headline,
  what it is about (code and title link), due date, received time, and an
  "Acknowledge" button. Unread rows have a 3 px accent bar and the word
  "Unread" for screen readers.
- Empty: "You're all caught up."

### 6.6 Audit log and verify (part 6)

- Header: `<h1>`, page actions "Verify integrity" (verifiers only) and
  "Export" (a disclosure with CSV and JSON; exporters only).
- Filters in a panel: Action, Category, Outcome and dates in the first row;
  actor and object ID under "More filters".
- The table keeps its columns; the category is a neutral `.cat` badge (§3);
  "denied" and "failure" outcomes use `.status` with their word. Changes and
  context stay in disclosures.
- "Older entries" becomes a `.pager` link.
- **Verify** (`audit_verify.html`): a result panel with the outcome as a
  `flash-ok` or `flash-error`, facts (entries checked, last entry, last hash)
  and the breaks as a list. Its text is unchanged.

### 6.7 Account (part 6)

- `<h1>` "Your account". Panels: "Your access" (unchanged facts);
  "Appearance" (the dark-mode choice, added in part 9, §7); "API tokens"
  (table, with state as `.status`); "New token" (scopes as a checkbox list
  with each scope's description).
- The API-off notice becomes `flash-info` with `role="status"`: it is a
  state, not an error.
- The sidebar link text changes from "API tokens" to "Your account", since
  the page holds both.

### 6.8 Document forms and document page sections (part 7)

**New document and Edit** (`documents/new.html`, `edit.html`,
`metadata_fields.html`, `fields.html`):

- Header: breadcrumb (`Documents › New` or `Documents › {ID} › Edit`) and
  `<h1>`.
- Panels in order: Details (title, description, type); Dates (the date grid
  with its help); Additional fields (the HTMX `#custom-fields` target is
  unchanged); Ownership (department, owner, start as); Who can see it (the
  three visibility choices as large radio cards with their descriptions);
  Tags; File.
- A footer row with "Create document" (or "Save changes") and "Cancel".
- Errors stay at the top as `role="alert"`. Required fields keep the `*`
  plus a line explaining it.
- Tag entry follows what sub-project 3 ships for curated tags.

**Workflow** (`documents/workflow.html`): each round is a disclosure whose
summary shows status (`.status`), date and version; steps are a `.steps`
table; the decision form sits under the step it belongs to; the activity
list uses the timeline style. "Submit for review" is a `.submit-review`
disclosure with the pickers in a `.grid-form`.

**Signatures** (`documents/signatures.html`): the signer table keeps its
columns; actions move into one "Manage" disclosure per row; "Add a signer"
is a disclosure at the end.

**Comments** (`documents/collab.html`, `comment.html`): threads with the
author line, body and a quiet toolbar (History, Edit, Delete); replies
indented under `.replies` with a left rule; the new-comment form at the end.
"Dates found in the file" keeps its table and its "Nothing changes until you
confirm it" line.

All of these keep every form's `action`, fields and CSRF token (R1 checked
31 forms on the document page; the same check is repeated).

## 7. Dark mode (part 9)

- **Default: dark.** Every person sees dark mode until they choose
  otherwise (O-1).
- **A per-person switch** with three choices: **Dark** (the default),
  **Light**, and **Match my device** (follows `prefers-color-scheme`).
  - **Saved per user** in a new column, `users.color_scheme` (`dark`,
    `light` or `system`, not null, default `dark`). This is R2's one
    migration; it takes the next free number on `main` and in open PRs when
    part 9 is built (`0011` on 2026-10-22, if P3 has not taken it).
  - **Where it is set** (both post to one new route, `POST
    /account/appearance`, fields `color_scheme` and `back`):
    - an "Appearance" panel on the account page (§6.7), with the three
      choices as radio buttons and a "Save" button;
    - a one-click button in the sidebar's user block ("Light mode" or "Dark
      mode", with an icon), which switches between dark and light and
      returns to the current page. It is a POST form with the CSRF token. The
      return path goes through the same local-path check the admin pages use
      for `back`, so it cannot send anyone to another site.
  - **No JavaScript.** The server writes the choice into the page as
    `<html data-theme="dark|light|system">`, so the page arrives in the
    right colors with no flash of the wrong mode.
  - **Audited:** each change writes `user.color_scheme_set` (before and
    after) in the same transaction, like every other change of state.
  - **Pages with nobody signed in** (sign-in, and the error page before
    sign-in) use the default, dark.
  - **Only the person themselves** changes their setting. Administrators do
    not set it for others; it does not appear on the admin user page.
- **How the colors switch:** every color token on `:root` in `app.css` is
  redefined once for `:root[data-theme="dark"]` and once, identically,
  inside `@media (prefers-color-scheme: dark)` for
  `:root[data-theme="system"]`. A test checks the two blocks are the same.
  No template changes beyond the `data-theme` attribute and the switch; no
  hard-coded colors anywhere (parts 1 to 8 already follow this). The sidebar
  is already dark and keeps its values.
- **Palette** (starting values; each pair was checked at 4.5:1 for text and
  3:1 for marks, and the test below enforces it):

| Token | Light (R1) | Dark |
|---|---|---|
| `--ground` | `#F3F5F8` | `#0F1720` |
| `--surface` | `#FFFFFF` | `#17212B` |
| `--thead` | `#F7F9FB` | `#1C2733` |
| `--ink` | `#15202B` | `#E6EBF0` (13.6:1 on surface) |
| `--muted` | `#4A5868` | `#A7B3C0` (7.1:1 on thead) |
| `--border` | `#DCE2E9` | `#2A3644` |
| `--row` | `#EEF1F4` | `#222D39` |
| `--control-border` | `#7D8A9A` | `#748395` (4.2:1) |
| `--focus` | `#1F6FEB` | `#6CA6FF` (6.6:1) |

| Status | Mark (dark) | Text (dark) | Tint (dark) |
|---|---|---|---|
| Critical | `#E5655A` | `#FF9B91` | `#3A1C1A` |
| Warning | `#D08A1F` | `#F2B866` | `#3A2A12` |
| Info | `#5B8FE0` | `#8DB8FF` | `#172A45` |
| Good | `#3FA56A` | `#7FD1A0` | `#15321F` |
| Neutral | ring `#7D8A9A` | `#B4BFCB` | `#26313D` |

- **Accent in both modes.** `/theme.css` gains a dark block with three
  derived values:
  - `--accent-link`: the accent lightened in OKLCH steps until it reaches
    4.5:1 on the dark ground **and** the dark surface;
  - `--accent-mark` (tab underline, current-item bar, chart bars): lightened
    until 3:1 on the dark surface;
  - `--accent-tint`: 18% accent over the dark surface, with `--ink` text
    at least 4.5:1.

  These sit in `/theme.css` under the same two selectors as the tokens.

  Primary buttons keep the accent with white text, which the existing rule
  already holds at 4.5:1 in both modes. **Branding refuses an accent** for
  which any light or dark derivation fails, with a message naming the mode.
  The Branding page previews the accent on both grounds.
- **Charts** use the status and accent tokens, so they change with the
  mode. The container-query sizes for charts are fixed in this part (brief
  §6).
- **The seal** sits on the sidebar, which is dark in both modes; no change.
  On the sign-in page, a seal on the dark surface gets a light ring so a dark
  seal stays visible.
- **Print** is always light.
- **Covers** every page in the shell, sign-in and the error page.

## 8. Activity feed (part 8)

Today's D-095 put "Recently updated" on the dashboard instead of a feed,
because a feed built from the audit log could expose it. This feed is built
so it cannot: it shows a viewer only what they could already find on pages
they can open.

- **Where:** a "Recent activity" panel on the dashboard, replacing "Recently
  updated", with the 10 newest items. There is no separate activity page
  (O-2).
- **Which events:** a fixed list in `pdms-core`, not the whole audit log:
  - only events about a document (`document_id` set), with outcome
    `success`;
  - only these kinds: document created; new file version; details changed;
    status changed; review submitted; a review step decided; review
    finished or cancelled; signature recorded or declined; comment posted.
    The exact action names are listed in the plan. Everything else (views,
    downloads, exports, permission and grant changes, admin actions, denied
    or failed attempts, comment edits and deletions) is never shown.
- **Who sees what:**
  - an event is shown only if the viewer may **view the document now**,
    through the same visibility and grant checks as the document page
    (D-085, D-090), evaluated for the current state, not the state at the
    time of the event;
  - the actor's name is shown only for kinds whose actor the document page
    already shows to that viewer: new versions, comments, workflow steps and
    signatures. "Details changed" and "status changed" show no name, because
    the full history (where those names are) is limited to the owner,
    managers and auditors;
  - nothing from the event's changes or context is shown: no before and
    after values, IP addresses, user agents or request IDs.
- **Each item:** a fixed phrase ("New version", "Comment by Pat Lee"), the
  document's ID and title as a link, the department, and a relative time.
- **Empty:** "No recent activity on documents you can see."
- **Not audited,** like the rest of the dashboard: viewing it is not an
  audit search.
- **Tests:** fixtures with a document in another department, a County-wide
  one, a Restricted one, and one whose visibility was narrowed after the
  event. Each event kind on the list shows; every other kind does not; the
  name rule holds; no changes or context appear in the HTML.
- **Decision:** a new decision amends D-095 (§12).

## 9. Browser-tab icon (part 8)

- When a seal is set, every page (shell, sign-in, error page, and the
  inline `/source` and `/license` pages) links it as the icon:
  `<link rel="icon" href="/branding/seal?v=…">`. The seal is already public
  (D-093), so nothing new is exposed. Browsers scale it; PADM does not
  resize images.
- Without a seal, the icon is a generic "PADM" mark shipped in the binary as
  `static/icon.svg`, served like the other static files with long cache
  headers.
- The version parameter changes with every seal change, so tabs update.
- **Tests:** the link is present on each page type, with and without a seal;
  `/static/icon.svg` is served as `image/svg+xml`.

## 10. Constraints

R1's constraints apply unchanged (brief §5): strict CSP, no JavaScript
required, WCAG 2.1 AA, one `<h1>` per page, permissions never widened,
behaviour preserved, a public repository, self-hosted assets. In addition:

- **Behaviour changes in R2 are only these:** the overview's "Other" count
  and overdue label (§6.3); the activity feed (§8); the tab icon (§9); dark
  mode as the default, the per-person switch, and the dark accent check on
  Branding (§7); the account link text (§6.7). Everything else is
  presentation.
- **One migration,** for the per-person color scheme (§7), in part 9. Its
  number is the next free one on `main` and in open PRs when it is built.
- **Colors only through tokens,** from part 1 on, so that dark mode needs no
  layout changes.

## 11. Testing

- **Keep green:** `shell.rs`, `csp_guard.rs`, `web.rs`, `documents.rs`,
  `schedule.rs`, `phase3.rs`, `phase4.rs`, `phase5.rs`, `branding.rs`,
  `entra.rs`, `oidc.rs`.
- **New, per part:**
  - the admin menu: each group and item appears only with its permission;
    `aria-current` on the current page; P4's item slots in;
  - key markup of each redesigned page (header, panels, empty states);
  - every form keeps its `action`, field names and CSRF token (a test that
    lists them per page, before and after);
  - appointments: one seat-state mapping, the same labels on the dashboard
    and `/positions`;
  - the overview totals (§6.3); the feed (§8); the tab icon (§9);
  - dark mode: a test parses the token blocks in `app.css` and checks every
    text pair at 4.5:1 and every mark at 3:1 in both modes, and that the
    `dark` and `system` blocks match; the theme derivation passes for the
    sample accents in both modes, and Branding refuses one that fails;
  - the switch: a new user gets `data-theme="dark"`; each choice saves,
    renders and is audited; the sidebar button flips dark and light and
    returns to a local page only (an outside URL in `back` goes to the
    dashboard); a person cannot change someone else's setting; the sign-in
    page is dark.
- **Screenshot tool** (`tools/ui-screenshots/`): every changed page at
  1440×900; calendar, notifications, appointments and account also at
  390×844. From part 9 on, the tool shoots both modes (it gains a
  `--theme dark|light` option that sets the signed-in user's choice), and
  dark is checked first because it is the default. No sideways
  scroll, no cut-off text, one `<h1>`, no console errors.
- **Demo data** gains what empty screens need: a webhook with deliveries, a
  position in holdover, a vacant seat, an edited comment, and a few feed
  events. Each new screenshot page gets its records.
- **Keyboard pass** on every changed page, including the admin menu
  disclosure below 900 px.

## 12. Documentation and decisions

In each part's PR: ROADMAP §0.3 row 5b; ADMIN_GUIDE where a screen's
wording or steps changed (admin navigation in part 4, records requests in
part 5, dark mode, the switch and the accent check in part 9); README if it
describes old screens; ARCHITECTURE for the new column in part 9.

**New decisions,** recorded in `docs/DECISIONS.md` in this spec's PR once
the owner approves. They take the next free numbers on `main` and in open
PRs (D-097 onward on 2026-10-22; plans P3 and P4 renumber at their
pre-flight):

- R2 scope, order and admin navigation (the answers in §2), with a dated
  update to D-092;
- dark mode: the default for everyone, with a per-person switch (dark,
  light, match my device), its own checked palette, and the accent check in
  both modes;
- the activity feed, on the dashboard only: the event list, the visibility
  and name rules; amends D-095;
- the browser-tab icon from the seal, with a generic fallback;
- the overview counts: "Other" excludes superseded, and the overdue tile's
  label.

## 13. Questions still open for the owner

None. O-1 to O-3 are answered in §2; the owner confirms them in review.
