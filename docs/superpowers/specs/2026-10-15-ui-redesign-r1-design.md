# PDMS UI redesign, round 1 (R1): design

- **Date:** 2026-10-15
- **Status:** approved by the owner, then built and merged to `main` on
  2026-10-21. ROADMAP §0.3 row 5a links the plan. This spec now serves as the
  reference for the design system that R2 builds on.
- **Direction:** "D". The project owner chose it from four demo directions
  (A Civic Modern, B Clean Workspace, C Executive Overview, D = A's layout in
  C's style). The demos are mockups only and are not part of this repository.
- **Roadmap:** sub-project 5 ("UI redesign") is split into R1 (this spec) and
  R2 (the remaining pages). The owner moved R1 ahead of sub-projects 1–3 so
  there is a presentable shell to show county leadership.
  - R2 was planned to come after 1–3. On 2026-10-21 the owner started it in
    parallel instead; see the [R2 brief](2026-10-21-ui-redesign-r2-brief.md)
    and D-092's update.

## 1. Purpose

Make PDMS look like a finished county product, using the real data and
permissions. The first audience is upper management. After that, R1 is the
foundation every later screen builds on.

**R1 is done when:**

- every page uses the new frame and visual style;
- the five key screens below match direction D;
- an administrator can re-brand PDMS (county name, accent color, seal)
  without a developer;
- local accounts can sign in with a plain username such as `sysadmin`;
- all existing tests still pass, and the new behavior has its own tests.

## 2. Scope

**In R1:**

1. A new app shell (sidebar, top bar, page frame) and visual system for every
   page.
2. A branding settings page (Administration → Branding).
3. Sign-in page redesign, plus username sign-in for local accounts.
4. Dashboard redesign.
5. County overview at the top of Reports.
6. Documents list redesign.
7. Document page redesign.
8. Shared components restyled, so the other ~45 pages pick up the new look
   while keeping their current layouts.

**Out of R1 (R2 or later; see §18):**

- page-by-page redesign of admin, calendar, appointments, records requests,
  notifications, audit log and account pages;
- dark mode;
- a true activity feed;
- a custom browser-tab icon;
- reworking how `install.sh` creates the first administrator, beyond adding
  the username prompt in §7.

## 3. Constraints

- **Content Security Policy is unchanged:** `default-src 'self'; script-src
  'self'; style-src 'self'; img-src 'self' data:`. Therefore:
  - no `style="…"` attributes;
  - no inline `<script>` and no `on…=` handler attributes;
  - no CDNs and no Google Fonts.
  - Charts are server-rendered SVG whose colors come from CSS classes (§13).
- **Self-hosted assets.**
  - Fonts and icons ship inside the binary, the same way `app.css` and
    `htmx.min.js` are embedded with `include_bytes!` and served from
    `/static/…`.
  - **Font:** Source Sans 3 (SIL Open Font License 1.1), weights 400, 600
    and 700, WOFF2, Latin subset.
  - **Icons:** a small subset of Lucide (ISC license), as Askama partials.
  - License texts are committed next to the assets and listed in a
    `THIRD_PARTY_NOTICES.md` at the repository root.
- **No JavaScript is required** for any R1 feature. HTMX keeps working where
  it is already used. The narrow-screen menu uses `<details>`.
- **Accessibility:** WCAG 2.1 AA.
  - Text contrast is at least 4.5:1; non-text UI and chart marks are at least
    3:1.
  - Everything is reachable by keyboard and focus is always visible.
  - The skip link stays, and status is never shown by color alone.
- **Permissions:**
  - Every number, list and chart shows only what the signed-in person may
    see, using the existing visibility and grant rules (D-085, D-090).
  - Nothing in R1 widens access.
- **Public repository:**
  - Defaults use placeholder branding (`[County Name]` comes from config;
    there is a generic "PDMS" mark).
  - No real county seal, internal hostnames or addresses are committed.

## 4. Visual system

All colors are CSS custom properties on `:root` in `app.css`, except the
accent family, which comes from `/theme.css` (§6.4).

| Token | Value | Use |
|---|---|---|
| `--nav-bg` | `#15202B` | Sidebar background |
| `--nav-active` | `#24364A` | Current sidebar item |
| `--nav-fg` | `#C9D3DE` | Sidebar text (10.9:1) |
| `--nav-muted` | `#AEB9C6` | County name, department under user name (8.3:1) |
| `--nav-label` | `#8C9AAB` | "Manage" group label (5.8:1) |
| `--nav-divider` | `#2C3E52` | Sidebar dividers |
| `--nav-signout` | `#F5B76B` | Sign out link in the sidebar (9.3:1) |
| `--ground` | `#F3F5F8` | Page background |
| `--surface` | `#FFFFFF` | Panels, tables, inputs |
| `--ink` | `#15202B` | Body text (15.1:1 on ground) |
| `--muted` | `#4A5868` | Secondary text (6.7:1 on ground) |
| `--border` | `#DCE2E9` | Panel borders (decorative) |
| `--row` | `#EEF1F4` | Table row dividers |
| `--thead` | `#F7F9FB` | Table header background |
| `--control-border` | `#7D8A9A` | Input, select and secondary-button borders (3.5:1) |
| `--focus` | `#1F6FEB` | 3px focus outline, 2px offset (4.6:1 on white, 3.6:1 on navy) |

**Status colors are reserved for status.** Each is always paired with a word.

| Status | Dot or mark | Text | Tint (badge background) |
|---|---|---|---|
| Critical (overdue, decision overdue) | `#C4352A` | `#A01B0F` | `#FBE7E5` |
| Warning (due soon, renewal due, holdover) | `#B86A00` | `#7A4300` | `#FFF1DC` |
| Info (in review, awaiting you, term ending) | `#2F64B5` | `#1D4F91` | `#E7EEF9` |
| Good (active, signed, current) | `#2F7D4F` | `#1E6B3F` | `#E5F3EA` |
| Neutral (draft, expired, vacant) | ring `#7D8A9A` | `#4A5868` | `#EEF1F4` |

**Accent family.** These are generated from the branding accent (§6.4). The
default accent is `#B45309`, which gives 5.0:1 with white text.

| Token | Rule |
|---|---|
| `--accent` | The configured accent: primary buttons, current tab underline, chart bars, notification count |
| `--accent-hover` | The accent darkened by 12% in OKLCH lightness |
| `--accent-link` | The accent darkened in 4% steps until it reaches at least 4.5:1 on `--ground`. Used for links. |
| `--accent-tint` | 12% accent over white. Used for highlight backgrounds, with `--ink` text. |

The default accent amber is close to the "warning" amber. That is acceptable
because status always carries a word, and the accent is expected to change
when the county chooses its branding.

**Typography:**

- Source Sans 3 is the font, with `system-ui` as the fallback.
- **Sizes:** body 15px/1.5; page title (h1) 30px/700; panel heading (h2)
  19px/700; small text 13px; count-tile numbers 38px/700.
- Numbers in tables and tiles use `font-variant-numeric: tabular-nums`.

**Shapes and spacing:**

- **Corner radius:** panels 8px; buttons and inputs 6px; badges and pills
  999px.
- **Spacing scale:** 4, 8, 12, 16, 22, 28, 40px.
- **Tap targets:** sidebar items are at least 44px tall; buttons at least
  40px.

**Shared components** (classes in `app.css`, used by every page):

- **Buttons:** `.button.primary` (accent background, white text),
  `.button` (white with `--control-border`), `.button.danger`,
  `.button.small`.
- **Form controls:** inputs, selects and textareas, with `--control-border`
  and the focus ring.
- **Panels and tiles:**
  - `.panel`: a white box, 1px `--border`, 8px radius;
  - `.panel-head`: heading row with an optional right-side link;
  - `.tile`: count tile, with an optional 3px top stripe in a status color
    or the accent.
- **Tables:** a compact table (`thead` on `--thead`, rows divided by
  `--row`) inside `.table-wrap`, which scrolls horizontally on narrow
  screens.
- **Status indicators:**
  - `.status`: an inline dot plus word in the status text color;
  - `.badge`: a pill on a tint.
- **Navigation pieces:**
  - `.tabs`: a row of links with an accent underline on
    `aria-current="page"`;
  - `.breadcrumb`;
  - `.pager`.
- **Messages:** `.flash` (ok and error, using the good and critical tints).
- **Disclosure:** `.disclosure`, a styled `<details>`/`<summary>`.
- **Utilities:** `.visually-hidden`, `.muted`, `.small`.

## 5. App shell (`base.html`)

**Layout:** two columns: the sidebar (232px) and the content area. Below
900px the sidebar becomes a top bar, and its menu opens from a
`<details>` "Menu" toggle.

**Sidebar, top to bottom:**

1. **Brand block:**
   - the seal (`/branding/seal`, 38px), or the generic "PDMS" mark when no
     seal is set;
   - "PDMS";
   - the county name (§6.3).
2. **Main menu:**
   - Dashboard, Documents, Calendar, Appointments, Reports;
   - Notifications, with the unread count as an accent pill (screen-reader
     text "N unread").
   - The current item has `aria-current="page"` and the `--nav-active`
     background.
3. **"Manage" group,** shown only when it has at least one item:
   - Records requests (`can_exports`);
   - Administration (`can_admin`);
   - Audit log (`can_audit`).
4. **User block, pinned to the bottom:**
   - initials circle, display name, and the person's department under it
     ("N departments" if more than one);
   - links: API tokens (`/account/tokens`) and Sign out. Sign out stays a
     POST form with the CSRF token.

**Top bar** (white, bottom border):

- a search form: `GET /documents`, field `q`, placeholder "Search titles,
  IDs, tags, and text inside files";
- a **New document** primary button when the person may create documents
  (the same check as today's `can_create`).

**Admin pages:** the current admin sub-navigation becomes a `.tabs` row at
the top of the content area. It keeps the same items and permission checks.

**Content area:**

- the flash message, then the page heading (h1);
- on R1's five screens the h1 comes from each page's own header block, so
  the shared `<h1>` in `base.html` becomes overridable per page;
- the footer line "Source code · License: AGPL-3.0-or-later" stays at the
  bottom of the content area.

**Print:** `@media print` hides the sidebar, top bar and buttons, so pages
print cleanly.

The page `<title>` stays "{page title} – {county name}".

## 6. Branding settings

### 6.1 Access and location

- Administration → **Branding** tab, at `/admin/branding`.
- Requires `Capability::ManageSettings`, which is already exposed as
  `can_settings`.

### 6.2 Fields

| Field | Rule | Empty means |
|---|---|---|
| County name | Trimmed, 1–120 characters | Use `[org] name` from the config file (today's behavior) |
| Accent color | `#RRGGBB` (the color input or typed hex), and **white text on it must reach at least 4.5:1** | Default `#B45309` |
| Seal | PNG or JPEG, at most 512 KiB, each side 16–2048 px | The generic "PDMS" mark |

**Actions:**

- **Save** (name and accent);
- **Upload seal;**
- **Remove seal;**
- **Reset to defaults** (clears all three, with a confirm step: a second page
  or `<details>`, no JS).

**Validation messages:**

- **Accent too light:** "White text on #F5D76E is hard to read (2.1:1; at
  least 4.5:1 is needed). Choose a darker shade."
- **Wrong file type:** "The seal must be a PNG or JPEG image."
- **Too large:** "The seal must be 512 KB or smaller."
- **Wrong dimensions:** "The seal must be between 16 and 2048 pixels on each
  side."

**Preview:** the page shows a static preview (a sidebar strip and one primary
button) in the saved colors.

### 6.3 Storage and use

- **Name and accent:** the existing, currently unused `app_settings` table,
  under the keys `branding.org_name` and `branding.accent` (JSON strings).
- **Seal:** a new table `branding_assets`, in migration 0009:
  ```sql
  CREATE TABLE branding_assets (
      name         text PRIMARY KEY CHECK (name IN ('seal')),
      content_type text NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg')),
      bytes        bytea NOT NULL CHECK (octet_length(bytes) <= 524288),
      sha256       text NOT NULL,
      width        integer NOT NULL,
      height       integer NOT NULL,
      updated_by   uuid REFERENCES users (id),
      updated_at   timestamptz NOT NULL DEFAULT now()
  );
  ```
  The seal lives in PDMS's database, not SharePoint, so branding works even
  when SharePoint is unreachable.
- **One resolver for the organization name:** the setting if set, else the
  config value. Every place that shows the organization name uses it: page
  titles, the sidebar, the sign-in page, notification emails, calendar
  feeds.
- **Caching:** the web process keeps branding in memory and refreshes it
  immediately after a save and otherwise at most every 60 seconds. The worker
  reads it when it sends.

### 6.4 Delivery

- **`GET /theme.css`** (public, no sign-in needed):
  - returns `:root { --accent: …; --accent-hover: …; --accent-link: …;
    --accent-tint: …; }`;
  - `base.html` and `login.html` link it as `/theme.css?v={version}`, where
    the version changes on every branding save;
  - it is served with `Cache-Control: public, max-age=31536000, immutable`.
- **`GET /branding/seal`** (public):
  - the stored bytes, with the stored `Content-Type`,
    `X-Content-Type-Options: nosniff`, and `Content-Disposition: inline;
    filename="seal.png"` (or `.jpg`);
  - versioned as `?v={sha256 prefix}`, with the same cache header;
  - **404** when no seal is set; the templates then show the generic mark
    instead of linking.
- The seal and county name appear before sign-in, so they are public to
  anyone who can reach PDMS. The page says so under the upload field.

### 6.5 Upload checks

- **The real type** comes from the file's first bytes (the PNG signature
  `89 50 4E 47 0D 0A 1A 0A`, or JPEG `FF D8 FF`), never from the browser's
  `Content-Type` or the file name.
- SVG, GIF, WebP and everything else are refused.
- **Dimensions** come from the PNG `IHDR` chunk or the JPEG `SOFn` marker. A
  file whose header cannot be parsed is refused.
- The upload is a multipart POST protected by the CSRF token.

### 6.6 Audit

Every change writes an audit entry, using the existing audit naming
convention:

| Event | What is recorded |
|---|---|
| `branding.updated` | Before and after name and accent |
| `branding.seal_set` | sha256, size, width, height (not the bytes) |
| `branding.seal_removed` | — |
| `branding.reset` | — |

## 7. Sign-in page and usernames

### 7.1 Page (`login.html`)

Two panels:

- **Left (navy):**
  - the seal or mark, and the county name;
  - the headline "Know what's due, what's overdue, and who owns it.";
  - three short points:
    - "Every document — Policies, contracts, agreements, grants";
    - "Every deadline — Reviews, renewals, expirations, terms";
    - "Every action — Recorded in a tamper-evident log";
  - the footer links "Source code · License: AGPL-3.0-or-later".
- **Right (light):**
  - the heading "Sign in", and any error or notice message;
  - **Username** and **Password** fields;
  - **Authenticator code**, shown when `[auth.local] require_totp` is on
    (today's rule);
  - the **Sign in** primary button;
  - an "or" divider;
  - **Sign in with Microsoft**, a secondary button.

**Rules:**

- The page has no help line.
- **When local sign-in is disabled in config,** the username and password
  form and the divider are hidden; only the Microsoft button shows.
- **"Sign in with Microsoft" is always visible.**
  - When Microsoft sign-in is configured, it links to
    `/auth/oidc/start?next=…` as today.
  - When it is not configured, `/auth/oidc/start` still exists and answers
    with a `303` to `/login?notice=sso_off`.
  - The sign-in page maps that code to the fixed text "Microsoft sign-in
    isn't set up yet. Contact your administrator."
  - Unknown notice codes are ignored. Request text is never echoed back.

### 7.2 Usernames

**Schema** (migration 0009):

```sql
ALTER TABLE users ADD COLUMN username text
    CHECK (username ~ '^[A-Za-z0-9._-]{2,64}$');
CREATE UNIQUE INDEX users_username_key ON users (lower(username))
    WHERE username IS NOT NULL;
```

**Username rules:**

- 2–64 characters: letters, digits, `.`, `_`, `-`.
- They can never contain `@`, so a username is never confused with an email.
- Comparison ignores case.
- Optional; every account still has an email, which notifications need.

**Sign-in:**

- The local sign-in form field is renamed from `email` to `username`.
- **Lookup:** if the value contains `@`, match `lower(email)`; otherwise
  match `lower(username)`.
- All existing checks then apply unchanged: local credentials exist, the
  account is active, the argon2id password, TOTP when required, and the
  existing rate limits and lockout.
- **One generic failure message** for every failure: "Username or password
  is incorrect." This covers an unknown account, a wrong password, a
  disabled account, and an account without local credentials.
  Authenticator-code checking is unchanged.
- **Equal timing:** when no account matches, the code still runs one
  argon2id verification against a fixed dummy hash, so response time does
  not reveal whether an account exists. If the code already does this, keep
  it.
- Successful and failed sign-ins are audited as today. The audit records the
  submitted identifier the same way it records the email today.

**Setting usernames:**

- **Admin page:** the Users page for an account **with local credentials**
  gets a Username field.
  - Saving it is audited as `user.username_set` (before and after).
  - A duplicate shows "That username is already in use."
- **CLI:**
  - `pdms create-admin` gains `--username <name>`;
  - a new `pdms set-username --email <email> --username <name>` command sets
    it for an existing account. It is audited with the system actor.
- **`install.sh`:**
  - the first-administrator step asks "Username [sysadmin]" (environment
    variable `PDMS_INSTALL_ADMIN_USERNAME` for unattended installs) and
    passes it to `create-admin`;
  - the final summary prints the username;
  - email is still asked for.
- **Accounts that come from Entra do not get usernames.** They sign in with
  Microsoft.

## 8. Dashboard (`/`)

- **Header:**
  - "Good morning, {first word of display name}": morning before 12:00,
    afternoon before 17:00, evening otherwise, in the configured time zone;
  - the date line "Thursday, October 15, 2026";
  - on the right, a **County overview** link to `/reports`.
- **Count tiles,** in order:

| Tile | Number | Subtext | Stripe | Links to |
|---|---|---|---|---|
| Overdue | Everything overdue the person can see (documents and appointments; today's "Overdue" list) | "Oldest: N days" | Critical | `/reports/overdue` |
| Due in 30 days | Upcoming items within 30 days that the person can see | "Next: {date}" | Warning | `/reports/upcoming` |
| Awaiting your approval | `workflow::my_pending` count | "Workflow steps waiting on you" | Info | `#attention` |
| Your reviews and renewals | `reviews::my_open` count | "On documents you own" | Accent | `#attention` |

- **Needs your attention** (`id="attention"`): a table with the columns
  Status, Item, Due, and Action.
  - **Rows:**
    - your open review and renewal tasks;
    - your pending approvals.
  - **Order:**
    1. overdue tasks, most days late first;
    2. approvals, oldest submission first;
    3. tasks due within 30 days, soonest first;
    4. remaining tasks, soonest first.
  - Every row is shown, because these lists are per person and short.

  | Row | Status | Item line | Due | Action |
  |---|---|---|---|---|
  | Overdue task | Critical "Overdue" | "{Kind}: {title}", plus ID | "{N} days late" + date | **Start review** (primary) → `/documents/{id}#reviews` |
  | Approval | Info "Awaiting you" | "{Step kind}: {title}", plus "{ID} · submitted by {name}" (`submitted_by`, or "system") | "Submitted {date}" (`submitted_at`) | **Review** → `/documents/{id}#workflow` |
  | Task due ≤ 30 days | Warning "Due soon" | "{Kind}: {title}", plus ID | "In {N} days" + date | **Start review** for reviews; **Open** for renewals → `#reviews` |
  | Other task | Neutral "Upcoming" | as above | "In {N} days" + date | **Open** |

  - **Empty state:** "Nothing needs your attention."
- **Coming up:**
  - the next up to 8 items from today's upcoming list (everything the person
    can see, D-090);
  - the existing range choices 30, 60, 90 and 365 days stay, as `.tabs`;
  - each row shows a navy date block, the title (linked), and "{label} ·
    {department}";
  - a **Calendar** link;
  - **empty state:** "Nothing scheduled in the next N days."
- **Boards and commissions:**
  - shown only to people who can view appointments (the same check as
    `/positions`);
  - a new core function lists the seats the person can see that are:
    - a decision overdue (critical);
    - a holdover (warning);
    - vacant (neutral);
    - a term ending within 90 days (info, "Term ends {date}").
  - Up to 6 rows, worst first, with an **All appointments** link;
  - **empty state:** "All seats are filled and current."
- **Recently updated:**
  - up to 5 documents the person can see, excluding archived ones, newest
    `updated_at` first;
  - each row shows the title (linked), "{ID} · {department}", and on the
    right either:
    - "New version by {uploader}", when the newest version's `created_at`
      is within 60 seconds of the document's `updated_at`; or
    - "Details updated" otherwise;
  - plus a date: Today, Yesterday, or "Oct 12".
  - The uploader is already shown to viewers on the document page, so this
    reveals nothing new.
- **Removed from the dashboard:**
  - the "Notifications" panel (the sidebar has the count);
  - the "Your access" panel, which moves to the top of the account page
    (`/account/tokens`) as "Your access".

## 9. County overview (`/reports`)

The overview sits above the existing report list, which stays as it is.

- **Filters** (GET, one row):
  - **Period**, `months` = 6 or 12 (default 6). It applies to the
    due-by-month chart.
  - **Department**, `department` = a department the person can see, or "All
    you can see". It applies to everything on the overview.
- **Tiles:**

| Tile | Number |
|---|---|
| Documents | Visible documents not archived or superseded; subtext "{N} types · {M} departments" |
| Overdue | As on the dashboard; subtext "Oldest: N days" |
| Due in 90 days | Upcoming items within 90 days; subtext "Next: {date}" |
| In review | Visible documents in status `in_review` |

- **Charts.** All are server-rendered SVG (§13), and each has a "View as
  table" link:
  1. **Due by month:** bars per month for the period; one series in the
     accent; the count above each bar; one category axis. Table: the
     existing `upcoming` report.
  2. **Status breakdown:** one stacked horizontal bar of visible documents
     by status.
     - **Segments:** Active (good), In review (info), Renewal due (warning),
       Overdue (critical), and Other (draft, expired, superseded; neutral).
     - 2px gaps between segments.
     - A legend lists each status with its count.
     - Table: the documents list.
  3. **Overdue by department:** horizontal bars, one per department with
     overdue items, in the critical color; the count at the end of each
     bar. Table: the existing `overdue` report.
  4. **Boards summary:** a small table of seat counts. The rows are Filled
     and current, Term ending within 90 days, Holdover, Vacant, and
     Decision overdue. Link: `/positions`.
- **Data:**
  - Everything comes from the existing visibility-filtered functions: the
    calendar dashboard and expansion, document search, and the report
    queries.
  - The only new query is the status count shared with the documents list
    (§10).
  - If a function's existing document cap is reached, the overview says
    "Based on the first N documents you can see."
- **Who sees it:** everyone, with their own data.
- **For leadership to see the whole county,** an administrator gives their
  account view access to all departments with an existing global grant.
  That is a setup step (§16), not a code change.

## 10. Documents list (`/documents`)

- **Header:** the h1 "Documents"; on the right, **Export CSV** (the existing
  `/documents/export.csv?{query}`) and **New document** (when allowed).
- **Status tabs** (`.tabs`, links that keep every other query parameter):
  - All, Active, In review, Renewal due, Overdue, Draft, Expired, each with
    its count;
  - the counts come from one new grouped count query, using the same
    visibility rules and all current filters except status;
  - "All" means today's default: everything except archived.
- **Filter row:** Search (`q`), Type, Department, and an Apply button.
- **More filters** (a `.disclosure`, open when any of its fields is set):
  - date field and range, custom field and value, sort, direction;
  - a Status select that keeps Archived and Superseded reachable.
  - Nothing that can be filtered today is lost.
- **Table** (inside `.table-wrap`), with the columns ID, Title (linked),
  Department, Owner, Status (`.status`), **Next date**, Visibility.
  - **Next date:**
    - if a review or renewal is overdue, show "Overdue · {date}" in the
      critical text color;
    - otherwise, show the earliest future date among next review, renewal
      and expiration, labeled "Review ·", "Renewal ·" or "Expires ·";
    - otherwise "—".
  - **Visibility:** "Department", "County-wide" or "Restricted". The last
    two are in bold.
- **Footer:** "Showing X–Y of N", then the existing previous and next pager.
- **Empty state:** "No documents match these filters.", plus a **Clear
  filters** link.

## 11. Document page (`/documents/{id}`)

Everything the current page does stays; it is reorganized.

- **Header:**
  - a breadcrumb ("Documents / {ID}");
  - badges: type, status (tinted), visibility (County-wide or Restricted
    only), Legal hold;
  - the title as h1, with the description beneath it;
  - **buttons:**
    - **Start review** (primary) appears when the signed-in person owns the
      document's open review task and that task is due within 30 days or
      overdue. It links to `#reviews`.
    - **Download {file}** is the primary button when Start review is
      absent; otherwise it is secondary.
    - **Edit details** (when allowed).
    - **History** (when `can_timeline`).
  - **Section links** ("On this page"): Key dates · Review & approval ·
    Files · Comments · Manage. Only sections that exist are shown.
- **Key numbers** (tiles; only the ones that apply):
  - **Days overdue,** in the critical color, with "{Kind} was due {date}".
    When nothing is overdue, show **Days until {kind}** with the date
    instead.
  - **Missed review cycles,** only when > 0.
  - **Versions:** the count, with "Latest {date}".
  - **Signatures:** "{signed} of {required}", only when the document type
    tracks signatures. Subtext "{N} pending" or "All signed".
- **Main column, in order:**
  1. **Key dates** (`id="dates"`):
     - **Timeline:** an SVG timeline from the earliest event to
       max(today, latest future event), marking:
       - the effective date;
       - each version upload;
       - each missed review cycle (critical ring, "Missed");
       - the next review, renewal and expiration;
       - today.
     - A plain ordered list of the same events goes beneath. The list is the
       accessible version; the SVG is `aria-hidden`.
  2. **Review & approval** (`id="reviews"`, `id="workflow"`): the existing
     workflow include, then the reviews-and-renewals section.
  3. **Files:** the version table (file, version, size, uploaded, by,
     download), and the upload form when allowed.
  4. **Signatures,** when tracked: the signer, role and state ("Signed
     {date}" good, or "Awaiting signature" warning).
  5. **Comments:** the existing `collab.html` include, under the D-091 rules.
- **Side panel:**
  - **Details:**
    - owner, department, who can see it, custom fields, review cycle,
      expires;
    - **Stored in,** only once SharePoint storage (sub-project 1) is active.
  - **Tags:** the tag list, and the add form when allowed.
  - **Status:** the "Move to …" buttons, when allowed.
  - **Reminders:** the existing notification-rules section.
- **Manage this document** (`.disclosure`, closed by default, at the
  bottom):
  - contains **Who can see it**, **Access**, **Primary record owner**,
    **Retention** and **Delete**;
  - each part appears only to people allowed to use it;
  - the whole disclosure is omitted when none apply.
- **The "Share" button from the demo is not built.** PDMS has no sharing
  feature; access comes from visibility and grants.

## 12. Other pages

- Every other template keeps its markup and gets the new look through the
  shell (§5) and shared components (§4).
- **Light-touch template edits** are allowed where a page uses classes that
  no longer exist:
  - wrapping wide tables in `.table-wrap`;
  - swapping old badge classes for `.status` or `.badge`.
  - No layout or content changes.
- `error.html` is restyled.
- The inline HTML pages served from `routes/ops.rs` (license, source) keep
  linking `app.css` and pick up the fonts.

## 13. Charts

- **Rendering:** Askama templates emit inline `<svg>`. The geometry is
  computed in Rust view structs (scales, bar sizes, label positions).
- **Colors come only from CSS classes,** for example
  `.chart-bar { fill: var(--accent); }` and
  `.chart-seg-critical { fill: #C4352A; }`.
- Geometry uses SVG presentation attributes (`x`, `y`, `width`, `height`,
  `rx`). These are not inline styles, so the CSP is satisfied.
- **Mark style:**
  - bars have 4px rounded ends at the data end only, anchored to the
    baseline;
  - 2px gaps between stacked segments;
  - gridlines are light and few.
  - A chart with a single series has no legend; the status breakdown has
    one.
- **Accessibility and hover:**
  - each chart `<svg>` has `role="img"` and an `aria-label` summarizing the
    data;
  - each mark has a `<title>` ("November 2026: 3 due"), which gives a native
    hover tooltip;
  - the "View as table" link is the full accessible alternative.
- **No chart library and no JavaScript.**

## 14. Summary of code changes

| Area | Change |
|---|---|
| Migration `0009_ui_redesign.sql` | `branding_assets` table; `users.username` column and index |
| `pdms-core` | Branding service (get, set, validate accent contrast, seal checks, org-name resolver, audit); username lookup in local sign-in; `set_username`; appointments "needing attention"; documents "recently updated"; grouped status counts; overview aggregation helpers |
| `pdms` CLI | `create-admin --username`; `set-username` |
| `pdms-web` routes | `/theme.css`, `/branding/seal`, `/admin/branding` (GET, POST), `/admin/branding/seal` (POST upload, POST remove), `/admin/branding/reset` (POST); `/static/fonts/{file}`; `/auth/oidc/start` always registered; `/reports` overview; dashboard, documents list and document page view models |
| Templates | `base.html`, `login.html`, `dashboard.html`, `documents/list.html`, `documents/show.html`, `reports/index.html`, `admin/branding.html` (new), `admin/user.html` (username field), `account_tokens.html` ("Your access"), icon partials, chart partials; light-touch edits elsewhere (§12) |
| Static | New `app.css`; Source Sans 3 WOFF2 files and `OFL.txt`; Lucide icon license |
| `install.sh` | Username prompt for the first administrator |
| Repo root | `THIRD_PARTY_NOTICES.md` |

The public routes (`/theme.css`, `/branding/seal`, `/static/fonts/…`,
`/login`, `/auth/oidc/start`) must be reachable without a session.

## 15. Testing

All existing tests keep passing; tests that post the old `email` sign-in
field are updated to `username`. New automated tests:

1. **Branding:**
   - only `ManageSettings` holders can view or change it;
   - a too-light accent is refused with the contrast message;
   - a valid accent changes `/theme.css` and its version parameter;
   - name fallback to config works;
   - every change writes its audit entry.
2. **Seal:**
   - a PNG and a JPEG are accepted;
   - SVG, GIF, a renamed text file, an oversize file, too-small or too-large
     dimensions, and an unparseable header are refused;
   - `/branding/seal` sends `nosniff` and the stored type;
   - it returns 404 when unset.
3. **The theme and its derived colors:**
   - `--accent-link` reaches at least 4.5:1 on `--ground` for the default
     and for every accent in a sample set;
   - `/theme.css` and `/branding/seal` work without a session.
4. **Usernames:**
   - sign-in works by username and by email, regardless of case;
   - a duplicate username is refused (case-insensitive);
   - `@` in a username is refused;
   - unknown user, wrong password and disabled account all give the same
     message;
   - an account without local credentials cannot sign in locally;
   - `user.username_set` is audited;
   - the CLI `set-username` works.
5. **Microsoft button:**
   - it is shown when Microsoft sign-in is disabled;
   - `/auth/oidc/start` redirects to `/login?notice=sso_off`, and the page
     shows the fixed message;
   - an unknown notice code shows nothing.
6. **Visibility on the new data:**
   - status counts, "Boards and commissions", "Recently updated", and the
     overview tiles and charts never include documents or seats the viewer
     cannot see;
   - each has a fixture with a document in another department, a
     County-wide one, and a Restricted one.
7. **Dashboard ordering:**
   - "Needs your attention" ordering and status mapping;
   - the greeting changes with the time of day (with an injectable clock).
8. **Documents list:** the "Next date" rules (overdue, earliest future, none)
   and the tab links keeping other filters.
9. **CSP guard:** a test scans every template, plus the inline HTML in
   `ops.rs`. It fails on:
   - any `style=` attribute;
   - any `<script>` without `src`;
   - any `on…=` attribute.
   The failure lists file and line.
10. **Static assets:** the fonts are served with `font/woff2` and long cache
    headers.

**Manual checks before merge:**

- **Screenshots** of the five screens at 1440×900 and 390×844, and of every
  other page at 1440×900, taken with a headless browser. No overlap, no cut
  text, no horizontal page scroll.
- **A keyboard-only pass** through the sidebar, the menu toggle, the
  sign-in form, the filters and the "Manage this document" disclosure.

## 16. Rollout and demo setup

1. Build on a feature branch in a worktree using subagent-driven
   development, with a task review per task and a final whole-branch review.
2. Fast-forward merge to `main` and push, after the owner approves.
3. **Test server:** `git pull`, rebuild and restart.
   - Migration 0009 only adds things. **No data is lost.**
4. **After the deploy, on the test server:**
   - Give the administrator account a username:
     `pdms set-username --email <admin email> --username sysadmin`.
   - On Administration → Branding, set the county name, an accent and a
     seal. Placeholders are fine until the county decides.
   - Give each leadership demo account view access to all departments with
     a global grant, so the county overview covers the whole county.
5. These steps also go into the admin guide (§17), so a partner can repeat
   them.

## 17. Documentation updates

- **`docs/ROADMAP.md` §0.3:**
  - split row 5 into **5a R1** (this spec; status as built) and **5b R2**
    (the remaining pages, after 1–3);
  - note the owner's decision to do R1 first.
- **`docs/DECISIONS.md`:**
  - **D-092:** direction D and the R1 scope and order;
  - **D-093:** branding is set on an admin page (name, accent with a
    contrast rule, PNG/JPEG seal) and is public before sign-in;
  - **D-094:** sign-in page layout (username and password first; Microsoft
    always visible with a "not set up" notice), and usernames for local
    accounts;
  - **D-095:** the dashboard shows "Recently updated" instead of an activity
    feed, and there is no "Share" button.
- **`docs/ADMIN_GUIDE.md`:**
  - a new "Branding" section;
  - usernames for local accounts (admin page, CLI, `install.sh`);
  - "Preparing a demo for leadership" (§16, step 4).
- **README:** the screenshots or the status line, if they describe the old
  UI.

## 18. Later (not in R1)

- **R2:** page-by-page redesign of the remaining screens (admin, calendar,
  appointments, records requests, notifications, audit log, account,
  report detail).
- Dark mode, with its own validated palette.
- An activity feed with per-viewer filtering of audit events.
- A browser-tab icon generated from the seal.
- **The first administrator account:** the owner wants to change how
  `install.sh` creates it, beyond the username prompt in R1. They will
  describe the change later. This could include making email optional for
  local-only accounts.
