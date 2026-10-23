# UI redesign R1 (direction D) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put PDMS in its new direction-D look on every page. That means:

- a navy sidebar shell with Source Sans 3;
- an accent color the admin can change;
- redesigned sign-in, dashboard, documents list, document page and county
  overview;
- a branding admin page;
- username sign-in for local accounts.

**Architecture:**

- **Visual system.** It lives in one rewritten `static/app.css`, with tokens
  and shared components. Fonts and icons are embedded with `include_bytes!`
  or as Askama partials.
- **Shell.** `base.html` becomes the sidebar shell.
- **Branding.**
  - Data lives in `app_settings` and a new `branding_assets` table, behind a
    new `pdms_core::branding` module.
  - The web process caches it in `AppState` and refreshes it from a
    middleware.
  - `/theme.css` serves the accent family; `/branding/seal` serves the seal.
- **Usernames.** A new `users.username` column. The local sign-in lookup
  branches on `@`.
- **Pages.** New view-model code feeds the redesigned pages. Charts are
  server-rendered SVG whose colors come from CSS classes, because the CSP
  stays strict.

**Tech Stack:** Rust 2024, Axum 0.8, SQLx 0.9 (`query!` with `.sqlx/` offline
data), Askama 0.16, PostgreSQL 15/16, HTMX (unchanged), bash (`install.sh`).

**Spec:** [docs/superpowers/specs/2026-10-15-ui-redesign-r1-design.md](../specs/2026-10-15-ui-redesign-r1-design.md). Every task's requirements include the spec section it names.

**Local paths:** `$R1_SCRATCH` is a machine-local working directory, outside
the repository, that holds the mockups, the staged assets and the screenshot
workspace. Each task dispatch gives its value; run
`export R1_SCRATCH=<value>` first.

**Visual references:** the approved direction-D mockups are in
`$R1_SCRATCH/pdms-demos/project/`:
`D-SignIn.dc.html`, `D-Dashboard.dc.html`, `D-Documents.dc.html`,
`D-Document.dc.html`, `D-Overview.dc.html`.

- **They are mockups.** They use inline `style=""` attributes and `{{accent}}`
  placeholders. Translate them into CSS classes; never copy `style=""` into
  templates.
- **Where the spec and a mockup differ, the spec wins.** Examples: control
  borders `#7D8A9A`, the focus ring `#1F6FEB`, the "Your tasks" tile renamed
  to "Your reviews and renewals", and no "Share" button.

**Staged assets** (already downloaded, checksums verified) are in
`$R1_SCRATCH/assets-probe/`:

| File | What it is |
|---|---|
| `ss3-400.woff2` | Source Sans 3 Latin, weight 400 (sha256 `78e22177…3bd6`), from `@fontsource/source-sans-3@5.0.12` |
| `ss3-600.woff2` | Weight 600 (sha256 `ebc046fe…0e93`) |
| `ss3-700.woff2` | Weight 700 (sha256 `37ea2727…3c13`) |
| `fontsource-LICENSE` | SIL OFL 1.1 text shipped with that package |
| `lucide/package/icons/*.svg` | Lucide icons, `lucide-static@0.460.0` |
| `lucide/package/LICENSE` | The ISC license |

## Global Constraints

- **CSP is unchanged:** `default-src 'self'; script-src 'self'; style-src
  'self'; img-src 'self' data:`. Therefore:
  - no `style="` attributes, no inline `<script>` and no `on…=` attributes
    in any template or inline HTML;
  - SVG geometry uses presentation attributes (`x`, `y`, `width`, `height`,
    `d`, `rx`) and colors come from CSS classes.
  - The Task 1 guard test enforces this from Task 1 on.
- **No new runtime JavaScript.** HTMX stays where it is. The narrow-screen
  menu is a `<details>` element.
- **Accessibility:**
  - text contrast at least 4.5:1, UI and chart marks at least 3:1;
  - the focus ring is `3px solid #1F6FEB`, offset 2px;
  - control borders are `#7D8A9A`;
  - sidebar items are at least 44px tall, buttons at least 40px;
  - status is always a dot or badge plus a word.
- **Colors:** exactly the token values in spec §4. The default accent is
  `#B45309`.
- **Permissions:** every new list, count and chart is built from the
  existing visibility-filtered functions (`search::search`, `search::all`,
  `calendar::*`, `appointments::list_positions`, `reviews::my_open`,
  `workflow::my_pending`) or applies the same predicate. Nothing in R1
  widens access.
- **Migrations:** the new migration is `migrations/0009_ui_redesign.sql`,
  with no SPDX header (D-080). Never edit 0001–0008.
- **New Rust files** start with:
  ```rust
  // SPDX-License-Identifier: AGPL-3.0-or-later
  // Copyright (C) 2026 PDMS contributors
  ```
- **Lints:**
  - `unwrap`, `expect`, `panic` and `dbg` are denied outside tests;
    `unsafe` is forbidden;
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
    and `cargo fmt --all --check` must pass at every commit.
- **Offline query data:** after any `sqlx::query!` or migration change, run
  `sqlx migrate run --source migrations`, then
  `cargo sqlx prepare --workspace -- --all-targets`, and commit `.sqlx/`.
- **Audit:** audit events are written by **core** services inside the
  transaction with `pdms_audit::append(&mut tx, vec![e])`, using a local
  `event(ctx, Category::…, "action", "object_type")` helper, as in
  `crates/pdms-core/src/directory/admin.rs:14`. Action names are exactly
  `branding.updated`, `branding.seal_set`, `branding.seal_removed`,
  `branding.reset` and `user.username_set`.
- **Public repository:**
  - No real county names, seals, hostnames or IP addresses in code, tests,
    fixtures or docs.
  - Never write security-review finding IDs into the repository.
- **Dev environment** (run once per shell):
  ```bash
  export PATH="$HOME/.cargo/bin:$PATH"
  docker start pdms-dev-db
  export DATABASE_URL=postgres://pdms_owner:devowner@127.0.0.1:55432/pdms
  export PDMS_TEST_DATABASE_URL=postgres://postgres:devsuper@127.0.0.1:55432/postgres
  export PDMS_TEST_OWNER_PASSWORD=devowner PDMS_TEST_APP_PASSWORD=devapp
  ```

## Review Focus

These five inputs are implied by the spec and most likely to bite a real
user. Each has a test in its owning task.

1. **A pale county color.** An admin types a light brand color such as
   `#F5D76E`. It must be refused with the contrast message, and buttons must
   never get unreadable white-on-yellow text.
   (Task 2 `accent_too_light_is_refused`, Task 4
   `branding_page_refuses_light_accent`.)
2. **A seal that is not really an image.** An SVG renamed `seal.png`, or an
   HTML file, uploaded as the seal must be refused. A real PNG must be served
   back with `nosniff`.
   (Task 2 `sniff_refuses_svg_and_renamed_text`, Task 4
   `seal_upload_round_trip_and_headers`.)
3. **Typing `SysAdmin` or an email address.** Sign-in must accept the
   username regardless of case, and must still accept the email. A wrong
   username and a wrong password must give the identical message.
   (Task 5 `login_by_username_any_case_and_by_email`,
   `unknown_username_and_wrong_password_look_the_same`.)
4. **A department user on the dashboard, documents tabs or county
   overview.** They must not see counts, rows or chart totals from another
   department's Department-visibility or Restricted documents. They must see
   County-wide ones.
   (Task 7 `dashboard_panels_respect_visibility`, Task 8
   `status_counts_respect_visibility`, Task 10
   `overview_counts_respect_visibility`.)
5. **Clicking "Sign in with Microsoft" before Entra is set up.** It must land
   back on the sign-in page with the fixed "isn't set up yet" message, not on
   a 404 page. An attacker-supplied `notice` value must never be echoed.
   (Task 6 `microsoft_button_when_sso_off_shows_notice`.)

---

### Task 1: Visual system, assets and the CSP guard

**Files:**

**Create:**
- `crates/pdms-web/static/fonts/source-sans-3-latin-400.woff2`,
  `…-600.woff2` and `…-700.woff2` (copied from the staged assets);
- `crates/pdms-web/static/fonts/OFL.txt` (copy of `fontsource-LICENSE`);
- `crates/pdms-web/templates/icons/*.html`: one partial per icon (list
  below);
- `crates/pdms-web/templates/icons/LICENSE-lucide.txt` (copy of the Lucide
  `LICENSE`);
- `THIRD_PARTY_NOTICES.md`;
- `crates/pdms-web/tests/csp_guard.rs`.

**Modify:**
- `crates/pdms-web/static/app.css` (full rewrite);
- `crates/pdms-web/src/routes/ops.rs` (font route);
- `crates/pdms-web/src/lib.rs` (register `/static/fonts/{file}`).

**Interfaces:**
- **Produces:**
  - **CSS classes** (spec §4) that later tasks use:
    - buttons: `.button`, `.button.primary`, `.button.danger`,
      `.button.small`;
    - containers: `.panel`, `.panel-head`, `.tile`, `.tile-critical`,
      `.tile-warning`, `.tile-info`, `.tile-accent`, `.table-wrap`;
    - status: `.status` with `.status-critical`, `.status-warning`,
      `.status-info`, `.status-good` and `.status-neutral`, and
      `.badge.badge-*` with the same suffixes;
    - navigation: `.tabs`, `.breadcrumb`, `.pager`;
    - `.flash` with `.flash-ok` and `.flash-error`;
    - `.disclosure`;
    - charts: `.chart`, `.chart-bar`, `.chart-seg-good`,
      `.chart-seg-info`, `.chart-seg-warning`, `.chart-seg-critical`,
      `.chart-seg-neutral`, `.chart-axis`, `.chart-grid`, `.chart-label`;
    - helpers: `.visually-hidden`, `.muted`, `.small`.
  - **Icon partials,** included as
    `{% include "icons/file-text.html" %}`. Each partial is a 20×20 `<svg
    aria-hidden="true" focusable="false" class="icon">` with
    `stroke="currentColor"`.
  - **Font route:** `GET /static/fonts/{file}`.

- [ ] **Step 1: Write the CSP guard test (it passes today and protects every later task).** Create `crates/pdms-web/tests/csp_guard.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Templates and inline HTML must stay compatible with the strict CSP:
//! no style attributes, no inline scripts, no event-handler attributes.

use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().is_some_and(|e| e == "html") {
            out.push(p);
        }
    }
}

fn offences(text: &str) -> Vec<(usize, String)> {
    let handler = regex::Regex::new(r#"\son[a-z]+\s*="#).unwrap();
    let mut found = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("style=\"") || lower.contains("style='") {
            found.push((i + 1, "style attribute".into()));
        }
        if lower.contains("<script") && !lower.contains("src=") && !lower.contains("type=\"application/json\"") {
            found.push((i + 1, "inline script".into()));
        }
        if handler.is_match(&lower) {
            found.push((i + 1, "event handler attribute".into()));
        }
    }
    found
}

#[test]
fn templates_and_inline_html_are_csp_clean() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    files(&root.join("templates"), &mut paths);
    paths.push(root.join("src/routes/ops.rs"));
    let mut report = Vec::new();
    for p in &paths {
        let text = std::fs::read_to_string(p).unwrap();
        for (line, what) in offences(&text) {
            report.push(format!("{}:{line}: {what}", p.display()));
        }
    }
    assert!(report.is_empty(), "CSP-incompatible markup:\n{}", report.join("\n"));
}

#[test]
fn guard_detects_each_offence() {
    assert_eq!(offences(r#"<p style="color:red">"#).len(), 1);
    assert_eq!(offences("<script>alert(1)</script>").len(), 1);
    assert_eq!(offences(r#"<a href="#" onclick="x()">"#).len(), 1);
    assert!(offences(r#"<script src="/static/htmx.min.js" defer></script>"#).is_empty());
    assert!(offences(r#"<button hx-on="x">"#).is_empty());
}
```

If `regex` is not already a dev-dependency of `pdms-web`, add it under
`[dev-dependencies]` in `crates/pdms-web/Cargo.toml` with the workspace
version. Check `crates/pdms-web/tests/support.rs`, which already uses a regex
for `.csrf()`.

- [ ] **Step 2: Run it.** `cargo test -p pdms-web --test csp_guard`. Expected: PASS (2 tests); no template has inline styles today.

- [ ] **Step 3: Copy the assets.** Use literal paths, and only `cp`/`mkdir`:

```bash
S=$R1_SCRATCH/assets-probe
mkdir -p crates/pdms-web/static/fonts crates/pdms-web/templates/icons
cp $S/ss3-400.woff2 crates/pdms-web/static/fonts/source-sans-3-latin-400.woff2
cp $S/ss3-600.woff2 crates/pdms-web/static/fonts/source-sans-3-latin-600.woff2
cp $S/ss3-700.woff2 crates/pdms-web/static/fonts/source-sans-3-latin-700.woff2
cp $S/fontsource-LICENSE crates/pdms-web/static/fonts/OFL.txt
cp $S/lucide/package/LICENSE crates/pdms-web/templates/icons/LICENSE-lucide.txt
sha256sum crates/pdms-web/static/fonts/*.woff2
```

The sha256 values must begin `78e22177`, `ebc046fe` and `37ea2727`
respectively.

- [ ] **Step 4: Create the icon partials.**
  - Make one file per icon at `crates/pdms-web/templates/icons/<name>.html`
    for these Lucide names:
    - `layout-dashboard`, `file-text`, `calendar`, `users`, `chart-column`,
      `bell`, `archive`, `settings`, `scroll-text`, `log-out`, `key-round`;
    - `search`, `plus`, `check`, `clock`, `circle-alert`, `eye`,
      `square-check`, `menu`, `download`, `pencil`, `history`, `upload`.
  - **Content:** the inner elements of
    `$S/lucide/package/icons/<name>.svg`, wrapped as:
    ```html
    <svg class="icon" aria-hidden="true" focusable="false" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><!-- inner paths copied verbatim --></svg>
    ```
    Keep the SVG on one line. Drop the `<!-- @license … -->` comment; the
    license file sits next to the partials and in `THIRD_PARTY_NOTICES.md`.

- [ ] **Step 5: Serve fonts.** In `crates/pdms-web/src/routes/ops.rs`, next to `CSS`/`HTMX`:

```rust
const FONT_400: &[u8] = include_bytes!("../../static/fonts/source-sans-3-latin-400.woff2");
const FONT_600: &[u8] = include_bytes!("../../static/fonts/source-sans-3-latin-600.woff2");
const FONT_700: &[u8] = include_bytes!("../../static/fonts/source-sans-3-latin-700.woff2");

/// `GET /static/fonts/{file}`: the embedded Source Sans 3 files (SIL OFL 1.1).
pub async fn static_font(Path(name): Path<String>) -> Response {
    let body = match name.as_str() {
        "source-sans-3-latin-400.woff2" => FONT_400,
        "source-sans-3-latin-600.woff2" => FONT_600,
        "source-sans-3-latin-700.woff2" => FONT_700,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response()
}
```

Register `.route("/static/fonts/{file}", get(ops::static_font))` in `lib.rs`
next to `/static/{file}` (around :361). The `security_headers` middleware
leaves `/static/` paths' Cache-Control alone (middleware.rs:73).

- [ ] **Step 6: Write a font route test.** Append to `crates/pdms-web/tests/web.rs`:

```rust
#[tokio::test]
async fn fonts_are_served_with_long_cache() {
    let app = app().await;
    let mut b = Browser::new(&app);
    let r = b.get("/static/fonts/source-sans-3-latin-400.woff2").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "font/woff2");
    assert!(r.headers["cache-control"].to_str().unwrap().contains("immutable"));
    assert_eq!(b.get("/static/fonts/../app.css").await.status, StatusCode::NOT_FOUND);
}
```

`Resp.body` is a `String`; a binary body may be lossy, so the test checks
only the status and headers. If `Browser::get` cannot take a non-UTF-8 body,
assert only status and headers, as above.

- [ ] **Step 7: Rewrite `app.css`.** Replace the file with the direction-D system:

1. **Header comment:** `/* PDMS styles (direction D). Colors meet WCAG 2.1 AA; see docs/superpowers/specs/2026-10-15-ui-redesign-r1-design.md §4. */`
2. **`@font-face`** for the three weights:
   - `font-family: "Source Sans 3"; src: url("/static/fonts/source-sans-3-latin-400.woff2") format("woff2"); font-weight: 400; font-display: swap;`
   - the same for 600 and 700.
3. **`:root` tokens:**
   - every token in spec §4;
   - the status palette as `--crit-dot`, `--crit-fg`, `--crit-tint`, and
     likewise `--warn-*`, `--info-*`, `--good-*`, `--neutral-*`;
   - **default accent family** (`/theme.css` overrides it later):
     `--accent: #B45309; --accent-hover: #93420A; --accent-link: #8A4B06;
     --accent-tint: #F6ECE3;`.
4. **Base:**
   - `body` uses Source Sans 3, falling back to system-ui; 15px/1.5;
     `--ink` text on `--ground`;
   - links use `--accent-link`;
   - `:focus-visible { outline: 3px solid var(--focus); outline-offset: 2px; }`;
   - `.skip-link` and `.visually-hidden` are kept.
5. **Shell:** `.app` (flex wrap), `.sidebar`, `.sidebar .brand`, `.nav`,
   `.nav a`, `.nav a[aria-current="page"]`, `.nav .count`,
   `.nav-group-label`, `.sidebar .me`, `.content`, `.topbar`,
   `.topbar .search`, `.page`, `.page-footer`.
   - **Below 900px:**
     - `.sidebar` becomes a full-width top bar;
     - `.menu-toggle` (a `<details>`) shows the nav when open;
     - the `.menu-toggle > summary` is visible only below 900px;
     - at 900px and up, the nav is always visible and the summary is hidden.
6. **Components:** everything listed under **Produces**.
   - **Buttons:**
     - `.button.primary`: `--accent` background, white text, `:hover` uses
       `--accent-hover`;
     - `.button`: white background with a 1px `--control-border` border;
     - min-height 40px, radius 6px, weight 600.
   - **Tables:** `table`, `thead th` (13px, `--muted`, `--thead`),
     `td`/`th` (row divider `--row`), `td.num` (tabular-nums,
     right-aligned).
   - **Inputs:** `input`, `select`, `textarea`, with `--control-border`.
7. **Legacy classes still used by other templates.**
   - Get the full list with:
     ```bash
     grep -rhoE 'class="[^"{]*"' crates/pdms-web/templates | sed -E 's/class="//;s/"$//' | tr ' ' '\n' | sort -u
     ```
   - Restyle every class in that list that the old `app.css` styled, so the
     other ~45 pages look consistent. This includes `.card`, `.facts`,
     `.filters`, `.toolbar`, `.grid-form`, `.inline`, `.stack`, `.events`,
     `.date`, `.help`, `.count`, `.subnav`, `.description`, `.two-col`,
     `.dash-grid`, `.doc-header`, `.wide`, `.link`, `.block` and `diff`.
   - **Map the badge families to the status palette:**
     - `.badge.status-*` and `.badge.task-*`;
     - `.badge.wf-*` and `.badge.ev-*`;
     - `.badge.confidential`, `.badge.hold`, `.tag`.
   - Mapping: overdue → critical; due-soon/renewal_due → warning;
     in_review/in_progress → info; active/completed/approved → good; draft,
     expired, superseded, archived, cancelled → neutral.
   - **`.card` keeps working.** It looks like `.panel`, with
     `padding: 0 18px` so its h2 margins still work.
8. **Print:** `@media print { .sidebar, .topbar, .menu-toggle, .button,
   .page-footer { display: none !important; } body { background: #fff; } }`.

Keep the file organized with section comments: tokens, base, shell,
components, legacy, charts, print.

- [ ] **Step 8: Create `THIRD_PARTY_NOTICES.md`** at the repository root:
  - **Source Sans 3:** © Adobe, SIL Open Font License 1.1, with the Reserved
    Font Name "Source"; full text in `crates/pdms-web/static/fonts/OFL.txt`;
    obtained from the `@fontsource/source-sans-3` 5.0.12 package
    (unmodified Latin-subset WOFF2 files).
  - **Lucide icons:** ISC license; text in
    `crates/pdms-web/templates/icons/LICENSE-lucide.txt`; version 0.460.0.
  - **htmx:** already bundled. State its license (BSD 2-Clause, zero-clause
    variant per upstream). Check the header of `static/htmx.min.js` and
    write what it says.

- [ ] **Step 9: Verify.**
  - `cargo test -p pdms-web` must pass. CSS changes don't break tests;
    template class assertions, if any, may need the old class names kept.
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo fmt --all --check`

- [ ] **Step 10: Commit.** Stage `crates/pdms-web/static`, `crates/pdms-web/templates/icons`, `crates/pdms-web/src`, `crates/pdms-web/tests/csp_guard.rs`, `crates/pdms-web/tests/web.rs`, `crates/pdms-web/Cargo.toml` (if changed) and `THIRD_PARTY_NOTICES.md`. Message: `ui: direction-D visual system, self-hosted fonts and icons, CSP guard test`.

---

### Task 2: Migration 0009 and the core branding module

**Files:**
- **Create:**
  - `migrations/0009_ui_redesign.sql`;
  - `crates/pdms-core/src/branding.rs`;
  - `crates/pdms-core/src/branding/color.rs`;
  - `crates/pdms-core/src/branding/seal.rs`;
  - `crates/pdms-core/tests/branding.rs`.
- **Modify:**
  - `crates/pdms-core/src/lib.rs` (`pub mod branding;`);
  - the org-name call sites in `crates/pdms-core/src/notifications/delivery.rs:420`, `calendar.rs:436`, `integrations/mod.rs:326`, `integrations/forward.rs:63`, `exports.rs:454` and `exports.rs:531`.

**Interfaces:**
- **Produces:**
  - `pdms_core::branding::{Branding, SealInfo, ThemeColors, DEFAULT_ACCENT}`;
  - `load(db: &PgPool) -> CoreResult<Branding>`;
  - `org_name(core: &Core) -> String`;
  - `update(core: &Core, ctx: &Ctx, org_name: &str, accent: &str) -> CoreResult<()>`;
  - `set_seal(core: &Core, ctx: &Ctx, bytes: &[u8]) -> CoreResult<SealInfo>`;
  - `remove_seal(core: &Core, ctx: &Ctx) -> CoreResult<()>`;
  - `reset(core: &Core, ctx: &Ctx) -> CoreResult<()>`;
  - `seal_bytes(db: &PgPool) -> CoreResult<Option<(String, Vec<u8>)>>`, giving `(content_type, bytes)`;
  - `color::{Rgb, parse_hex, contrast, derive_theme, validate_accent, AccentError}`;
  - `seal::{sniff, SealError}`.
- **The `users.username` column** is created here. Task 5 uses it.

- [ ] **Step 1: Write the migration** `migrations/0009_ui_redesign.sql`:

```sql
-- UI redesign R1: branding assets (the seal) and usernames for local accounts.
-- Name and accent live in app_settings (keys branding.org_name, branding.accent).

CREATE TABLE branding_assets (
    name         text PRIMARY KEY CHECK (name IN ('seal')),
    content_type text NOT NULL CHECK (content_type IN ('image/png', 'image/jpeg')),
    bytes        bytea NOT NULL CHECK (octet_length(bytes) <= 524288),
    sha256       text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    width        integer NOT NULL CHECK (width BETWEEN 16 AND 2048),
    height       integer NOT NULL CHECK (height BETWEEN 16 AND 2048),
    updated_by   uuid REFERENCES users (id),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE users ADD COLUMN username text
    CHECK (username ~ '^[A-Za-z0-9._-]{2,64}$');
CREATE UNIQUE INDEX users_username_key ON users (lower(username))
    WHERE username IS NOT NULL;
```

Check how 0001 grants table privileges to the app role (for example
`GRANT SELECT, INSERT, UPDATE, DELETE ON app_settings TO pdms_app;` near
0001:381). Add the matching grant for `branding_assets` with the same role
name.

Then run `sqlx migrate run --source migrations`.

- [ ] **Step 2: Write the color module with its unit tests.** Create `crates/pdms-core/src/branding/color.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Accent color math: WCAG contrast and the derived theme shades.

/// Page background (`--ground`); links must reach 4.5:1 on it.
pub const GROUND: Rgb = Rgb(0xF3, 0xF5, 0xF8);
pub const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);
pub const MIN_TEXT_CONTRAST: f64 = 4.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AccentError {
    Invalid,
    TooLight { ratio: f64 },
}

impl AccentError {
    pub fn message(&self, input: &str) -> String {
        match self {
            Self::Invalid => "Enter the accent as a hex color such as #1F3A5F.".into(),
            Self::TooLight { ratio } => format!(
                "White text on {input} is hard to read ({ratio:.1}:1; at least 4.5:1 is needed). Choose a darker shade."
            ),
        }
    }
}

/// Parses `#RRGGBB` (case-insensitive, surrounding whitespace ignored).
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim();
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn to_linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

fn luminance(c: Rgb) -> f64 {
    0.2126 * to_linear(c.0) + 0.7152 * to_linear(c.1) + 0.0722 * to_linear(c.2)
}

/// WCAG 2.1 contrast ratio, 1.0 ..= 21.0.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn to_oklab(c: Rgb) -> (f64, f64, f64) {
    let (r, g, b) = (to_linear(c.0), to_linear(c.1), to_linear(c.2));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    (
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    )
}

fn from_oklab(lab: (f64, f64, f64)) -> Rgb {
    let (big_l, a, b) = lab;
    let l = (big_l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (big_l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (big_l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    Rgb(
        from_linear(4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s),
        from_linear(-1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s),
        from_linear(-0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s),
    )
}

/// Scales OKLCH lightness by `factor` (hue and chroma kept).
pub fn darken(c: Rgb, factor: f64) -> Rgb {
    let (l, a, b) = to_oklab(c);
    from_oklab((l * factor, a, b))
}

/// `weight` of `c` over white, mixed in sRGB.
pub fn tint(c: Rgb, weight: f64) -> Rgb {
    let mix = |x: u8| (f64::from(x) * weight + 255.0 * (1.0 - weight)).round() as u8;
    Rgb(mix(c.0), mix(c.1), mix(c.2))
}

/// The accent must carry white button text at 4.5:1.
pub fn validate_accent(input: &str) -> Result<Rgb, AccentError> {
    let c = parse_hex(input).ok_or(AccentError::Invalid)?;
    let ratio = contrast(c, WHITE);
    if ratio < MIN_TEXT_CONTRAST {
        return Err(AccentError::TooLight { ratio });
    }
    Ok(c)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColors {
    pub accent: String,
    pub hover: String,
    pub link: String,
    pub tint: String,
}

/// Hover is 12% darker; link is darkened in 4% steps (at least one) until it
/// reaches 4.5:1 on the page background; tint is 12% accent over white.
pub fn derive_theme(accent: Rgb) -> ThemeColors {
    let mut link = darken(accent, 0.96);
    for _ in 0..40 {
        if contrast(link, GROUND) >= MIN_TEXT_CONTRAST {
            break;
        }
        link = darken(link, 0.96);
    }
    ThemeColors {
        accent: accent.hex(),
        hover: darken(accent, 0.88).hex(),
        link: link.hex(),
        tint: tint(accent, 0.12).hex(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_round_trip() {
        assert_eq!(parse_hex(" #b45309 "), Some(Rgb(0xB4, 0x53, 0x09)));
        assert_eq!(Rgb(0xB4, 0x53, 0x09).hex(), "#B45309");
        for bad in ["B45309", "#B4530", "#B45309FF", "#GGGGGG", "", "red"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        assert!((contrast(Rgb(0, 0, 0), WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Rgb(0xB4, 0x53, 0x09), WHITE) - 5.02).abs() < 0.01);
        assert!((contrast(Rgb(0x15, 0x20, 0x2B), GROUND) - 15.10).abs() < 0.02);
    }

    #[test]
    fn accent_too_light_is_refused() {
        match validate_accent("#F5D76E") {
            Err(AccentError::TooLight { ratio }) => assert!(ratio < 4.5),
            other => panic!("expected TooLight, got {other:?}"),
        }
        assert_eq!(validate_accent("nope"), Err(AccentError::Invalid));
        assert!(validate_accent("#B45309").is_ok());
        assert!(validate_accent("#1F3A5F").is_ok());
        assert!(AccentError::TooLight { ratio: 2.1 }.message("#F5D76E").contains("2.1:1"));
    }

    #[test]
    fn derived_link_is_readable_for_every_valid_accent() {
        for hex in ["#B45309", "#1F3A5F", "#0B6B5F", "#7A1F2B", "#3446C4", "#6D28D9", "#0F766E", "#767676"] {
            let accent = validate_accent(hex).unwrap();
            let t = derive_theme(accent);
            let link = parse_hex(&t.link).unwrap();
            assert!(contrast(link, GROUND) >= 4.5, "{hex} -> {}", t.link);
            assert_ne!(t.link, t.accent, "link must be at least one step darker");
            let hover = parse_hex(&t.hover).unwrap();
            assert!(contrast(hover, WHITE) >= contrast(accent, WHITE));
        }
    }

    #[test]
    fn darken_keeps_hue_roughly() {
        let d = darken(Rgb(0xB4, 0x53, 0x09), 0.88);
        assert!(d.0 > d.1 && d.1 > d.2, "still an orange: {d:?}");
    }
}
```

- [ ] **Step 3: Write the seal sniffer with its unit tests.** Create `crates/pdms-core/src/branding/seal.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Seal upload checks: real type from the first bytes, dimensions from the header.

pub const MAX_BYTES: usize = 512 * 1024;
pub const MIN_SIDE: u32 = 16;
pub const MAX_SIDE: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    NotPngOrJpeg,
    TooLarge,
    BadDimensions,
    Unreadable,
}

impl SealError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotPngOrJpeg => "The seal must be a PNG or JPEG image.",
            Self::TooLarge => "The seal must be 512 KB or smaller.",
            Self::BadDimensions => "The seal must be between 16 and 2048 pixels on each side.",
            Self::Unreadable => "The seal image could not be read. Save it again as PNG or JPEG.",
        }
    }
}

const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(*b.get(i)?) << 8 | u32::from(*b.get(i + 1)?))
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(be16(b, i)? << 16 | be16(b, i + 2)?)
}

fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(12..16)? == b"IHDR").then_some(())?;
    Some((be32(b, 16)?, be32(b, 20)?))
}

fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 3 < b.len() {
        if b[i] != 0xFF {
            return None;
        }
        while b.get(i + 1) == Some(&0xFF) {
            i += 1;
        }
        let marker = *b.get(i + 1)?;
        if marker == 0x01 || (0xD0..=0xD8).contains(&marker) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
        if is_sof {
            return Some((be16(b, i + 7)?, be16(b, i + 5)?));
        }
        if marker == 0xD9 || len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Returns `(content_type, width, height)` or why the file is refused.
pub fn sniff(bytes: &[u8]) -> Result<(&'static str, u32, u32), SealError> {
    if bytes.len() > MAX_BYTES {
        return Err(SealError::TooLarge);
    }
    let (ctype, size) = if bytes.starts_with(&PNG_SIG) {
        ("image/png", png_size(bytes))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("image/jpeg", jpeg_size(bytes))
    } else {
        return Err(SealError::NotPngOrJpeg);
    };
    let (w, h) = size.ok_or(SealError::Unreadable)?;
    if !(MIN_SIDE..=MAX_SIDE).contains(&w) || !(MIN_SIDE..=MAX_SIDE).contains(&h) {
        return Err(SealError::BadDimensions);
    }
    Ok((ctype, w, h))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal PNG header bytes: signature + IHDR chunk with the given size.
    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = PNG_SIG.to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    /// Minimal JPEG: SOI, an APP0 segment, then SOF0 with the given size.
    pub(crate) fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn accepts_png_and_jpeg_with_sizes() {
        assert_eq!(sniff(&png(200, 180)), Ok(("image/png", 200, 180)));
        assert_eq!(sniff(&jpeg(300, 120)), Ok(("image/jpeg", 300, 120)));
    }

    #[test]
    fn sniff_refuses_svg_and_renamed_text() {
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"), Err(SealError::NotPngOrJpeg));
        assert_eq!(sniff(b"<html><script>alert(1)</script>"), Err(SealError::NotPngOrJpeg));
        assert_eq!(sniff(b"GIF89a\x10\x00\x10\x00"), Err(SealError::NotPngOrJpeg));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Err(SealError::NotPngOrJpeg));
    }

    #[test]
    fn refuses_bad_dimensions_size_and_truncated_headers() {
        assert_eq!(sniff(&png(8, 200)), Err(SealError::BadDimensions));
        assert_eq!(sniff(&png(4096, 200)), Err(SealError::BadDimensions));
        assert_eq!(sniff(&png(200, 200)[..14]), Err(SealError::Unreadable));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xD9]), Err(SealError::Unreadable));
        let mut big = png(100, 100);
        big.resize(MAX_BYTES + 1, 0);
        assert_eq!(sniff(&big), Err(SealError::TooLarge));
    }
}
```

- [ ] **Step 4: Write the branding service.** Create `crates/pdms-core/src/branding.rs`, declaring `pub mod color; pub mod seal;`. Requirements:
  - **Types:**
    - `pub const DEFAULT_ACCENT: &str = "#B45309";`
    - `pub struct Branding { pub org_name: Option<String>, pub accent: Option<String>, pub seal: Option<SealInfo> }`, plus `pub fn accent_or_default(&self) -> &str` and `pub fn theme(&self) -> color::ThemeColors`. `theme` parses the stored accent, falling back to `DEFAULT_ACCENT` when it is missing or invalid.
    - `pub struct SealInfo { pub sha256: String, pub content_type: String, pub width: i32, pub height: i32, pub updated_at: DateTime<Utc> }`.
  - **`load(db)`:**
    - reads `app_settings` keys `branding.org_name` and `branding.accent` (JSON strings) and the `branding_assets` row `seal` (without its bytes);
    - a missing key gives `None`; a non-string JSON value is treated as missing.
  - **`org_name(core)`:**
    - returns the `branding.org_name` value if set and non-empty, else `core.config.org.name.clone()`;
    - on a DB error, logs at `warn` and falls back to config;
    - never fails.
  - **`update(core, ctx, org_name, accent)`:**
    - `ctx.require(Capability::ManageSettings)?`;
    - trims both;
    - name: when non-empty, 1–120 characters, else `CoreError::validation("The county name must be 120 characters or fewer.")`;
    - accent: when non-empty, `color::validate_accent(accent)`, mapping the error with `CoreError::validation(e.message(accent.trim()))`. Store the normalized form `Rgb::hex()` (uppercase);
    - in one transaction: `SELECT … FOR UPDATE` the two keys, then upsert non-empty values (`INSERT … ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = $3, updated_at = now()`) and `DELETE` keys whose new value is empty;
    - audit `branding.updated` (`Category::Admin`, object type `settings`) with `.change("org_name", before, after)` and `.change("accent", before, after)`. Write it only when something changed; a no-op save writes no audit entry.
  - **`set_seal(core, ctx, bytes)`:**
    - requires `ManageSettings`;
    - `seal::sniff(bytes)`, mapping errors to `CoreError::validation(e.message())`;
    - sha256 as lowercase hex (`sha2` is already a core dependency; check `Cargo.toml`);
    - upsert the `seal` row;
    - audit `branding.seal_set` with `.context("sha256", …)`, `.context("bytes", len)`, `.context("width", w)` and `.context("height", h)`.
  - **`remove_seal`:** requires `ManageSettings`; deletes the row; audits `branding.seal_removed` **only if a row existed**.
  - **`reset`:** requires `ManageSettings`; deletes both keys and the seal row in one transaction; audits `branding.reset` with the before-values as changes (name, accent, seal sha256 → empty).
  - **`seal_bytes(db)`:** returns `SELECT content_type, bytes FROM branding_assets WHERE name = 'seal'`.

- [ ] **Step 5: Switch the core org-name readers.** Replace `core.config.org.name` with `branding::org_name(core).await` in:
  - `notifications/delivery.rs:420`;
  - `calendar.rs:436` (the ICS calendar name);
  - `integrations/mod.rs:326`;
  - `integrations/forward.rs:63`;
  - `exports.rs:454` and `exports.rs:531`.

  Each of these is already in an async function with `core` in scope. If one
  is not, thread the name in from its async caller. Leave
  `crates/pdms/src/main.rs` (log line, TOTP issuer) on the config value.

- [ ] **Step 6: Write the integration tests.** Create `crates/pdms-core/tests/branding.rs`, using `mod common; use common::*;`. Tests:
  - **`only_settings_managers_can_change_branding`:** a plain user gets `Forbidden` from `update`, `set_seal`, `remove_seal` and `reset`.
  - **`update_validates_normalizes_and_audits`:**
    - the system ctx sets `"[County Name]"` and `"#1f3a5f"`;
    - `load` returns `"#1F3A5F"`;
    - `audit_actions` ends with `"branding.updated"`;
    - saving the same values again adds no new audit entry;
    - `org_name(&core)` returns `"[County Name]"`;
    - after `update(…, "", "")`, `org_name` returns the config name.
  - **`accent_too_light_is_refused_in_service`:** `update(…, "", "#F5D76E")` gives `Validation`, with a message containing "4.5:1".
  - **`seal_round_trip`:**
    - `set_seal` with a test PNG (128×128) and `seal_bytes` returns `image/png` and the same bytes; `load().seal` has width 128;
    - `remove_seal` empties it and audits `branding.seal_removed`;
    - a second `remove_seal` adds no entry.
    - Build the PNG bytes inline with the same layout as `seal::tests::png`. The integration test cannot reach the `pub(crate)` helper, so copy the 10-line builder into the test file.
  - **`reset_clears_everything`.**

- [ ] **Step 7: Prepare the query data, then verify.**
  - `cargo sqlx prepare --workspace -- --all-targets`
  - `cargo test -p pdms-core branding` and `cargo test -p pdms-core --lib branding`
  - clippy and fmt.

- [ ] **Step 8: Commit.** Stage `migrations/0009_ui_redesign.sql`, `crates/pdms-core`, `.sqlx`. Message: `branding: settings, seal, accent contrast rules, org-name resolver; migration 0009`.

---

### Task 3: The app shell, theme delivery and branding cache in the web layer

**Files:**
- **Modify:**
  - `crates/pdms-web/src/state.rs` (branding cache);
  - `crates/pdms-web/src/lib.rs` (routes and refresh middleware);
  - `crates/pdms-web/src/middleware.rs` (the `branding_refresh` fn);
  - `crates/pdms-web/src/views.rs` (Layout fields);
  - `crates/pdms-web/src/session.rs` (department label and `can_create`);
  - `crates/pdms-web/src/error.rs` (org name from the cache);
  - `crates/pdms-web/src/routes/ops.rs` (`theme_css`, `seal`, and the `/source` page using the cache);
  - `crates/pdms-web/templates/base.html` (rewrite).
- **Test:** `crates/pdms-web/tests/shell.rs` (new).

**Interfaces:**
- **Consumes:** `pdms_core::branding::{load, Branding, SealInfo}`, `Branding::theme()`, `seal_bytes`.
- **Produces:**
  - `AppState::branding(&self) -> BrandingSnapshot`, a clone of the cached snapshot, and `AppState::reload_branding(&self) -> impl Future<Output = ()>`;
  - `BrandingSnapshot { org_name: String, theme: ThemeColors, version: String, seal_version: Option<String>, loaded_at: Instant }`;
  - `AppState::org_name(&self) -> String`. It now returns an owned `String`; update the callers;
  - Layout's new fields:
    - `org_name` from the snapshot;
    - `theme_version: String`, `seal_version: Option<String>`;
    - `user_department: String`;
    - `can_create: bool`;
    - `admin_tabs: bool` (true when `section == "admin"`).

- [ ] **Step 1: Write the failing tests.** Create `crates/pdms-web/tests/shell.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

mod support;
use axum::http::StatusCode;
use support::*;

#[tokio::test]
async fn theme_css_is_public_and_carries_the_accent() {
    let app = app().await;
    let mut b = Browser::new(&app);
    let r = b.get("/theme.css").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.headers["content-type"].to_str().unwrap().starts_with("text/css"));
    assert!(r.body.contains("--accent: #B45309;"), "{}", r.body);
    assert!(r.body.contains("--accent-link:"));
    assert!(r.headers["cache-control"].to_str().unwrap().contains("immutable"));
}

#[tokio::test]
async fn seal_is_404_until_set() {
    let app = app().await;
    let mut b = Browser::new(&app);
    assert_eq!(b.get("/branding/seal").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn shell_has_sidebar_menu_and_permissions() {
    let app = app().await;
    let (_, secret) = user(&app.core, "ada@example.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("ada@example.org", &secret).await;
    let home = b.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    for item in ["Dashboard", "Documents", "Calendar", "Appointments", "Reports", "Notifications"] {
        assert!(home.body.contains(item), "missing {item}");
    }
    assert!(home.body.contains("href=\"/theme.css?v="));
    assert!(home.body.contains("class=\"menu-toggle\""));
    assert!(!home.body.contains(">Administration<"), "plain user must not see Administration");
    assert!(!home.body.contains(">Audit log<"));
    assert!(home.body.contains("name=\"q\""), "top bar search");
}
```

Adjust the `user()` and `login()` calls to the real helper signatures in
`support.rs`. If its role argument takes a role key, `"admin"` makes an
org-wide admin; add a fourth test,
`admin_sees_manage_group_and_admin_tabs`. In it, an admin user sees
"Manage", "Administration" and "Audit log", and `/admin/users` shows the
admin tab row with `class="tabs"`.

Run: `cargo test -p pdms-web --test shell`. Expected: FAIL (`/theme.css` is
404).

- [ ] **Step 2: Add the branding cache to the state.** In `state.rs`:
  - Add `branding: Arc<std::sync::RwLock<BrandingSnapshot>>`.
  - **`BrandingSnapshot::from(core_config_name: &str, b: &pdms_core::branding::Branding) -> Self`:**
    - `org_name`: `b.org_name` or the config name;
    - `theme`: `b.theme()`;
    - `version`: the first 12 hex characters of sha256 over `org_name + "\n" + theme.accent + "\n" + seal sha or ""`;
    - `seal_version`: the first 12 characters of the seal sha256.
  - **`AppState::new`** builds the initial snapshot from config alone (no DB access in `new`). The router-construction path then calls `reload_branding().await` where an async context exists. If `AppState::new` is sync, do the first load lazily in the middleware: initialize `loaded_at` far enough in the past that the first request reloads.
  - **`reload_branding()`:** `pdms_core::branding::load(&self.core.db).await`. On success, swap the snapshot and update `error::set_org_name(…)`. On failure, `tracing::warn!` and keep the old snapshot.
  - **`branding()`:** read-locks and clones. Use `unwrap_or_else(|p| p.into_inner())` for poisoned locks, since `unwrap` is denied.

- [ ] **Step 3: Write the refresh middleware.** In `middleware.rs`, add `pub async fn branding_refresh(State(state): State<AppState>, req: Request, next: Next) -> Response`. When `state.branding().loaded_at.elapsed() >= Duration::from_secs(60)`, it calls `state.reload_branding().await`, then always runs `next`. Add it in `lib.rs` as the innermost global layer, so it runs after security headers and before handlers.

- [ ] **Step 4: Update the error page's org name.**
  - `error.rs` uses a `OnceLock<String>`. Change it to a `std::sync::RwLock<String>` static initialized to the config default.
  - `set_org_name(name: &str)` overwrites it, and readers clone it.
  - Keep the public function names, so `lib.rs:52` still compiles.

- [ ] **Step 5: Add the theme and seal routes.** In `routes/ops.rs`:

```rust
/// `GET /theme.css`: the accent family from Administration -> Branding.
pub async fn theme_css(State(state): State<AppState>) -> Response {
    let t = state.branding().theme;
    let css = format!(
        ":root {{ --accent: {}; --accent-hover: {}; --accent-link: {}; --accent-tint: {}; }}\n",
        t.accent, t.hover, t.link, t.tint
    );
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        css,
    )
        .into_response()
}

/// `GET /branding/seal`: the uploaded seal, or 404 when none is set.
pub async fn seal(State(state): State<AppState>) -> AppResult<Response> {
    let Some((ctype, bytes)) = pdms_core::branding::seal_bytes(&state.core.db).await? else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    // The table's CHECK constraint allows only these two types.
    let (ctype, ext) = if ctype == "image/png" { ("image/png", "png") } else { ("image/jpeg", "jpg") };
    Ok((
        [
            (header::CONTENT_TYPE, ctype.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"seal.{ext}\"")),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
        ],
        bytes,
    )
        .into_response())
}
```

Register `/theme.css` and `/branding/seal` with `get(...)` in `lib.rs`, near
`/tag-colors.css`. They are public because the handlers take no
`CurrentUser`. Make sure `security_headers` does not overwrite these
Cache-Control values. It only sets `no-store` when none is present
(middleware.rs:73); confirm this by reading it.

- [ ] **Step 6: Add the department label and `can_create` to the current user.**
  - In `session.rs`, `CurrentUser` loads the principal already. Add `department_label: String` and `can_create: bool`, filled in the extractor:
    - **`department_label`:** the primary department name, or the first membership; "N departments" when there are more than one; "" when there are none. Use one query: `SELECT d.name, m.is_primary FROM department_memberships m JOIN departments d … WHERE m.user_id = $1`. Read the real table names from `migrations/0001_foundation.sql`.
    - **`can_create`:** `!pdms_core::documents::access::creatable_departments(&state.core, &ctx).await?.is_empty()`. That is the same check the documents list uses at `routes/documents.rs:386`.
  - Copy both into `Layout::new` as `user_department` and `can_create`.

- [ ] **Step 7: Rewrite `base.html`.** Build the shell from spec §5, using `D-Dashboard.dc.html` for the visual reference. The skeleton (fill in the elided parts):

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="htmx-config" content='{"includeIndicatorStyles": false, "allowEval": false, "selfRequestsOnly": true}'>
  <title>{{ l.title }} – {{ l.org_name }}</title>
  <link rel="stylesheet" href="/static/app.css">
  <link rel="stylesheet" href="/theme.css?v={{ l.theme_version }}">
  <link rel="stylesheet" href="/tag-colors.css">
  <script src="/static/htmx.min.js" defer></script>
</head>
<body hx-headers='{"X-CSRF-Token": "{{ l.csrf }}"}'>
<a class="skip-link" href="#main">Skip to main content</a>
<div class="app">
  <aside class="sidebar">
    <a class="brand" href="/">
      {% if let Some(v) = l.seal_version %}<img src="/branding/seal?v={{ v }}" alt="" width="38" height="38">{% else %}<span class="mark" aria-hidden="true">PDMS</span>{% endif %}
      <span class="brand-text"><span class="product">PDMS</span><span class="org">{{ l.org_name }}</span></span>
    </a>
    <details class="menu-toggle">
      <summary>{% include "icons/menu.html" %} Menu</summary>
      <nav aria-label="Main">
        <ul class="nav">
          <li><a href="/" {% if l.section == "home" %}aria-current="page"{% endif %}>{% include "icons/layout-dashboard.html" %}Dashboard</a></li>
          <!-- Documents (file-text), Calendar (calendar), Appointments (users), Reports (chart-column),
               Notifications (bell) with {% if l.unread > 0 %}<span class="count">{{ l.unread }}<span class="visually-hidden"> unread</span></span>{% endif %} -->
        </ul>
        {% if l.can_exports || l.can_admin || l.can_audit %}
        <p class="nav-group-label">Manage</p>
        <ul class="nav">
          <!-- Records requests (archive, can_exports), Administration (settings, can_admin, href l.admin_home()),
               Audit log (scroll-text, can_audit), each with aria-current by section -->
        </ul>
        {% endif %}
      </nav>
      <div class="me">
        <span class="initials" aria-hidden="true">{{ l.initials() }}</span>
        <span class="who"><span class="name">{{ l.user_name }}</span>{% if !l.user_department.is_empty() %}<span class="dept">{{ l.user_department }}</span>{% endif %}</span>
        <a href="/account/tokens" {% if l.section == "account" %}aria-current="page"{% endif %}>{% include "icons/key-round.html" %}API tokens</a>
        <form method="post" action="/logout"><input type="hidden" name="_csrf" value="{{ l.csrf }}"><button type="submit" class="signout">{% include "icons/log-out.html" %}Sign out</button></form>
      </div>
    </details>
  </aside>
  <div class="content">
    <header class="topbar">
      <form class="search" method="get" action="/documents" role="search">
        <label for="topbar-q" class="visually-hidden">Search documents</label>
        {% include "icons/search.html" %}
        <input id="topbar-q" name="q" type="search" placeholder="Search titles, IDs, tags, and text inside files">
      </form>
      {% if l.can_create %}<a class="button primary" href="/documents/new">{% include "icons/plus.html" %}New document</a>{% endif %}
    </header>
    {% if l.admin_tabs %}
    <nav class="tabs admin-tabs" aria-label="Administration">
      <!-- the exact admin links and permission checks from the old subnav (base.html:38-58), unchanged, plus
           {% if l.can_settings %}<li><a href="/admin/branding">Branding</a></li>{% endif %} -->
    </nav>
    {% endif %}
    <main id="main" class="page" tabindex="-1">
      {% if let Some(f) = l.flash %}<div class="flash {% if f.ok %}flash-ok{% else %}flash-error{% endif %}" role="{% if f.ok %}status{% else %}alert{% endif %}">{{ f.message }}</div>{% endif %}
      {% block heading %}<h1>{{ l.title }}</h1>{% endblock %}
      {% block content %}{% endblock %}
    </main>
    <footer class="page-footer"><a href="/source">Source code</a> · <a href="/license">License: AGPL-3.0-or-later</a></footer>
  </div>
</div>
</body>
</html>
```

**Notes:**
- **Ruling: the nav is rendered twice from one partial.**
  - Chromium hides closed `<details>` content in a way CSS cannot reliably
    override. So move the two `<ul class="nav">` lists, the "Manage" label
    and the `.me` block into `templates/partials/nav.html` and include it
    twice:
    1. once directly in the sidebar, as `<div class="sidebar-nav">…</div>`,
       hidden below 900px with `display: none`;
    2. once inside `<details class="menu-toggle">` (rendered **without**
       `open`), whose whole `<details>` is hidden at 900px and up.
  - `display: none` removes the hidden copy from the accessibility tree, so
    screen readers never hear the menu twice.
  - Give the copies different `aria-label`s on their `<nav>`: "Main" and
    "Main (menu)". The partial takes the label from a variable set with
    `{% let nav_label = "Main" %}` before each include.
  - The skeleton above shows the `<details>` copy only. Add the
    `.sidebar-nav` copy directly above it.
- **The Branding admin tab** links to a page Task 4 builds. Until then it
  would 404, so add the tab in Task 4, not here.
- **`l.initials()`:** add `pub fn initials(&self) -> String` to `Layout`.
  It takes the first letters of the first two whitespace-separated words of
  `user_name`, uppercase; a single word gives its first two letters.
- **Pages using `{% block heading %}`:** pages that render their own header
  (Tasks 7–10) override `{% block heading %}{% endblock %}` to emit their
  own h1.

- [ ] **Step 8: Update the source page and other org-name callers.**
  - The `/source` inline HTML in `ops.rs` uses `state.org_name()`; it now
    returns `String`.
  - `api.rs:1273` and `routes/auth.rs:113` use `state.org_name()` too; they
    keep compiling via the owned `String`.
  - Fix every call site the compiler flags.

- [ ] **Step 9: Run the tests.**
  - `cargo test -p pdms-web --test shell`: expected PASS.
  - `cargo test -p pdms-web`: all pass.
    - Update any existing assertion that depended on the old header markup,
      for example a test checking `class="site-header"`.
    - **Do not** weaken permission assertions.

- [ ] **Step 10: Commit.** Message: `ui: sidebar shell, /theme.css and /branding/seal, branding cache with 60 s refresh`.

---

### Task 4: Administration → Branding page

**Files:**
- **Create:**
  - `crates/pdms-web/templates/admin/branding.html`;
  - `crates/pdms-web/src/routes/branding.rs`;
  - `crates/pdms-web/tests/branding.rs`.
- **Modify:**
  - `crates/pdms-web/src/routes/mod.rs`;
  - `crates/pdms-web/src/lib.rs` (routes in `doc_admin_routes`, or a new merged router; upload body limit);
  - `crates/pdms-web/src/middleware.rs` (`is_upload_route` gains `/admin/branding/seal`);
  - `crates/pdms-web/templates/base.html` (Branding tab).

**Interfaces:**
- **Consumes:**
  - `pdms_core::branding::{update, set_seal, remove_seal, reset, load}` and `AppState::reload_branding`;
  - `upload::read(&state, deferred, multipart) -> UploadForm`, with the CSRF token as the first field and the file field named `file`.
- **Produces these routes:**
  - `GET /admin/branding`;
  - `POST /admin/branding` (form fields `org_name`, `accent`);
  - `POST /admin/branding/seal` (multipart);
  - `POST /admin/branding/seal/remove`;
  - `GET /admin/branding/reset` (confirm page) and `POST /admin/branding/reset`.

- [ ] **Step 1: Write the failing tests.** Create `crates/pdms-web/tests/branding.rs`, using the `support.rs` helpers. Tests:
  - **`branding_page_requires_settings_capability`:**
    - a plain user gets 403 or the access-denied page on `GET /admin/branding`;
    - an admin (role `"admin"`) gets 200 and sees "Branding", "County name", "Accent color" and "Seal".
  - **`branding_page_refuses_light_accent`:**
    - an admin POSTs `org_name=[County Name]&accent=#F5D76E&_csrf=…`;
    - the response redirects back, and the follow-up page shows the flash containing "4.5:1";
    - `/theme.css` still has `#B45309`.
  - **`saving_branding_changes_name_theme_and_audit`:**
    - POST `accent=#1F3A5F` and `org_name=[County Name]`;
    - the home page title contains `– [County Name]`;
    - `/theme.css` contains `--accent: #1F3A5F;`;
    - the `?v=` value in `base.html` changed;
    - `actions(&app.core)` contains `branding.updated`.
  - **`seal_upload_round_trip_and_headers`:**
    - upload a valid minimal PNG (build it inline as in Task 2) via `b.multipart("/admin/branding/seal", &[("_csrf", &csrf)], Some(("seal.png", &png)))`;
    - `GET /branding/seal` gives 200 with `image/png`, `nosniff`, and `inline; filename="seal.png"`;
    - the home page contains `/branding/seal?v=`.
    - Then upload `b"<svg …>"` named `seal.png`. The flash shows "must be a PNG or JPEG" and the stored seal is unchanged.
  - **`reset_restores_defaults`:** after reset, `/theme.css` has `#B45309`, the seal returns 404, and the title uses `Test Org PDMS`.

- [ ] **Step 2: Write the handlers** (`routes/branding.rs`).
  - Every handler starts with `user.require(&state, Capability::ManageSettings, "/admin/branding").await?`.
  - **Page struct:** `BrandingPage { l: Layout, org_name: String, config_name: String, accent: String, seal_version: Option<String>, seal_info: Option<(i32, i32)> }`.
  - **POST save:** calls `branding::update`. On `Ok`, `state.reload_branding().await` then `redirect_with(... "Branding saved.")`. On `Validation(msg)`, `redirect_with(..., ok = false, msg)`. Other errors propagate. Mirror `views::done`, but reload before redirecting.
  - **POST seal:** `upload::read`. Error if there is no file ("Choose an image to upload."). Read the spooled file into memory, capped at 512 KiB + 1: read at most `MAX_BYTES + 1` bytes and let `sniff` return `TooLarge`. Then `branding::set_seal`, reload, and redirect with "Seal uploaded.".
  - **POST remove:** `remove_seal`, reload, then "Seal removed.".
  - **GET reset:** a confirm page. Reuse `admin/branding.html` with a `confirm_reset: bool` flag, or a tiny separate template. It explains what resets and has a POST form with a **Reset to defaults** danger button and a Cancel link.
  - **POST reset:** `reset`, reload, then "Branding reset to defaults.".

- [ ] **Step 3: Register the routes.**
  - In `lib.rs`, merge a `branding_routes` router into the `/admin` nest. The seal POST gets the same `upload_limit` layer used for document uploads, since the seal check itself caps at 512 KiB.
  - Add `"/admin/branding/seal"` to `is_upload_route` in `middleware.rs:114-121`, so `DeferredCsrf` is inserted for that multipart route.

- [ ] **Step 4: Write the template** `admin/branding.html` (extends base). Contents:
  - an intro line: "These settings change how PDMS looks for everyone. The seal and county name also appear on the sign-in page, which anyone who can reach PDMS can see.";
  - **form 1**, fields:
    - "County name": text, maxlength 120, placeholder showing `config_name`, help "Leave blank to use {config_name}.";
    - "Accent color": `<input type="color" id="accent-picker">` **and** `<input type="text" name="accent" pattern="#[0-9A-Fa-f]{6}" value="{{ accent }}">`. Without JS the picker cannot sync to the text box, so make the color input itself `name="accent"`. Keep a text alternative via a separate form field `accent_text` that the handler prefers when non-empty. Ruling: one `<input type="color" name="accent">` plus `<input type="text" name="accent_text">` labeled "or type a hex code"; the handler uses `accent_text` if non-empty, else `accent`. Update the test POSTs to send `accent_text`;
    - help text: "White text must stay readable on it (4.5:1).";
    - a **Save** button;
  - **form 2:** seal upload (`enctype="multipart/form-data"`; `_csrf` as the **first** input, then `<input type="file" name="file" accept="image/png,image/jpeg">`). It shows the current seal (`<img src="/branding/seal?v=…" alt="Current seal" width="96" height="96">`) or "No seal uploaded. PDMS shows a plain mark.";
  - **form 3:** "Remove seal", shown only when a seal is set;
  - a **Preview** panel: a static sidebar strip (`.sidebar`-styled mini block with brand and one active item) plus one `.button.primary`, both using the live CSS variables. No JS;
  - a link to the reset confirm page.

- [ ] **Step 5: Add the Branding tab.** In `base.html`'s admin tabs, add `{% if l.can_settings %}<a href="/admin/branding" …>Branding</a>{% endif %}`.

- [ ] **Step 6: Verify.** `cargo test -p pdms-web --test branding`, then the full `cargo test -p pdms-web`, clippy and fmt.

- [ ] **Step 7: Commit.** Message: `admin: Branding page (county name, accent with contrast check, seal upload)`.

---

### Task 5: Usernames for local accounts

**Files:**
- **Modify:**
  - `crates/pdms-core/src/auth/local.rs` (lookup by username or email);
  - `crates/pdms-core/src/directory/admin.rs` (`set_username`);
  - `crates/pdms-core/src/directory/mod.rs` (`UserSummary.username`);
  - `crates/pdms-web/src/routes/auth.rs` (form field `username`, messages);
  - `crates/pdms-web/src/routes/admin.rs` and `templates/admin/user.html` (username form);
  - `crates/pdms-web/src/lib.rs` (route);
  - `crates/pdms/src/main.rs` (`create-admin --username`, `set-username`);
  - `install.sh`;
  - `crates/pdms-web/tests/support.rs` (`login` posts `username`).
- **Test:**
  - `crates/pdms-core/tests/usernames.rs` (new);
  - `crates/pdms-web/tests/web.rs` (append).

**Interfaces:**
- **Produces:**
  - `pdms_core::directory::set_username(db: &PgPool, ctx: &Ctx, user_id: Uuid, username: Option<&str>) -> CoreResult<()>`;
  - `UserSummary.username: Option<String>`;
  - `pdms_core::auth::local::login(core, identifier: &str, password, code, client)`. The parameter is renamed; the signature is otherwise unchanged;
  - the route `POST /admin/users/{id}/username` (field `username`).

- [ ] **Step 1: Write the failing core tests.** Create `crates/pdms-core/tests/usernames.rs`:
  - **`set_username_validates_and_is_unique_ignoring_case`:**
    - create two local users (look at how `common.rs` or `create_admin` sets local credentials: `set_local_credentials`);
    - `set_username(u1, Some("sysadmin"))` succeeds;
    - `set_username(u2, Some("SysAdmin"))` gives `Conflict` or `Validation` with "That username is already in use.";
    - `"has@sign"`, `"a"` and a 65-character name give `Validation`;
    - `None` clears it;
    - `user.username_set` is audited each time it changes, and a no-op does not audit.
  - **`username_requires_local_credentials`:** an account without local credentials gives `Validation("Only accounts with local sign-in can have a username.")`.
  - **`login_by_username_any_case_and_by_email`:**
    - with `require_totp = false` in the test config (set it on the `Core` config before building, or use the existing helper);
    - `login(core, "SYSADMIN", PW, "", &client)` and `login(core, "admin@example.org", PW, "", &client)` both succeed.
  - **`unknown_username_and_wrong_password_both_fail_generically`:** both return `LoginError::Failed(_)`; the reasons are `UnknownUser` and `BadPassword`.

- [ ] **Step 2: Change the lookup.** In `auth/local.rs:101-107`, replace the WHERE clause:

```sql
 WHERE CASE WHEN strpos($1, '@') > 0
            THEN lower(u.email) = lower($1)
            ELSE lower(u.username) = lower($1)
       END
```

Rename the `email` parameter to `identifier`, and keep the
`attempted_identifier` audit context. The dummy argon2 verify already runs
when no row matches (:114-119); leave it.

- [ ] **Step 3: Add `set_username`.** In `directory/admin.rs`, model it on `update_user` (:193-227):
  - `ctx.require(Capability::ManageDirectory)?`;
  - trim; an empty string means `None`;
  - validate against `^[A-Za-z0-9._-]{2,64}$` (no `@` possible) with the message "Usernames are 2–64 letters, numbers, dots, dashes, or underscores.";
  - `SELECT u.username, (c.user_id IS NOT NULL) AS has_local FROM users u LEFT JOIN local_credentials c … WHERE u.id = $1 FOR UPDATE OF u`;
  - an unknown id gives `NotFound`; no local credentials gives the Validation message above;
  - `UPDATE users SET username = $2, updated_at = now() WHERE id = $1`;
  - map a unique violation on `users_username_key` to `CoreError::Conflict("That username is already in use.".into())`. Check how other code maps unique violations, for example department code conflicts, and follow it;
  - audit `user.username_set` (`Category::Admin`, object type `user`) with `.change("username", before, after)`, only when the value changed.
  - Add `username` to `UserSummary` and its queries.

- [ ] **Step 4: Update the web login handler.**
  - In `routes/auth.rs`, rename `LocalLoginForm.email` to `username`. Pass `&f.username` to `local::login`.
  - **Failure message (ruling):**
    - when `config.auth.local.require_totp` is true: "Username, password, or authenticator code is incorrect.";
    - otherwise: "Username or password is incorrect.".
    - It is still one message for every failure reason, except `LoginFailure::Disabled`, which keeps its own text.
  - Update `support.rs`'s `Browser::login` to post `username` instead of `email`. Keep the `login(email, secret)` signature; an email is a valid identifier.

- [ ] **Step 5: Add the admin user page form.**
  - On `templates/admin/user.html`, when the user has local login (`u.user.has_local_login`), add a small form posting to `/admin/users/{id}/username`. It has a "Username" text field (pattern `[A-Za-z0-9._-]{2,64}`) with help "Used with the password on the sign-in page. Leave blank to sign in with the email address only." and a Save button.
  - **Handler:** `set_username`, then `done(&state, &format!("/admin/users/{id}"), r, "Username saved.")`. `Conflict` and `Validation` show as an error flash; confirm `done` already maps them, and if not, map them.
  - Register the route in `admin_routes`, which is already behind `require_admin`; the core requires `ManageDirectory`.

- [ ] **Step 6: Update the CLI.** In `crates/pdms/src/main.rs`:
  - `CreateAdmin` gains `#[arg(long)] username: Option<String>`. After `set_local_credentials` succeeds and the user id is known, call `directory::set_username(&core.db, &ctx, id, Some(u))` when `username` is set.
  - A new `SetUsername { #[arg(long)] email: String, #[arg(long)] username: String }` subcommand:
    - finds the user by email (case-insensitive), as `create_admin` does via `list_users`;
    - calls `set_username` with `Ctx::system("cli:set-username")`;
    - prints `Username for {email} set to {username}.`;
    - exits non-zero with the error message on failure.

- [ ] **Step 7: Update `install.sh`.**
  - In section 9 (:632-648), after `ask ADMIN_EMAIL …`, add `ask ADMIN_USERNAME "Administrator username" "sysadmin"`. The `ask` helper already honors `PDMS_INSTALL_ADMIN_USERNAME`.
  - Add `PDMS_INSTALL_ADMIN_USERNAME` to the unattended-variables help text at :54.
  - At the `create-admin` invocation (:665-677), add `--username "$ADMIN_USERNAME"`.
  - In the final summary (:701-706), print `Username: $ADMIN_USERNAME` before the password line.
  - Initialize `ADMIN_USERNAME=""` next to `ADMIN_EMAIL=""` (:633).
  - Run `bash -n install.sh` to syntax-check. `bash -n` on a path literal is fine; the harness quirk only blocks `bash <script>` executions. If it is blocked, check the syntax in the main checkout during the final review instead.

- [ ] **Step 8: Write the web tests** (append to `crates/pdms-web/tests/web.rs`):
  - **`login_page_says_username`:** `GET /login` contains `name="username"` and `>Username<`.
  - **`unknown_username_and_wrong_password_look_the_same`:**
    - POST `/login/local` with an unknown username, then with a known user and a wrong password;
    - both give 401 and the same message text;
    - neither response body contains the submitted username.
  - **`admin_sets_username_and_user_signs_in_with_it`:**
    - an admin POSTs `username=SysAdmin` for a local user;
    - that user signs in with `sysadmin` plus PW plus the TOTP code;
    - `actions` contains `user.username_set`.

- [ ] **Step 9: Verify.**
  - `cargo sqlx prepare --workspace -- --all-targets`
  - `cargo test -p pdms-core --test usernames`
  - `cargo test -p pdms-web`
  - `cargo test -p pdms` (if it has tests)
  - clippy and fmt.

- [ ] **Step 10: Commit.** Message: `auth: usernames for local accounts (sign-in, admin page, CLI, installer)`.

---

### Task 6: Sign-in page and the always-visible Microsoft button

**Files:**
- **Modify:**
  - `crates/pdms-web/templates/login.html` (rewrite);
  - `crates/pdms-web/src/routes/auth.rs` (`oidc_start` when off, the `notice` query, page fields);
  - `crates/pdms-web/templates/error.html` (restyle to match; standalone).
- **Test:** `crates/pdms-web/tests/web.rs` (append) and `crates/pdms-web/tests/oidc.rs` (check existing OIDC tests still pass).

**Interfaces:**
- **Consumes:** `AppState::branding()` (org name, theme version, seal version).
- **Produces:**
  - `/login?notice=sso_off`;
  - `LoginPage { org_name: String, theme_version: String, seal_version: Option<String>, csrf, next, next_encoded, oidc_on: bool, local_enabled: bool, totp_required: bool, error: Option<&str>, notice: Option<&'static str> }`.

- [ ] **Step 1: Write the failing tests** (append to `tests/web.rs`):

```rust
#[tokio::test]
async fn microsoft_button_when_sso_off_shows_notice() {
    let app = app().await; // OIDC is off in tests
    let mut b = Browser::new(&app);
    let page = b.get("/login").await;
    assert!(page.body.contains("Sign in with Microsoft"));
    let r = b.get("/auth/oidc/start?next=%2F").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location(), "/login?notice=sso_off");
    let back = b.get("/login?notice=sso_off").await;
    assert!(back.body.contains("Microsoft sign-in isn't set up yet. Contact your administrator."));
    let evil = b.get("/login?notice=%3Cscript%3Ealert(1)%3C%2Fscript%3E").await;
    assert!(!evil.body.contains("alert(1)"));
    assert!(!evil.body.contains("isn't set up yet"));
}

#[tokio::test]
async fn sign_in_page_has_branding_and_no_help_line() {
    let app = app().await;
    let mut b = Browser::new(&app);
    let page = b.get("/login").await;
    assert!(page.body.contains("Know what's due, what's overdue, and who owns it."));
    assert!(page.body.contains("Test Org PDMS"));
    assert!(page.body.contains("href=\"/theme.css?v="));
    assert!(!page.body.to_lowercase().contains("county staff"));
    assert!(page.body.contains("Source code"));
}
```

- [ ] **Step 2: Handle `oidc_start` when it's off.** In `auth.rs:194-209`, replace the 404 with `return Ok(Redirect::to("/login?notice=sso_off").into_response())`, or the code's existing 303 helper (`Redirect::to` is 303 in axum 0.8). Leave `oidc_callback` as it is (404 when off).

- [ ] **Step 3: Add the notice mapping.** `NextQuery` gains `#[serde(default)] notice: String`. Map it in a function:

```rust
fn notice_text(code: &str) -> Option<&'static str> {
    match code {
        "sso_off" => Some("Microsoft sign-in isn't set up yet. Contact your administrator."),
        _ => None,
    }
}
```

Pass it into `render_login`.

- [ ] **Step 4: Rewrite `login.html`** (standalone; spec §7.1; visual reference `D-SignIn.dc.html`).
  - **Head:** links `/static/app.css` and `/theme.css?v={{ theme_version }}`.
  - **Body:** `<div class="signin">` with two sections.
  - **Left section: `<section class="signin-about" aria-label="About PDMS">`:**
    - the brand (seal or mark, "PDMS", org name);
    - h1 "Know what's due, what's overdue, and who owns it.";
    - three points as a `<ul>`;
    - footer links.
  - **Right section: `<main id="main" class="signin-form">`:**
    - h2 "Sign in";
    - error (`.flash.flash-error role="alert"`) and notice (`.flash.flash-info role="status"`; add `.flash-info` with the info tint in `app.css`);
    - **when `local_enabled`:** the form posting `/login/local`, with `_csrf`, `next`, Username (`autocomplete="username"`, `autocapitalize="none"`, `spellcheck="false"`), Password, the authenticator code when `totp_required`, and the **Sign in** `.button.primary` (full width). Then the `<p class="divider"><span>or</span></p>`;
    - **always:** `<a class="button ms" href="/auth/oidc/start?next={{ next_encoded }}">Sign in with Microsoft</a>`. Add a small inline-SVG four-square Microsoft logo with presentation-attribute `fill` colors: `#F25022`, `#7FBA00`, `#00A4EF`, `#FFB900`. These are fixed brand colors; `fill` presentation attributes are allowed.
  - **Remove** the old `<details>` "Emergency administrator sign-in" wrapper.
  - **CSS:** add `.signin`, `.signin-about`, `.signin-form`, `.divider` and `.button.ms` styles to `app.css`. The layout is two columns of 1fr 440px at 900px and up, stacked below.

- [ ] **Step 5: Restyle `error.html`.** Keep it standalone: the org name from `error.rs`, `app.css` and `/theme.css` (no version; it is fine to omit `?v=` here), a centered `.panel` with the message and a "Back to the dashboard" link.

- [ ] **Step 6: Verify.**
  - `cargo test -p pdms-web` (including `tests/oidc.rs`, which configures OIDC; the button links to the real start route there);
  - the CSP guard;
  - clippy and fmt.

- [ ] **Step 7: Commit.** Message: `ui: redesigned sign-in page; Microsoft button always visible with a not-set-up notice`.

---

### Task 7: Dashboard

**Files:**
- **Modify:**
  - `crates/pdms-core/src/appointments.rs` (`attention`, extracted from the web layer);
  - `crates/pdms-web/src/routes/appointments.rs` (use the core function);
  - `crates/pdms-web/src/routes/schedule.rs` (dashboard view model);
  - `crates/pdms-web/templates/dashboard.html` (rewrite);
  - `crates/pdms-web/src/routes/account.rs` or wherever `/account/tokens` lives, plus `templates/account_tokens.html` ("Your access").
- **Create:** `crates/pdms-web/src/dashboard.rs`, the pure helpers below.
- **Test:**
  - `crates/pdms-web/tests/schedule.rs` (append);
  - unit tests in `src/dashboard.rs`;
  - `crates/pdms-core/tests/appointments_attention.rs` (new).

**Interfaces:**
- **Consumes:** `calendar::dashboard`, `reviews::my_open` (`Task { kind: DueEvent, due_date, … }`), `workflow::my_pending` (`PendingStep { submitted_by, submitted_at, kind: StepKind, … }`), `search::search`, `documents::versions`, `appointments::list_positions`, `any_visible`.
- **Produces:**
  - `pdms_core::appointments::{SeatState, SeatAttention, attention(core: &Core, ctx: &Ctx, today: NaiveDate, within_days: i64) -> CoreResult<Vec<SeatAttention>>}`;
  - `SeatState { DecisionOverdue, Holdover, Vacant, TermEnding(NaiveDate) }`, ordered worst first;
  - `SeatAttention { position_id: Uuid, label: String, state: SeatState }`;
  - `crate::dashboard::{greeting, first_name, AttentionRow, order_attention}`.

- [ ] **Step 1: Write the pure helpers with their unit tests.** Create `crates/pdms-web/src/dashboard.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Pure helpers for the dashboard: greeting and "Needs your attention" ordering.

use chrono::{NaiveDate, NaiveTime, Timelike};

pub fn greeting(local: NaiveTime) -> &'static str {
    match local.hour() {
        0..=11 => "Good morning",
        12..=16 => "Good afternoon",
        _ => "Good evening",
    }
}

pub fn first_name(display_name: &str) -> &str {
    display_name.split_whitespace().next().unwrap_or(display_name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    Overdue,
    Approval,
    DueSoon,
    Upcoming,
}

#[derive(Debug, Clone)]
pub struct AttentionRow {
    pub urgency: Urgency,
    /// Overdue: days late (sorted descending). Others: the due or submitted date.
    pub days_late: i64,
    pub date: NaiveDate,
    pub status_class: &'static str,
    pub status_label: &'static str,
    pub item: String,
    pub detail: String,
    pub due_text: String,
    pub due_date_text: String,
    pub action_label: &'static str,
    pub action_primary: bool,
    pub href: String,
}

/// Overdue tasks (most days late first), approvals (oldest first),
/// tasks due within 30 days (soonest first), then other tasks (soonest first).
pub fn order_attention(rows: &mut [AttentionRow]) {
    rows.sort_by(|a, b| {
        a.urgency.cmp(&b.urgency).then_with(|| match a.urgency {
            Urgency::Overdue => b.days_late.cmp(&a.days_late),
            _ => a.date.cmp(&b.date),
        })
    });
}

pub fn task_urgency(today: NaiveDate, due: NaiveDate) -> Urgency {
    let days = (due - today).num_days();
    if days < 0 {
        Urgency::Overdue
    } else if days <= 30 {
        Urgency::DueSoon
    } else {
        Urgency::Upcoming
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn row(u: Urgency, days_late: i64, date: &str, item: &str) -> AttentionRow {
        AttentionRow {
            urgency: u, days_late, date: d(date), status_class: "", status_label: "",
            item: item.into(), detail: String::new(), due_text: String::new(),
            due_date_text: String::new(), action_label: "", action_primary: false, href: String::new(),
        }
    }

    #[test]
    fn greeting_follows_the_clock() {
        assert_eq!(greeting(NaiveTime::from_hms_opt(8, 0, 0).unwrap()), "Good morning");
        assert_eq!(greeting(NaiveTime::from_hms_opt(11, 59, 0).unwrap()), "Good morning");
        assert_eq!(greeting(NaiveTime::from_hms_opt(12, 0, 0).unwrap()), "Good afternoon");
        assert_eq!(greeting(NaiveTime::from_hms_opt(16, 59, 0).unwrap()), "Good afternoon");
        assert_eq!(greeting(NaiveTime::from_hms_opt(17, 0, 0).unwrap()), "Good evening");
        assert_eq!(first_name("Alice Ops"), "Alice");
        assert_eq!(first_name("ada@example.org"), "ada@example.org");
    }

    #[test]
    fn attention_order() {
        let mut rows = vec![
            row(Urgency::Upcoming, 0, "2026-12-31", "later"),
            row(Urgency::DueSoon, 0, "2026-11-07", "soon-b"),
            row(Urgency::Approval, 0, "2026-10-01", "approval-old"),
            row(Urgency::Overdue, 44, "2026-09-01", "late-44"),
            row(Urgency::DueSoon, 0, "2026-11-03", "soon-a"),
            row(Urgency::Overdue, 1263, "2023-05-01", "late-1263"),
            row(Urgency::Approval, 0, "2026-10-10", "approval-new"),
        ];
        order_attention(&mut rows);
        let names: Vec<_> = rows.iter().map(|r| r.item.as_str()).collect();
        assert_eq!(names, ["late-1263", "late-44", "approval-old", "approval-new", "soon-a", "soon-b", "later"]);
        assert_eq!(task_urgency(d("2026-10-15"), d("2026-10-14")), Urgency::Overdue);
        assert_eq!(task_urgency(d("2026-10-15"), d("2026-11-14")), Urgency::DueSoon);
        assert_eq!(task_urgency(d("2026-10-15"), d("2026-11-15")), Urgency::Upcoming);
    }
}
```

Add `mod dashboard;` to `crates/pdms-web/src/lib.rs`.

- [ ] **Step 2: Extract the seat-attention logic into core.**
  - **What exists:** `crates/pdms-web/src/routes/appointments.rs:70-110` computes "Holdover", "Decision due" and "Vacant" labels from `Position::vacancies()` and the appointments' `standing(today)`.
  - **Move it** into `pdms_core::appointments::attention(core, ctx, today, within_days)`. It returns `SeatAttention` rows for positions the caller can see:
    - `list_positions(core, ctx, false)` gives the visible, active positions;
    - for each position, load its active appointments the same way the web handler does today;
    - classify each:
      - `DecisionOverdue`: the appointment is active, `reappointment_due_date < today`, and the term has not ended;
      - `Holdover`: `standing()` returns "Holdover (term ended)";
      - `TermEnding(end_date)`: the end date is within `within_days` and none of the above apply;
      - `Vacant`: `p.vacancies() > 0`, one row per position with label "{position label} ({n} vacant)" when n > 1.
    - Sort by state (enum order), then label.
  - **Keep the web appointments page's behavior identical** by making it call the new function, or a shared private classifier, so there is one source of truth.
  - **Core test** `crates/pdms-core/tests/appointments_attention.rs`:
    - create a department, a position with 2 seats and one appointment whose term ended (holdover);
    - another position with a reappointment due date in the past (`DecisionOverdue`);
    - a third whose term ends in 30 days (`TermEnding`);
    - expect 4 rows: the decision overdue, the holdover, the vacancy from the 2-seat position, and the term ending, in enum order;
    - **visibility:** a user in another department without `ViewAllAppointments` gets an empty list.
    - Use the appointment-creation APIs the existing `pdms-core` tests use (grep `create_position` in `crates/pdms-core/tests`).

- [ ] **Step 3: Build the dashboard view model** (`routes/schedule.rs`).
  - Replace `DashboardPage` with these fields:
    - `l`;
    - `greeting: String` ("Good morning, Alice") and `date_line: String` ("Thursday, October 15, 2026");
    - `days`;
    - the tiles: `tile_overdue: (usize, String)` (count, "Oldest: N days" or "Nothing overdue"), `tile_due30: (usize, String)` ("Next: {date}" or "Nothing due"), `tile_approvals: usize`, `tile_tasks: usize`;
    - `attention: Vec<AttentionRow>`;
    - `upcoming: Vec<UpcomingView>`, the first 8 of `dash.upcoming`. `UpcomingView { month: String ("NOV"), day: String ("3"), title, href, label, department }`;
    - `boards: Option<Vec<SeatView>>`: `None` when `!appointments::any_visible(core, ctx)`. `SeatView { label, status_class, status_text }`, at most 6;
    - `recent: Vec<RecentView>`.
  - **Time:** use the local time in `core.config.timezone()` for the greeting and the date line.
  - **Tile counts:**
    - Overdue counts `dash.overdue.len()`, with the oldest from the max `days_overdue`;
    - Due in 30 days counts the `dash.upcoming` items with `date <= today + 30`. The dashboard loads with `days >= 30`, so the list always covers 30 days;
    - the remaining tiles are `my_pending().len()` and `my_open().len()`.
  - **Attention rows:**
    - **Tasks:**
      - urgency from `task_urgency(today, t.due_date)`;
      - status overdue → ("critical", "Overdue"), due-soon → ("warning", "Due soon"), upcoming → ("neutral", "Upcoming");
      - item `"{kind label}: {title}"`, detail `external_id`;
      - due text "N days late", "In N days" or "Today", with the date;
      - action: "Start review", primary when overdue or due soon, for `DueEvent::Review`; "Open" otherwise;
      - href `/documents/{id}#reviews`.
    - **Approvals:**
      - urgency `Approval`, status ("info", "Awaiting you");
      - item `"{kind.label()}: {title}"`, detail `"{external_id} · submitted by {submitted_by or "system"}"`;
      - due text "Submitted {date}";
      - action "Review" (not primary), href `/documents/{id}#workflow`;
      - date `submitted_at` in local time.
    - Then `order_attention`.
  - **Recent:**
    - `search::search(core, ctx, &DocumentFilter { per_page: 5, ..Default::default() })` (default sort is updated, descending; excludes archived);
    - for each item, `documents::versions(core, ctx, id)` and the newest version with `file_changed`. If `|newest.created_at − item.updated_at| <= 60s`, show "New version by {created_by_name or "PDMS"}", else "Details updated";
    - date text: "Today", "Yesterday" or "%b %-d" in local time.
  - **Remove** the `inbox`, `departments` and `roles` fields from the dashboard.

- [ ] **Step 4: Rewrite `dashboard.html`** (spec §8; visual reference `D-Dashboard.dc.html`):
  - `{% block heading %}`: a header row with h1 `{{ greeting }}`, the date line, and a "County overview" link to `/reports`;
  - a tiles grid of four `.tile`s with stripe classes (`tile-critical`, `tile-warning`, `tile-info`, `tile-accent`), each a link to its target from spec §8;
  - "Needs your attention" (`<section id="attention" class="panel">`) with a `.table-wrap` table of Status, Item, Due and Action, plus the empty state;
  - "Coming up" with `.tabs` for 30/60/90/365 (`/?days=N`, `aria-current` on the active one), the list, the "Calendar" link and the empty state;
  - "Boards and commissions" (only `{% if let Some(seats) = boards %}`) with the "All appointments" link `/positions` and the empty state;
  - "Recently updated" with its empty state "No recent changes.".

- [ ] **Step 5: Move "Your access" to the account page.** On the `/account/tokens` page (find its handler with `grep -rn "account_tokens.html" crates/pdms-web/src`), add a `.panel` at the top listing departments and roles, loaded via `directory::get_user(...).memberships` and `.roles` as the old dashboard did.

- [ ] **Step 6: Write the tests** (append to `crates/pdms-web/tests/schedule.rs`):
  - **`dashboard_greets_and_shows_tiles`:** after login, the home page contains "Good " plus the first name, and the four tile titles "Overdue", "Due in 30 days", "Awaiting your approval", "Your reviews and renewals".
  - **`dashboard_panels_respect_visibility`:**
    - set up two departments, A and B, and a user in A;
    - documents in B: one Department-visibility, one Restricted, one County-wide, each with a review date in 10 days and recently updated;
    - the A user's dashboard "Coming up" and "Recently updated" include the County-wide title and **not** the other two;
    - the Due-in-30 count equals the number of visible items.
    - Create the documents through the core APIs the existing web tests use (grep `documents::create` in `crates/pdms-web/tests`).
  - **`boards_panel_hidden_without_appointment_access`:** a user who cannot see any positions gets no "Boards and commissions" heading.
  - Update the existing `login_sets_session_and_lands_on_home` assertion: `"Welcome, ada@example.org"` becomes `"Good "`.

- [ ] **Step 7: Verify and commit.** Run the full `cargo test --workspace`, clippy and fmt. Message: `ui: dashboard redesign (tiles, needs your attention, boards, recently updated)`.

---

### Task 8: Documents list

**Files:**
- **Modify:**
  - `crates/pdms-core/src/documents/search.rs` (`status_counts`);
  - `crates/pdms-web/src/routes/documents.rs` (`list`, `Row`, `ListPage`);
  - `crates/pdms-web/templates/documents/list.html` (rewrite).
- **Create:** `crates/pdms-web/src/next_date.rs`.
- **Test:**
  - unit tests in `next_date.rs`;
  - `crates/pdms-core/tests/search.rs`, or the existing search test file (append);
  - `crates/pdms-web/tests/documents.rs` (append).

**Interfaces:**
- **Produces:**
  - `pdms_core::documents::search::status_counts(core: &Core, ctx: &Ctx, f: &DocumentFilter) -> CoreResult<Vec<(DocumentStatus, i64)>>`. It ignores `f.status`, `f.include_archived` and paging, and counts every non-deleted visible document per status;
  - `crate::next_date::{NextDate, next_date}`.

- [ ] **Step 1: Write the "next date" rule with its tests.** Create `crates/pdms-web/src/next_date.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! The documents list's single "Next date" column.

use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextDate {
    pub label: &'static str,
    pub date: NaiveDate,
    pub overdue: bool,
}

/// Overdue review or renewal wins (earliest past one); otherwise the earliest
/// future review, renewal, or expiration; otherwise nothing.
pub fn next_date(
    today: NaiveDate,
    review: Option<NaiveDate>,
    renewal: Option<NaiveDate>,
    expiration: Option<NaiveDate>,
) -> Option<NextDate> {
    let past = [(review, "Review"), (renewal, "Renewal")]
        .into_iter()
        .filter_map(|(d, l)| d.filter(|d| *d < today).map(|d| (d, l)))
        .min_by_key(|(d, _)| *d);
    if let Some((date, _)) = past {
        return Some(NextDate { label: "Overdue", date, overdue: true });
    }
    [(review, "Review"), (renewal, "Renewal"), (expiration, "Expires")]
        .into_iter()
        .filter_map(|(d, l)| d.filter(|d| *d >= today).map(|d| (d, l)))
        .min_by_key(|(d, _)| *d)
        .map(|(date, label)| NextDate { label, date, overdue: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn overdue_review_beats_future_dates() {
        let n = next_date(d("2026-10-15"), Some(d("2023-05-01")), Some(d("2027-01-01")), None).unwrap();
        assert_eq!((n.label, n.date, n.overdue), ("Overdue", d("2023-05-01"), true));
    }

    #[test]
    fn earliest_future_date_with_label() {
        let n = next_date(d("2026-10-15"), Some(d("2026-11-03")), Some(d("2026-11-07")), Some(d("2026-12-31"))).unwrap();
        assert_eq!((n.label, n.date, n.overdue), ("Review", d("2026-11-03"), false));
        let n = next_date(d("2026-10-15"), None, None, Some(d("2026-12-31"))).unwrap();
        assert_eq!(n.label, "Expires");
        let n = next_date(d("2026-10-15"), Some(d("2026-10-15")), None, None).unwrap();
        assert_eq!((n.label, n.overdue), ("Review", false), "today is not overdue");
    }

    #[test]
    fn past_expiration_alone_is_nothing() {
        assert_eq!(next_date(d("2026-10-15"), None, None, Some(d("2025-01-01"))), None);
        assert_eq!(next_date(d("2026-10-15"), None, None, None), None);
    }
}
```

Add `mod next_date;` in `lib.rs`.

- [ ] **Step 2: Add `status_counts` in core.**
  - In `search.rs`, build the same WHERE clause as the private `query()` builder (:256-319): visibility predicate, `deleted_at IS NULL`, and every filter **except** status and include_archived. Then `SELECT d.status, count(*) … GROUP BY d.status`.
  - Refactor `query()` so it takes a flag that skips the status and archived clauses, or extract the shared WHERE-building into a helper. Do not duplicate the visibility predicate code.
  - **Core test `status_counts_respect_visibility`:** user in A; B has one Department-visibility doc, one Restricted, one County-wide (status active); A has one draft. The counts for the A user are {active: 1, draft: 1}.

- [ ] **Step 3: Update the list handler.**
  - `ListPage` gains:
    - `tabs: Vec<TabView { label, count, href, current: bool }>`, in the order All, Active, In review, Renewal due, Overdue, Draft, Expired;
    - `more_open: bool`: true when `date_field`, `cf`, `sort`, `dir`, `tag` or a status that is archived or superseded is set;
    - `showing: String` ("Showing 1–6 of 6").
  - **Tabs:**
    - each href is `/documents?{base_query without status and page}&status={value}`;
    - "All" has no status param; its count is the sum of all statuses except archived;
    - `current` comes from `q.status`.
  - **`Row`:**
    - replace `next_review`, `renewal` and `expiration` with `next: Option<NextDate>` and `next_text: String` (`"{label} · {%b %-d, %Y}"`, or "—");
    - add `visibility_label: &'static str` and `visibility_strong: bool` (true for County-wide and Restricted).
  - Today's date comes from `pdms_core::schedule::local_today(Utc::now(), tz)`.

- [ ] **Step 4: Rewrite `list.html`** (spec §10; reference `D-Documents.dc.html`):
  - `{% block heading %}`: a header row with h1 "Documents" and the Export CSV / New document buttons;
  - the `.tabs` row;
  - a GET form with the filter row (Search `q`, Type, Department, an Apply button) and a `<details class="disclosure" {% if more_open %}open{% endif %}><summary>More filters</summary>` holding:
    - every remaining existing control: status select, date field and range, tags and tag mode, custom field and value/min/max, sort, direction, "mine", archived;
    - their names must be unchanged, so the query parameters keep working;
  - the `.table-wrap` table, with an `.status.status-{class}` mapping from the document status (active good, in_review info, renewal_due warning, overdue critical, others neutral);
  - "Showing …" plus the existing pager;
  - the empty state with a "Clear filters" link to `/documents`.

- [ ] **Step 5: Write the web tests** (append to `tests/documents.rs`):
  - **`status_tabs_show_counts_and_keep_filters`:** with `?q=Safety&type=…`, every tab href keeps `q=Safety`, and the "Active" tab count matches.
  - **`next_date_column_shows_overdue_and_labels`:** a document with a past review shows "Overdue · "; one with a future renewal shows "Renewal · ".
  - Update existing list assertions that referenced the "Next review", "Renewal" or "Expires" column headers.

- [ ] **Step 6: Verify and commit.** Run `cargo sqlx prepare --workspace -- --all-targets` if a `query!` changed, the full tests, clippy and fmt. Message: `ui: documents list redesign (status tabs with counts, next date, more filters)`.

---

### Task 9: Document page

**Files:**
- **Modify:**
  - `crates/pdms-web/src/routes/documents.rs` (`show` and `ShowPage`);
  - `crates/pdms-web/templates/documents/show.html` (reorganize);
  - small class-only edits in `documents/workflow.html` and `documents/collab.html` if needed.
- **Create:** `crates/pdms-web/src/timeline.rs` (pure geometry), plus its unit tests.
- **Test:** `crates/pdms-web/tests/documents.rs` (append).

**Interfaces:**
- **Consumes:**
  - the existing `ShowPage` data (`crates/pdms-web/src/routes/documents.rs:703-745`: `open_tasks`, `missed`, `missed_count`, `versions`, `dates`, `d.owner_id`, …);
  - `pdms_core::signatures::list(core, ctx, id) -> Vec<Signatory>`;
  - `documents::versions`.
- **Produces:**
  - `crate::timeline::{Timeline, Mark, MarkKind, build}`;
  - new `ShowPage` fields `key_numbers: Vec<KeyNumber>`, `timeline: Option<Timeline>`, `events: Vec<(String date, String text, bool missed)>`, `signatures: Vec<SigView>`, `start_review: bool`, `section_links: Vec<(&str, &str)>`.

- [ ] **Step 1: Write the timeline geometry with its tests.** Create `crates/pdms-web/src/timeline.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Geometry for the document page's key-dates timeline (SVG, viewBox 0 0 1000 64).

use chrono::{Datelike, NaiveDate};

pub const WIDTH: f64 = 1000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkKind {
    Event,
    Missed,
    Future,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub x: f64,
    pub kind: MarkKind,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    pub marks: Vec<Mark>,
    pub ticks: Vec<(f64, String)>,
    pub today_x: f64,
    /// Start of the red "overdue" span (earliest missed date), if any.
    pub overdue_from_x: Option<f64>,
}

/// `events`: (date, description, kind). Returns None when there are no events.
pub fn build(today: NaiveDate, events: &[(NaiveDate, String, MarkKind)]) -> Option<Timeline> {
    let first = events.iter().map(|e| e.0).min()?;
    let last = events.iter().map(|e| e.0).max()?.max(today);
    let span = (last - first).num_days().max(1) as f64;
    let x = |d: NaiveDate| ((d - first).num_days() as f64 / span * WIDTH).clamp(0.0, WIDTH);
    let marks = events
        .iter()
        .map(|(d, text, kind)| Mark { x: x(*d), kind: *kind, title: format!("{} · {text}", d.format("%b %-d, %Y")) })
        .collect();
    let mut ticks = Vec::new();
    let years = (last.year() - first.year()).max(1);
    let step = if years > 12 { 5 } else if years > 6 { 2 } else { 1 };
    let mut y = first.year();
    while y <= last.year() {
        if let Some(d) = NaiveDate::from_ymd_opt(y, 1, 1) {
            if d >= first {
                ticks.push((x(d), y.to_string()));
            }
        }
        y += step;
    }
    let overdue_from_x = events.iter().filter(|e| e.2 == MarkKind::Missed).map(|e| x(e.0)).reduce(f64::min);
    Some(Timeline { marks, ticks, today_x: x(today), overdue_from_x })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn places_events_and_today_proportionally() {
        let ev = vec![
            (d("2013-05-01"), "Executed and effective".to_string(), MarkKind::Event),
            (d("2018-05-01"), "Five-year review".to_string(), MarkKind::Missed),
            (d("2023-05-01"), "Five-year review".to_string(), MarkKind::Missed),
        ];
        let t = build(d("2026-10-15"), &ev).unwrap();
        assert_eq!(t.marks[0].x, 0.0);
        assert!((t.today_x - WIDTH).abs() < 1e-9, "today is the latest point");
        assert!(t.marks[1].x > 300.0 && t.marks[1].x < 450.0);
        assert_eq!(t.overdue_from_x, Some(t.marks[1].x));
        assert!(t.marks[1].title.starts_with("May 1, 2018"));
        assert!(!t.ticks.is_empty());
    }

    #[test]
    fn future_events_extend_the_axis() {
        let ev = vec![(d("2026-01-01"), "Effective".to_string(), MarkKind::Event), (d("2027-01-01"), "Renewal".to_string(), MarkKind::Future)];
        let t = build(d("2026-10-15"), &ev).unwrap();
        assert!(t.today_x < WIDTH);
        assert_eq!(t.marks[1].x, WIDTH);
        assert_eq!(build(d("2026-10-15"), &[]), None);
    }
}
```

- [ ] **Step 2: Extend the `show` handler.**
  - **Events:**
    - the effective date ("Effective");
    - each version with `file_changed` ("Version N uploaded", or the version note when present, as `Event`);
    - each missed cycle date, from the same source as `missed`/`missed_count` ("Review missed", as `Missed`);
    - the next review, renewal and expiration ("Next review", "Renewal", "Expires": `Future` if ≥ today, `Event` if past and not already counted as missed).
  - Sort by date; build `events` (the list) and `timeline`.
  - **Key numbers** (`KeyNumber { label, value, sub, class }`; only those that apply):
    - Days overdue: from the most overdue open task, or the earliest missed cycle; class `critical`;
    - otherwise "Days until {kind}" for the nearest future review, renewal or expiration;
    - "Missed review cycles" when `missed_count > 0`;
    - "Versions" (count of file versions, sub "Latest {date}");
    - "Signatures" when `signatures::list` is non-empty (`"{signed} of {total}"`, sub "{pending} pending" or "All signed").
  - **`start_review`:** true when `user.ctx.user_id() == Some(d.owner_id)` and an open task with `kind == Review` is due within 30 days or overdue.
  - **`section_links`:**
    - ("dates", "Key dates") when there are events;
    - ("reviews", "Review & approval");
    - ("files", "Files");
    - ("comments", "Comments") when the collab section renders;
    - ("manage", "Manage") when any manage permission is true.

- [ ] **Step 3: Reorganize `show.html`** (spec §11; reference `D-Document.dc.html`). Override `{% block heading %}`.
  - **Every existing form, `id`, `name` and permission condition must survive unchanged.**
  - Only the order and wrapping change, plus the new blocks.
  - Move these existing sections into `<details class="disclosure" id="manage">`: Who can see it, Access, Primary record owner, Retention, Delete.
  - Render the timeline SVG as `<svg class="timeline" viewBox="0 0 1000 64" aria-hidden="true" focusable="false">`. It contains:
    - an axis line, and an `overdue_from_x` span (class `timeline-overdue`);
    - marks as `<circle cx cy="22" r="7" class="timeline-mark timeline-{kind}"><title>…</title></circle>`;
    - a today line (class `timeline-today`) and tick labels (`<text class="chart-label">`).
  - Then the `<ol>` event list.
  - **CSS:** add the timeline classes to `app.css`.

- [ ] **Step 4: Write the tests** (append to `tests/documents.rs`):
  - **`document_page_keeps_every_action`:** as an admin, the page still contains the forms for the status move, the tag add, the version upload, visibility, grant add, retention and delete. Assert on their `action="…"` URLs, which are unchanged. "Manage this document" wraps the last ones.
  - **`start_review_button_only_for_owner_with_due_review`:** an owner with an overdue review task sees "Start review"; another user with view access does not.
  - **`key_numbers_show_versions_and_overdue`:** a document with 2 file versions and an overdue review shows "Versions" with 2, and "Days overdue".

- [ ] **Step 5: Verify and commit.** Full tests, clippy and fmt. Message: `ui: document page redesign (key numbers, key-dates timeline, side panel, manage section)`.

---

### Task 10: County overview and charts

**Files:**
- **Create:**
  - `crates/pdms-web/src/charts.rs` (pure geometry and tests);
  - `crates/pdms-web/templates/charts/vbars.html`, `charts/hbars.html`, `charts/stacked.html`.
- **Modify:**
  - `crates/pdms-web/src/routes/exports.rs` (`reports_index` view model);
  - `crates/pdms-web/templates/reports/index.html`;
  - `crates/pdms-web/static/app.css` (chart classes, if not done in Task 1).
- **Test:**
  - `crates/pdms-web/tests/phase4.rs`, or whichever file covers reports, via grep `"/reports"` (append);
  - unit tests in `charts.rs`.

**Interfaces:**
- **Consumes:**
  - `calendar::dashboard(core, ctx, &filter, 1)` (overdue);
  - `calendar::events(core, ctx, &filter, today, end, false)` (upcoming);
  - `search::status_counts` (Task 8);
  - `search::all` (types and departments count, capped);
  - `appointments::attention` (Task 7) and `list_positions` (seat totals);
  - `directory::list_departments`, filtered to departments that appear in the viewer's visible documents.
- **Produces:**
  - `crate::charts::{VBars, HBars, Stacked, vbars, hbars, stacked}`;
  - the overview fields on `ReportsIndex`.

- [ ] **Step 1: Write the chart geometry with its tests.** Create `crates/pdms-web/src/charts.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Server-side chart geometry. Templates emit SVG; colors come from CSS classes.

#[derive(Debug, Clone, PartialEq)]
pub struct VBar {
    pub path: String,
    pub x_mid: f64,
    pub top: f64,
    pub value: i64,
    pub label: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VBars {
    pub width: f64,
    pub height: f64,
    pub baseline: f64,
    pub bars: Vec<VBar>,
    pub aria: String,
}

/// Rounded top corners only (data end), square at the baseline.
fn top_rounded(x: f64, y: f64, w: f64, h: f64, r: f64) -> String {
    if h <= 0.0 {
        return String::new();
    }
    let r = r.min(w / 2.0).min(h);
    format!(
        "M{x:.1},{b:.1}V{y1:.1}Q{x:.1},{y:.1} {x1:.1},{y:.1}H{x2:.1}Q{xw:.1},{y:.1} {xw:.1},{y1:.1}V{b:.1}Z",
        b = y + h, y1 = y + r, x1 = x + r, x2 = x + w - r, xw = x + w
    )
}

/// Vertical bars for (label, value, tooltip) with an aria summary prefix.
pub fn vbars(data: &[(String, i64, String)], aria_prefix: &str) -> VBars {
    let (width, height, top_pad, label_h) = (600.0, 220.0, 24.0, 28.0);
    let baseline = height - label_h;
    let max = data.iter().map(|d| d.1).max().unwrap_or(0).max(1) as f64;
    let n = data.len().max(1) as f64;
    let slot = width / n;
    let bar_w = (slot * 0.5).min(48.0);
    let bars = data
        .iter()
        .enumerate()
        .map(|(i, (label, value, title))| {
            let h = (*value as f64 / max) * (baseline - top_pad);
            let x = slot * i as f64 + (slot - bar_w) / 2.0;
            VBar {
                path: top_rounded(x, baseline - h, bar_w, h, 4.0),
                x_mid: x + bar_w / 2.0,
                top: baseline - h,
                value: *value,
                label: label.clone(),
                title: title.clone(),
            }
        })
        .collect();
    let summary: Vec<String> = data.iter().map(|d| format!("{} {}", d.0, d.1)).collect();
    VBars { width, height, baseline, bars, aria: format!("{aria_prefix}: {}.", summary.join(", ")) }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HBar {
    pub y: f64,
    pub path: String,
    pub end_x: f64,
    pub value: i64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HBars {
    pub width: f64,
    pub height: f64,
    pub label_w: f64,
    pub bars: Vec<HBar>,
    pub aria: String,
}

/// Horizontal bars, rounded at the right (data) end.
pub fn hbars(data: &[(String, i64)], aria_prefix: &str) -> HBars {
    let (width, label_w, row_h, bar_h) = (600.0, 180.0, 34.0, 18.0);
    let plot = width - label_w - 40.0;
    let max = data.iter().map(|d| d.1).max().unwrap_or(0).max(1) as f64;
    let bars = data
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let y = i as f64 * row_h + (row_h - bar_h) / 2.0;
            let w = (*value as f64 / max) * plot;
            let r = 4.0_f64.min(w / 2.0).min(bar_h / 2.0);
            let path = if w <= 0.0 {
                String::new()
            } else {
                format!(
                    "M{x:.1},{y:.1}H{xr:.1}Q{xe:.1},{y:.1} {xe:.1},{yr:.1}V{yb2:.1}Q{xe:.1},{yb:.1} {xr:.1},{yb:.1}H{x:.1}Z",
                    x = label_w, xr = label_w + w - r, xe = label_w + w, yr = y + r, yb2 = y + bar_h - r, yb = y + bar_h
                )
            };
            HBar { y, path, end_x: label_w + w, value: *value, label: label.clone() }
        })
        .collect();
    let summary: Vec<String> = data.iter().map(|d| format!("{} {}", d.0, d.1)).collect();
    HBars {
        width,
        height: (data.len().max(1) as f64) * row_h,
        label_w,
        bars,
        aria: format!("{aria_prefix}: {}.", summary.join(", ")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub x: f64,
    pub w: f64,
    pub class: &'static str,
    pub label: &'static str,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stacked {
    pub width: f64,
    pub segments: Vec<Segment>,
    pub total: i64,
    pub aria: String,
}

/// One stacked bar with 2px gaps between non-empty segments.
pub fn stacked(parts: &[(&'static str, &'static str, i64)], aria_prefix: &str) -> Stacked {
    let width = 600.0;
    let total: i64 = parts.iter().map(|p| p.2).sum();
    let nonzero: Vec<_> = parts.iter().filter(|p| p.2 > 0).collect();
    let gaps = nonzero.len().saturating_sub(1) as f64 * 2.0;
    let usable = (width - gaps).max(0.0);
    let mut x = 0.0;
    let mut segments = Vec::new();
    for (class, label, count) in nonzero {
        let w = if total > 0 { *count as f64 / total as f64 * usable } else { 0.0 };
        segments.push(Segment { x, w, class, label, count: *count });
        x += w + 2.0;
    }
    let summary: Vec<String> = parts.iter().map(|p| format!("{} {}", p.1, p.2)).collect();
    Stacked { width, segments, total, aria: format!("{aria_prefix}: {}.", summary.join(", ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vbars_scale_to_the_tallest_and_round_the_top() {
        let c = vbars(&[("Nov".into(), 3, "November 2026: 3 due".into()), ("Dec".into(), 1, "December 2026: 1 due".into()), ("Jan".into(), 0, "January 2027: 0 due".into())], "Items due per month");
        assert_eq!(c.bars.len(), 3);
        assert!(c.bars[0].top < c.bars[1].top, "3 is taller than 1");
        assert!(c.bars[2].path.is_empty(), "zero has no mark");
        assert!(c.bars[0].path.starts_with('M') && c.bars[0].path.contains('Q'));
        assert_eq!(c.aria, "Items due per month: Nov 3, Dec 1, Jan 0.");
    }

    #[test]
    fn stacked_leaves_two_pixel_gaps_and_skips_empty() {
        let s = stacked(&[("chart-seg-good", "Active", 3), ("chart-seg-info", "In review", 1), ("chart-seg-warning", "Renewal due", 0), ("chart-seg-critical", "Overdue", 2)], "Documents by status");
        assert_eq!(s.total, 6);
        assert_eq!(s.segments.len(), 3);
        let gap = s.segments[1].x - (s.segments[0].x + s.segments[0].w);
        assert!((gap - 2.0).abs() < 1e-9);
        let end = s.segments[2].x + s.segments[2].w;
        assert!((end - 600.0).abs() < 1e-6);
    }

    #[test]
    fn hbars_rows_and_empty_input() {
        let h = hbars(&[("Operations".into(), 2), ("Human Resources".into(), 1)], "Overdue by department");
        assert_eq!(h.bars.len(), 2);
        assert!(h.bars[0].end_x > h.bars[1].end_x);
        let e = hbars(&[], "Overdue by department");
        assert!(e.bars.is_empty());
    }
}
```

Add `mod charts;` to `lib.rs`.

- [ ] **Step 2: Write the chart partials.** Each one expects a variable named `chart` in scope; set it with `{% let chart = … %}` before the include.
  - **`charts/vbars.html`:**
    ```html
    <svg class="chart" viewBox="0 0 {{ chart.width }} {{ chart.height }}" role="img" aria-label="{{ chart.aria }}">
    ```
    It contains:
    - a baseline `<line class="chart-axis" …>`;
    - for each bar: `<path class="chart-bar" d="{{ b.path }}"><title>{{ b.title }}</title></path>`, the value `<text class="chart-value" x="{{ b.x_mid }}" y="{{ b.top - 6.0 }}" text-anchor="middle">`, and the label `<text class="chart-label" … y="{{ chart.baseline + 18.0 }}">`.
    - If Askama cannot do float arithmetic in the template, precompute `value_y` and `label_y` in the struct. They are presentation attributes, so the CSP is fine.
  - **`charts/hbars.html`:** a label `<text>` at x = `label_w − 10`, right-anchored; the path with `class="{{ class }}"`, set by the caller as `bar_class`; the value at `end_x + 6`.
  - **`charts/stacked.html`:** an SVG `<rect x w height="28" class="{{ s.class }}" rx="4">` per segment, with a `<title>`, then an HTML legend `<ul class="legend">` with a dot in `legend-{class}` and "{label} {count}". Status colors go with words.

- [ ] **Step 3: Build the overview data in `reports_index`.**
  - **Query parameters:** `months: Option<u32>` (only 6 or 12, default 6) and `department: Option<Uuid>`.
  - **Department filter:** applied through `DocumentFilter { department_id, ..Default }`.
  - **Tiles:**
    - Documents: the sum of `status_counts`, excluding archived and superseded; sub "{types} types · {departments} departments", from `search::all(…, 20_000)` distinct `type_name` and `department_name`. If it returns 20,000 rows, set a `capped` flag.
    - Overdue: `calendar::dashboard(core, ctx, &f, 1).overdue` (count and max `days_overdue`).
    - Due in 90 days: `calendar::events(core, ctx, &f, today, today + 90, false)` (count, first date).
    - In review: `status_counts[in_review]`.
  - **Charts:**
    1. **Due by month:** `calendar::events` for `today..today + months`, bucketed by `(year, month)` over every month in the range, including zero months. Label "Nov", tooltip "November 2026: 3 due". Use `vbars`.
    2. **Status breakdown:** from `status_counts`. Active → good, In review → info, Renewal due → warning, Overdue → critical, Other (draft + expired + superseded) → neutral. Use `stacked`.
    3. **Overdue by department:** group `dashboard.overdue` by `event.department`, sorted by count descending. Use `hbars` with bar class `chart-seg-critical`. Show the empty state "Nothing is overdue." when empty.
    4. **Boards summary:** from `appointments::attention(core, ctx, today, 90)` counts per state, plus "Filled and current" = Σ `filled` over visible active positions minus the holdover, decision-overdue and term-ending appointments. Hidden when `!any_visible`.
  - **Department select options:** the departments among the visible documents, from `search::all` (distinct ids and names; extend the summary if the id is not present), sorted by name.
  - **"View as table" links:** `/reports/upcoming?days={months*31}`, `/documents`, `/reports/overdue`, `/positions`.

- [ ] **Step 4: Rewrite `reports/index.html`.**
  - `{% block heading %}` "Reports".
  - A `.panel` "County overview" containing:
    - a GET filter row with Period and Department selects and Apply;
    - the "Based on the first 20,000 documents you can see." note when `capped`;
    - the tiles;
    - a 2×2 grid of chart panels, each with a title, subtitle and the "View as table" link.
  - Then the existing report list under h2 "All reports", unchanged in content.

- [ ] **Step 5: Write the tests** (append to the reports test file):
  - **`overview_counts_respect_visibility`:** the same three-document setup as Task 7 (B: Department, Restricted, County-wide; the user in A). The overview's Documents tile counts only A's documents plus the County-wide one, and the status breakdown legend total matches.
  - **`overview_charts_render_as_svg_with_labels`:** the page contains `role="img"`, an `aria-label` starting with "Items due per month:", and "View as table".
  - **`overview_period_and_department_filters`:** `?months=12` shows 12 month labels; an invalid `?months=7` falls back to 6; `?department=<A id>` excludes the County-wide B document from the Documents tile.

- [ ] **Step 6: Verify and commit.** Full tests, clippy and fmt. Message: `ui: county overview on Reports with server-rendered charts`.

---

### Task 11: Remaining pages, visual verification

**Files:**
- **Modify:** any template under `crates/pdms-web/templates/` that renders badly in the new shell.
  - Light-touch only: wrap wide tables in `<div class="table-wrap">`, swap obsolete classes, add `{% block heading %}` overrides where a page prints its own h1 and would now show two.
  - Do not change content or forms.
- **Create:** a screenshot script **in the scratchpad**, not the repository: `$R1_SCRATCH/r1-shots/shoot.py`.

- [ ] **Step 1: Run a local instance with demo data.**
  - Copy `config/pdms.example.toml` to `$R1_SCRATCH/r1-shots/pdms.toml`. In it:
    - point the database URLs at the dev database (`postgres://pdms_owner:devowner@127.0.0.1:55432/pdms` for the owner, and the app role URL per the example's comments);
    - bind `127.0.0.1:18080`;
    - set `secure_cookies = false` and `[auth.local] require_totp = false`;
    - storage path in the scratchpad;
    - OCR and ClamAV off.
  - Run `cargo run -p pdms -- --config …/pdms.toml migrate`, then `seed-demo`, then `serve --with-worker` in the background.
  - Read `crates/pdms-core/src/demo.rs` for the demo accounts and their password.

- [ ] **Step 2: Take screenshots.**
  - Use the local Playwright image:
    ```bash
    docker run --rm --network host -v $R1_SCRATCH/r1-shots:/w mcr.microsoft.com/playwright/python:v1.49.0-jammy python /w/shoot.py
    ```
  - **`shoot.py`:**
    - signs in through `/login` as a demo admin;
    - captures full-page PNGs at 1440×900 for every GET page reachable from the sidebar and admin tabs: dashboard, documents list, one document, calendar, positions and one position, reports and one report, exports, notifications, audit log and verify, account tokens, every `/admin/*` page, `/login` (signed out) and an error page (`/documents/00000000-0000-0000-0000-000000000000`);
    - also captures the five key screens at 390×844.
  - Write the PNGs to `/w/out/`.

- [ ] **Step 3: Inspect every screenshot.** Read the PNGs and check that each has:
  - no horizontal page scroll (wide tables scroll inside `.table-wrap`);
  - no overlapping or cut-off text;
  - no doubled h1;
  - visible focus outlines when tabbing. Do a quick keyboard pass in Playwright: press Tab 10 times on the dashboard and screenshot.
  - Fix what's wrong, then re-shoot.

- [ ] **Step 4: Stop the local server and clean up.** Leave the dev database migrated at 0009; it is the shared dev DB, so the demo rows can stay.

- [ ] **Step 5: Commit the template fixes.** Message: `ui: light-touch fixes so every page fits the new shell`.

---

### Task 12: Documentation and final verification

**Files:**
- **Modify:** `docs/DECISIONS.md`, `docs/ADMIN_GUIDE.md`, `docs/ROADMAP.md`, `README.md` (only if it describes the old UI or the email-only sign-in).

- [ ] **Step 1: Add the decisions** to `docs/DECISIONS.md`, matching the existing entry format (`### D-0NN Title — Accepted (2026-10-15)` plus a paragraph):
  - **D-092:** UI direction D, chosen from four demos; the R1 scope; R1 ahead of sub-projects 1–3; R2 after them.
  - **D-093:** branding set on an admin page (county name, accent with a 4.5:1 white-text rule, PNG/JPEG seal of at most 512 KB, stored in PDMS's database); public before sign-in; the organization-name resolver is used for emails, feeds, webhooks and exports.
  - **D-094:** sign-in page with username and password first and Microsoft always visible ("not set up yet" notice when off); usernames for local accounts (2–64 of `[A-Za-z0-9._-]`, unique ignoring case, no `@`); one generic failure message.
  - **D-095:** the dashboard shows "Recently updated" instead of an activity feed, so the audit log is not exposed; no "Share" button.

- [ ] **Step 2: Update `docs/ADMIN_GUIDE.md`.**
  - a "Branding" section (what each field does, the contrast rule, seal rules, public visibility, reset);
  - "Usernames for local accounts" (admin page, `pdms set-username`, `install.sh` prompt and `PDMS_INSTALL_ADMIN_USERNAME`);
  - "Preparing a demo for leadership": spec §16 step 4, with commands and click-paths and no hostnames.
  - Update any place that says local sign-in uses an email address only.

- [ ] **Step 3: Update `docs/ROADMAP.md` §0.3.** Row 5a's status becomes "**Done** (2026-10-15)", with a link to this plan. Add one line under §0.1 Done.

- [ ] **Step 4: Run the full verification** from a clean shell with the dev env exported:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `SQLX_OFFLINE=true cargo sqlx prepare --workspace --check -- --all-targets` (the CI check)
  - `cargo test --workspace`. Record the pass count. Every test must pass; only the two existing opt-in ignored tests may be ignored.
  - `bash -n install.sh`

- [ ] **Step 5: Commit.** Message: `docs: decisions D-092..D-095, admin guide for branding and usernames, roadmap R1 done`.
