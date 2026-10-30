# DV: viewing documents in the browser (phase 1). Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A **View** button opens a PDF or image in a new browser tab,
shown by the browser's own viewer, with the same permission check as a
download and audited as `document.downloaded` with `disposition: inline`.

**Architecture:** `pdms-core` gains one small module holding the list of
viewable types, and a `view_file` beside `open_file`; both share one body
that records the disposition it chose in the audit. The web crate gets one
new address, `GET /documents/{id}/versions/{n}/view`. Its response carries a
marker, which makes `middleware::security_headers` send the stricter view
policy instead of the page policy. The document page shows **View** for
viewable files.

**Tech Stack:** Rust 2024, Axum, Askama, SQLx (PostgreSQL).

**Spec:** [2026-10-30-document-viewing-design.md](../specs/2026-10-30-document-viewing-design.md)
(phase 1 approved by the owner on 2026-10-30; phase 2, Office files as PDF,
is **not** in this plan). Read §2, §3, §6 and §7 before any task.

## Global Constraints

- Work goes straight to `main` in small commits. Each commit builds and
  passes the checks under "Commands" in `CLAUDE.md`. Commit messages start
  with `DV:`. Pull before you push, and never force-push.
- Keep the DV row in ROADMAP §0.1a current: each task marked In progress or
  Done, with the date.
- **No migration.** **Decision:** D-118, the next after D-117 (pull and
  re-check before Task 4).
- Every new source file starts with `SPDX-License-Identifier: AGPL-3.0-or-later`
  and `Copyright (C) 2026 PDMS contributors`. Test files put those two lines
  first, then the `#![allow(...)]` block the other tests use.
- No `unwrap`, `expect`, `panic` or `dbg` outside tests.
- Templates: no `style="…"` attributes, no inline `<script>`, no `on…=`
  handlers. WCAG 2.1 AA, and one `<h1>` per page. Colours only from the
  tokens in `static/app.css`.
- **The viewable types, exactly:** `application/pdf`, `image/png`,
  `image/jpeg`, `image/gif`, `image/webp`. The decision goes by the stored,
  sniffed `blobs.media_type` (D-027), never by the file name.
- **The view policy, exactly** (one line, single spaces):
  `default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'; sandbox`
- **The page policy is unchanged:**
  `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'`
- **Audit:** every file sent writes `document.downloaded` with context
  `disposition` = `inline` or `attachment`, besides the existing `version`,
  `sha256` and `file_name`. `document.viewed` keeps meaning "opened the
  document page" (D-034).
- The view response sends no `Accept-Ranges` header.
- Everything else a download does (the `View` permission, visibility, the
  SharePoint check that stops a file changed outside PDMS) applies to a view
  unchanged, because both go through the same core code.

## Review Focus

1. **A version that doesn't exist** (`/versions/99/view`) answers exactly
   as the download address does (404), and writes no audit event. Test in
   Task 2.
2. **A file name with quotes and non-ASCII characters** gets the same safe
   `filename=` / `filename*=` pair inline as it does as an attachment. Test
   in Task 2.
3. **An error answer at the view address** (403 for someone without access,
   404) is an ordinary PDMS page and must carry the page policy, not the
   view policy. Test in Task 2.
4. **The latest version only changed the details**, not the file: the header
   **View** points at that version, and it serves the file it carries. Test
   in Task 3.
5. **A stored type that is empty or not on any list** (an old blob) is never
   shown inline. Test in Task 1.

---

### Task 1: Which files can be viewed, and `view_file`

**Files:**
- Create: `crates/pdms-core/src/documents/viewing.rs`
- Modify: `crates/pdms-core/src/documents/mod.rs` (add `pub mod viewing;`
  and `pub use viewing::{Disposition, viewable};`)
- Modify: `crates/pdms-core/src/documents/service.rs` (`FileDownload`,
  `open_file`, new `view_file`)
- Test: unit tests in `viewing.rs`; `crates/pdms-core/tests/documents.rs`;
  `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Produces, in `pdms_core::documents`:
  - `pub const VIEWABLE_TYPES: [&str; 5]` (in `viewing`), the list from
    Global Constraints;
  - `pub fn viewable(media_type: &str) -> bool`;
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Disposition { Attachment, Inline }`
    with `pub fn as_str(self) -> &'static str` returning `"attachment"` or
    `"inline"`;
  - `FileDownload` gains `pub disposition: Disposition`;
  - `open_file(core, ctx, id, version_no: Option<i32>) -> CoreResult<FileDownload>`:
    signature unchanged, always `Attachment`;
  - `pub async fn view_file(core: &Core, ctx: &Ctx, id: Uuid, version_no: i32) -> CoreResult<FileDownload>`:
    `Inline` when `viewable(stored type)`, otherwise `Attachment`.

- [ ] **Step 1: Write the failing tests**

Unit test in `viewing.rs`:

```rust
#[test]
fn only_the_listed_types_are_viewable() {
    for t in ["application/pdf", "image/png", "image/jpeg", "image/gif", "image/webp"] {
        assert!(viewable(t), "{t}");
    }
    for t in ["", "text/plain", "text/html", "image/svg+xml", "image/tiff",
              "application/msword", "application/pdf; charset=utf-8", "APPLICATION/PDF"] {
        assert!(!viewable(t), "{t}");
    }
}
```

In `crates/pdms-core/tests/documents.rs`, a new test
`viewing_is_audited_as_a_download_with_its_disposition`. Use the file's
existing helpers (`editing_department`, `new_doc`, `meta`, `pdf`,
`read_all`, `as_user`, `user_in_new_department`):

- a PDF document: `open_file(.., None)` → `disposition == Attachment`; the
  newest `document.downloaded` row has `context->>'disposition' = 'attachment'`;
- `view_file(.., 1)` → `disposition == Inline`, `media_type == "application/pdf"`,
  bytes equal the upload; the newest `document.downloaded` row has
  `disposition = 'inline'`, `version = 1`, and the same `sha256` and
  `file_name` as the download's row;
- a plain-text document whose content is
  `<html><script>alert(1)</script></html>` (uploaded as `notes.txt`):
  `view_file(.., 1)` → `disposition == Attachment`, audited `attachment`;
- Bob from another department: `view_file` → `Err(CoreError::Forbidden(_))`,
  and the count of `document.downloaded` rows is unchanged.

In `crates/pdms-core/tests/sharepoint.rs`,
`changed_files_stop_the_download_and_are_flagged`: after the `tamper`, also
assert that `read_all(view_file(&core, &ctx, id, 1).await.unwrap().stream).await.is_err()`,
the same as the download.

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test -p pdms-core --lib viewing && cargo test -p pdms-core --test documents viewing_is_audited`
Expected: compile errors (`viewable`, `view_file`, `disposition` not found).

- [ ] **Step 3: Implement**

`viewing.rs`: module doc "Which stored files the browser may show (D-118)".
`viewable` is an exact match against `VIEWABLE_TYPES`.

`service.rs`: move the body of `open_file` into a private
`open_file_as(core, ctx, id, version_no: Option<i32>, inline_if_viewable: bool)`.
It decides the disposition from the row's `media_type` after the existing
query and before the audit, and adds `.context("disposition", d.as_str())`
to the existing `document.downloaded` event. `open_file` passes `false`;
`view_file` passes `Some(version_no)` and `true`. Update `open_file`'s doc
comment, and give `view_file` one: "Open a version's file to show in the
browser: inline only for a viewable type (D-118). Requires `view`; audited
as a download."

- [ ] **Step 4: Run the tests**

Run: `cargo test -p pdms-core --lib viewing && cargo test -p pdms-core --test documents && cargo test -p pdms-core --test sharepoint changed_files`
Expected: PASS. `api.rs` and `routes/documents.rs` still compile: they
don't read the new field yet.

- [ ] **Step 5: Run the checks, then commit and push**

`cargo sqlx prepare --workspace --check -- --all-targets` must pass (no
query text changes). Commit `DV: which files can be viewed, and view_file`.

---

### Task 2: The view address and the view policy

**Files:**
- Modify: `crates/pdms-web/src/middleware.rs` (`security_headers`)
- Modify: `crates/pdms-web/src/routes/documents.rs` (`content_disposition`,
  `download_inner`, new `view_version`, and the unit test module)
- Modify: `crates/pdms-web/src/routes/api.rs` (only if it must adapt to the
  helper's new shape; the API stays download-only)
- Modify: `crates/pdms-web/src/lib.rs` (route table, beside
  `/{id}/versions/{n}/download`)
- Test: `crates/pdms-web/tests/documents.rs`

**Interfaces:**
- Consumes: `documents::{view_file, Disposition, FileDownload}` (Task 1).
- Produces:
  - `middleware.rs`: `const PAGE_CSP: &str` (the existing string, moved to
    a constant), `const VIEW_CSP: &str` (Global Constraints), and
    `#[derive(Clone, Copy, Debug)] pub(crate) struct ViewPolicy;`. A
    response whose extensions hold `ViewPolicy` gets `VIEW_CSP`; every other
    response gets `PAGE_CSP`. All other headers are set as today.
  - `routes/documents.rs`:
    - `pub(crate) fn content_disposition_as(d: Disposition, name: &str) -> String`,
      with `content_disposition(name)` left as
      `content_disposition_as(Disposition::Attachment, name)`, so its
      callers in `exports.rs` and `api.rs` don't change;
    - `fn file_response(f: FileDownload) -> Response`, which `download_inner`
      and `view_version` share. It inserts `ViewPolicy` into the response's
      extensions only when `f.disposition == Disposition::Inline`;
    - `pub async fn view_version(State(state): State<AppState>, user: CurrentUser, Path((id, n)): Path<(Uuid, i32)>) -> AppResult<Response>`.
  - Route: `.route("/{id}/versions/{n}/view", get(documents::view_version))`.

- [ ] **Step 1: Write the failing tests**

In `crates/pdms-web/tests/documents.rs`, with the file's helpers (`app`,
`editor`, `Browser`, `create_via_form`, `pdf`). Add byte helpers for the
image types: PNG as in `shell.rs`; JPEG `FF D8 FF E0 00 10 4A 46 49 46 00`;
GIF `GIF89a` then a few bytes; WebP `RIFF`, 4 size bytes, `WEBPVP8 `. Add a
legacy Word helper: the OLE signature `D0 CF 11 E0 A1 B1 1A E1` then 504
zero bytes, uploaded as `minutes.doc`. Write `VIEW_CSP` and `PAGE_CSP` out
as string literals in the test file, copied from Global Constraints.

1. `view_shows_pdfs_and_images_in_the_browser`: for each of
   `(file name, bytes, type)`:
   - `report.pdf`;
   - `photo.png`, `scan.jpg`, `map.gif` and `chart.webp`.

   Create a document, then `GET {doc}/versions/1/view`:
   - status 200;
   - `content-type` == the type;
   - `content-disposition` starts with `inline;` and contains the file name;
   - `x-content-type-options` == `nosniff`;
   - `cache-control` == `private, no-store`;
   - no `accept-ranges` header;
   - `content-security-policy` == `VIEW_CSP`;
   - `x-frame-options` == `DENY`;
   - `content-length` == the byte count.

   For the PDF, also the body equals the upload.
2. `view_sends_other_types_as_downloads`: `minutes.doc` (legacy Word) and
   `notes.txt` with content `<html><script>alert(1)</script></html>`. Both
   answer 200 with `content-disposition` starting `attachment;`,
   `content-security-policy` == `PAGE_CSP`, and the same body as their
   `/download`.
3. In `read_only_and_outside_users_are_denied_on_every_path`, add
   `format!("{doc}/versions/1/view")` to the GET list (403).
4. `only_the_view_answer_gets_the_view_policy`: `PAGE_CSP` on:
   - the document page;
   - `{doc}/download`;
   - `/static/app.css`;
   - `{doc}/versions/99/view`, which answers 404, the same as
     `{doc}/versions/99/download`, and adds no `document.downloaded` row
     (count it with `actions(&app.core)` from `support.rs`);
   - Bob's 403 at `{doc}/versions/1/view`.

   None of them contains `sandbox`.
5. `view_keeps_odd_file_names_safe`: upload `Résumé "final".pdf`. The
   view's `content-disposition` equals the download's with its first word
   changed: `view == download.replacen("attachment", "inline", 1)`. The
   download's value must contain `filename*=UTF-8''`.
6. `each_version_is_viewed_with_its_own_file`: add a second PDF as version
   2, posting to `{doc}/versions` as
   `upload_new_version_and_edit_details` does. `{doc}/versions/1/view` then
   returns the first file's bytes, and `{doc}/versions/2/view` the second's.

Also extend the unit test `content_disposition_is_safe` in
`routes/documents.rs`: `content_disposition_as(Disposition::Inline, …)`
equals the attachment string with `attachment` replaced by `inline`.

- [ ] **Step 2: Run them to make sure they fail**

Run: `cargo test -p pdms-web --test documents -- view_ only_the_view read_only_and_outside each_version`
Expected: 404 at the view address, and compile errors for the unit test.

- [ ] **Step 3: Implement** as in Interfaces. `view_version` calls
`documents::view_file(&state.core, &user.ctx, id, n)` and returns
`file_response(f)`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p pdms-web` (all web tests: the policy change touches
every response). Expected: PASS, including `csp_guard`.

- [ ] **Step 5: Run the checks, then commit and push**

Commit `DV: the view address and its stricter policy`.

---

### Task 3: View on the document page

**Files:**
- Modify: `crates/pdms-web/src/routes/documents.rs` (`VersionRow`,
  `version_row`)
- Modify: `crates/pdms-web/templates/documents/show.html` (header actions
  and the Files table)
- Modify: `crates/pdms-web/static/app.css` only if two links in
  `td.file-actions` need spacing at 390 px (use existing tokens)
- Test: `crates/pdms-web/tests/documents.rs`

**Interfaces:**
- Consumes: `documents::viewable` (Task 1), the view address (Task 2).
- Produces: `VersionRow.viewable: bool`, set from
  `v.media_type.as_deref().is_some_and(documents::viewable)`.

- [ ] **Step 1: Write the failing tests**

`document_page_offers_view_for_viewable_files`:

- **A PDF document's page:**
  - contains `href="{doc}/versions/1/view"` twice, once in the header and
    once in the Files table;
  - both links have `target="_blank"` and `rel="noopener"`;
  - the page contains `(opens in a new tab)`;
  - `Download report.pdf` is still there;
  - with no review due, the View link has class `button primary`, and
    Download has class `button` without `primary`.
- **A `minutes.doc` document's page:** contains no `/view"`, and its
  Download keeps `button primary`.
- **After Edit details on the PDF document**, which makes version 2 with
  the file unchanged:
  - the header's View points at `{doc}/versions/2/view`, and that address
    serves the PDF's bytes inline;
  - the Files table still offers View only on version 1, the row where the
    file changed.

- [ ] **Step 2: Run it to make sure it fails**

Run: `cargo test -p pdms-web --test documents document_page_offers_view`
Expected: FAIL (no View link).

- [ ] **Step 3: Implement**

- **Header**, inside the existing `{% if let Some(f) = current_file %}`:
  - when `f.viewable`, a link before Download:
    `<a class="button{% if !start_review %} primary{% endif %}" href="/documents/{{ d.id }}/versions/{{ f.version_no }}/view" target="_blank" rel="noopener">`,
    containing `{% include "icons/eye.html" %}View`, then
    `<span class="visually-hidden"> {{ f.file_name }} (opens in a new tab)</span>`;
  - Download gets `primary` only when `!start_review && !f.viewable`.
- **Files table**, in the `file-actions` cell, before Download, when
  `v.has_file && v.file_changed && v.viewable`: the same link to
  `/documents/{{ d.id }}/versions/{{ v.version_no }}/view`, with the hidden
  text ` version {{ v.version_no }}, {{ v.file_name }} (opens in a new tab)`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p pdms-web` (including `csp_guard` and
`document_page_keeps_every_action`). Expected: PASS.

If Docker is available, also run the screenshot tool
(`tools/ui-screenshots/README.md`) on the document page, at 390 px and
1440 px. Expected: no overflow, and one `<h1>`.

- [ ] **Step 5: Run the checks, then commit and push**

Commit `DV: View on the document page`.

---

### Task 4: Documentation and the decision

**Files:**
- Modify: `docs/DECISIONS.md` (new D-118; "(amended by D-118)" after D-027's
  status)
- Modify: `docs/ADMIN_GUIDE.md`, `README.md`, `docs/ARCHITECTURE.md`
- Modify: `docs/ROADMAP.md` (§0.1a, §0.3, §0.6)

- [ ] **Step 1: D-118**, "Files are shown in the browser only for a short
list of types — Accepted (phase 1 built 2026-10-30)". Write it from spec
§2, §3 and §6:
  - the five types, decided by the stored type;
  - the view address;
  - the view policy and why each part is there (sandbox; inline styles
    only for the browser's own image page);
  - pages keep their policy, and only the view answer is marked;
  - no `Accept-Ranges`;
  - audited as `document.downloaded` with `disposition`.

  Say that phase 2 (Office files as PDF through SharePoint) waits for the
  owner.
- [ ] **Step 2: ADMIN_GUIDE.** Add a short section where downloads and
the audit log are described:
  - which files open in the browser;
  - that the viewer's own Save and Print aren't seen by PDMS, which is why
    a view is audited like a download;
  - how to tell views from downloads in the audit log (`disposition`).
- [ ] **Step 3: README** (features: View for PDFs and images) and
**ARCHITECTURE** (the view address and the two policies in
`middleware.rs`).
- [ ] **Step 4: ROADMAP:** §0.3 DV status ("Phase 1 built 2026-10-30; the
browser check on a county PC is next; phase 2 waits for the owner"), and
§0.1a.
- [ ] **Step 5: Commit and push** `DV: docs and D-118`.

---

### Task 5: The browser check on a county PC (the owner)

This needs real browsers, and only the owner deploys the test server
(CLAUDE.md). It can't be done by an agent. Use the test server after an
upgrade, or a local `pdms serve` reached from a county PC.

- [ ] **Step 1:** Sign in, then view a PDF with a link in it and a large
scanned JPEG (3000 px or wider) in **Edge**, **Chrome** and **Firefox**.
Check each of these:
  - the PDF shows;
  - the viewer's **Save** works;
  - its **Print** works;
  - the link in the PDF opens;
  - the JPEG fits the window and zooms on click;
  - the file opens in its new tab rather than an error page, both from the
    View button and when the view address is pasted into the address bar.
    (The view answer carries both `Cross-Origin-Opener-Policy: same-origin`
    and a CSP `sandbox`; the HTML Standard says a top-level page with both
    should not load. Current Chromium and Firefox load it anyway.)

  Also open one PDF and one JPEG on an iPhone or iPad. Every iOS browser
  uses Safari's engine, and spec §4 says Safari on iPhone shows them. With
  SharePoint storage, also view a large PDF (tens of MB). PDMS checks a
  SharePoint file in full before it shows it, so the new tab waits a
  moment; the file must still open rather than time out at the proxy.
- [ ] **Step 2:** If something is blocked, add only the allowance it needs
to `VIEW_CSP`: `allow-downloads` (Save), `allow-modals` (Print), or
`allow-popups` and `allow-popups-to-escape-sandbox` (links). Never add
`allow-scripts`, `allow-same-origin`, `allow-forms` or
`allow-top-navigation`. Update the test literal and D-118 in the same
commit, `DV: allow <what> in the view policy`. If the file does not open
in its tab, leave `Cross-Origin-Opener-Policy` off the view answer only, in
`security_headers`, and say so in D-118.
- [ ] **Step 3:** Record the result in the spec's §4 and in ROADMAP §0.3.
When it passes, DV phase 1 is done:
  - add it to §0.1;
  - remove its §0.1a row (or keep the row, if phase 2 is approved);
  - remove its column from §0.6.
