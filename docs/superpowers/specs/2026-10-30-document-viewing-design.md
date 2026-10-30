# Viewing documents in the browser: design

- **Date:** 2026-10-30
- **Status:** phase 1 (PDFs and images) approved by the owner on
  2026-10-30 and built the same day; the browser check on a county PC is
  next (plan Task 5). Phase 2 (§5) waits for the owner's answer to §10. Plan:
  [2026-10-30-document-viewing.md](../plans/2026-10-30-document-viewing.md).
- **Item:** "Viewing documents in the browser" (DV) in ROADMAP §0.3. Commit
  messages start with `DV:`.
- **Why:** today every file in PDMS is downloaded
  (`Content-Disposition: attachment`, D-027). To read a policy, a person
  downloads it, finds it in their Downloads folder, opens it, and is left
  with a copy on their PC. Most of the time they only wanted to read it.
  A **View** button that opens the file in a new browser tab saves those
  steps, and it leaves no copy behind unless they choose to save one.

## 1. What changes for a person using PDMS

1. On a document's page, a file that can be viewed (§2) gets a **View**
   button next to **Download**. View opens the file in a new browser tab,
   in the browser's own PDF viewer or image viewer.
2. The **Files** table has a **View** link beside each version's
   **Download** link, for the same types.
3. **Download** stays exactly as it is, for every file.
4. Files that can't be viewed show only **Download**, as today.
5. Who can view a file is exactly who can download it. Viewing is audited
   like a download (§6).

## 2. Which files can be viewed

**Phase 1** (no new parts): the types every supported browser shows safely
on its own.

| Type | Stored as | Shown by |
|---|---|---|
| PDF | `application/pdf` | the browser's PDF viewer |
| PNG, JPEG, GIF, WebP | `image/png`, `image/jpeg`, `image/gif`, `image/webp` | the browser's image page |

PDMS decides a file's type from its content when it is uploaded, never from
its name or the uploader's claim (D-027). The **View** button and the view
response go by that stored type, from one list in the code.

**Everything else stays download-only in phase 1**, for these reasons:

- **Word, Excel, PowerPoint, OpenDocument and RTF:** browsers can't show
  them. Phase 2 (§5) shows them as PDF.
- **TIFF:** only Safari shows TIFF. Phase 2 converts it too, which matters
  because scanned records are often TIFF.
- **Plain text and CSV:** a browser could show them, but a CSV is meant for
  Excel and a raw text page helps little (my call; easy to add later).
- **Outlook and email messages:** browsers can't show them, and a converted
  copy would leave out the attachments (my call).

**Why there is a list at all.** A file shown in the browser is opened as a
page on PDMS's own address. An HTML or SVG file shown that way could act as
PDMS for whoever opens it. PDMS stores nothing as HTML or SVG today (PDMS refuses files that start as
HTML; other text with markup in it, an SVG for example, is stored as plain
text), but the list keeps viewing safe even if
the accepted types change: a type is shown only if it is on the list.

## 3. How a file is sent for viewing

**Address:** `GET /documents/{id}/versions/{n}/view`. The **View** button in
the page header points at the current file's version.

- **Same check as a download:** the handler calls the same core function as
  the download (`documents::open_file`), so the `View` permission, the
  visibility rules (D-085), and the pause on a SharePoint file changed
  outside PDMS (D-105) apply unchanged.
- **A type not on the list** is sent exactly as a download is
  (`attachment`). Nothing reaches this address that way from PDMS's own
  pages, only from a typed or old link.

**Response headers for a viewable type:**

| Header | Value | Why |
|---|---|---|
| `Content-Type` | the stored type | decided from the content at upload (D-027) |
| `Content-Disposition` | `inline; filename="…"; filename*=UTF-8''…` | shown instead of saved. The viewer's own Save uses the real name. Same encoding as `content_disposition` today |
| `X-Content-Type-Options` | `nosniff` | as today: the browser must not guess another type |
| `Cache-Control` | `private, no-store` | as downloads |
| `Content-Security-Policy` | the **view policy** below | stricter than the pages' policy |
| `Accept-Ranges` | not sent | the viewer fetches the file once, so one view is one audit event |

The other headers the middleware adds (`X-Frame-Options: DENY`,
`Referrer-Policy`, `Cross-Origin-Opener-Policy`, HSTS) stay as they are.

**The view policy:**

```
default-src 'none'; style-src 'unsafe-inline'; base-uri 'none';
form-action 'none'; frame-ancestors 'none'; sandbox
```

- **`default-src 'none'`:** the response may load nothing else.
- **`sandbox` with no allowances:** the browser treats the tab as coming
  from nowhere, not from PDMS. Even if something in a file managed to run,
  it couldn't use the person's PDMS sign-in. Both browser viewers work
  under it (§4).
- **`style-src 'unsafe-inline'`:** Chromium's image page (Chrome and Edge)
  centres the image and fits a large scan to the window with inline
  styles. Under PDMS's page policy those are blocked: in the check, the
  image sat at the top left. Allowing them is safe here, because the
  response is an image or a PDF, never HTML, and `nosniff` stops the browser
  reading it as HTML.
- **Allowances the browser check may need (§4):** if a viewer's own Save,
  Print, or a link inside a PDF is blocked, add only the matching
  allowance: `allow-downloads`, `allow-modals`, `allow-popups` or
  `allow-popups-to-escape-sandbox`. Never add `allow-scripts`,
  `allow-same-origin`, `allow-forms` or `allow-top-navigation`.

**Only the view handler can choose the view policy.** Today
`middleware::security_headers` overwrites any policy a handler sets. The
view handler instead marks its response (a response extension), and the
middleware then sends the view policy in place of the page policy. Both
policies live side by side in `middleware.rs`. No other handler can send a
different policy, and a test checks that pages keep the page policy.

**Why a new tab and not a frame on the document page:** a frame would need
the page policy and `X-Frame-Options` loosened for PDMS's own pages. A new
tab needs neither, and it gives the viewer the whole screen. A frame can
come later if people ask for it.

## 4. The browser check (done 2026-10-30, with one part left)

ROADMAP §0.5 asked for this before the design was settled. A throwaway
script served a test PDF and PNG with `Content-Disposition: inline` under
four policies:

1. PDMS's page policy;
2. the page policy plus `sandbox`;
3. a strict policy;
4. the strict policy plus `sandbox`.

It ran in Microsoft's Playwright image, in Chromium (the engine of Chrome
and Edge, in its full headless mode) and in Firefox.

- **PDF:** Chromium's PDF viewer and Firefox's (PDF.js) showed the page
  under every policy, including both with `sandbox`.
- **PNG:** shown in both. Chromium's image page logged its inline styles as
  blocked under every policy without `'unsafe-inline'`, hence that allowance
  (§3).

**Left for the plan's last task (Task 5), once the view address is built,
on a county PC with the real browsers.**
Headless browsers can't show these, and Edge now has its own PDF viewer.
Check in Edge, Chrome and Firefox, with the built view policy:

- the PDF shows;
- the viewer's **Save** and **Print** work;
- a link inside a PDF opens;
- a large scanned JPEG fits the window.

If something is blocked, add only the allowance it needs (§3), and record
which one in the plan.

**Phones:** Safari on iPhone shows PDFs; Chrome on Android has no built-in
PDF viewer and downloads the file instead. That is fine: the audit records
it like any view.

## 5. Phase 2: Word, Excel and other files, shown as PDF

**This is the open question for the owner (§10).** The design is here so the
choice is concrete.

**When SharePoint storage is on**, Microsoft Graph can convert a file in the
site to PDF: `GET /drives/{drive}/items/{item}/content?format=pdf`. PDMS
asks for the conversion, follows Graph's redirect itself (as downloads do
today in `GraphClient::download`), and streams the PDF to the person with
the same headers as §3, as `application/pdf`, named after the original with
`.pdf`.

- **Types:** Word, Excel and PowerPoint (both the current and the 97–2003
  formats), OpenDocument text, spreadsheet and presentation, RTF, and TIFF.
- **The button says "View as PDF"**, because the result is Microsoft's
  conversion, not the original: fonts and page breaks can differ, and Excel
  shows each sheet's print layout.
- **D-082 still holds.** No SharePoint link reaches the browser; the
  conversion goes through PDMS's check and audit like every download.
- **The file is checked before it is converted.** A converted PDF can't be
  compared with the stored file's SHA-256 the way a download is. So PDMS
  first reads the item's size and QuickXorHash and compares them with what
  it recorded at upload (`sharepoint_files`). A difference is handled
  exactly as a download that finds a change (D-105): recorded, the file
  paused, administrators told once.
- **Not converted yet, or can't be:**
  - a file PDMS hasn't copied to SharePoint yet gets a page in the new tab
    saying so and offering **Download**;
  - a file Microsoft can't convert (Graph refuses, fails or times out) gets
    "Microsoft couldn't make a PDF of this file", also with **Download**.

  Both are ordinary PDMS pages, with one `<h1>`.
- **No copies are kept.** Each view converts again, which takes a few
  seconds for a large file. Keeping converted PDFs would mean storing and
  expiring a second copy of every file. That can be added if waiting turns
  out to be a problem.
- **With storage on the server's disk** (no SharePoint), these types stay
  download-only, and there is no "View as PDF" button.
- **Permission:** conversion should need nothing beyond the site grant PDMS
  already has (`Sites.Selected`, full control). Microsoft documents the
  conversion for the broad file permissions, so only the real tenant can
  confirm it. The plan's phase 2 starts with that check on the test server.

**Rejected:**

- **A LibreOffice converter beside PDMS, like the OCR sidecar:** it would
  work without SharePoint. But it is a large image, it lays out Office files
  less faithfully than Microsoft does, and SharePoint is where the files live
  (D-081).
- **SharePoint's or Office Online's own viewer:** it would hand people
  SharePoint links and need them to have SharePoint access, which D-082
  rules out.

## 6. Permissions and audit

- **No new permission.** Viewing needs `View` on the document, like a
  download, and nothing widens access.
- **A view is audited as `document.downloaded`**, the same event as a
  download, with one more context field. Opening a file in the browser hands
  the person the whole file, just as a download does (the viewer can save or
  print it), so audit searches and reports for downloads should include
  views. `document.viewed` already means opening the document's page
  (D-034), so it isn't reused.
  - context `disposition`: `inline` (viewed) or `attachment` (downloaded).
    Downloads get `attachment` from now on; older events have no
    `disposition`.
  - phase 2 adds context `converted_to`: `pdf`. `sha256` stays the stored
    file's hash.
  - the event is written when PDMS starts sending the file, as downloads are
    today.
- **The viewer's own Save and Print** happen in the browser and aren't
  seen by PDMS; the view event already records that the person had the file.

## 7. Tests

1. Each phase 1 type: the view answers 200 with the stored `Content-Type`,
   `Content-Disposition` starting `inline;`, `nosniff`,
   `private, no-store`, no `Accept-Ranges`, and exactly the view policy.
2. A Word file at the view address is sent as `attachment`, byte for byte
   like its download.
3. A plain-text file whose content is HTML (`<script>…`) is sent as
   `attachment`, never inline.
4. Someone without `View` gets the same answer as for the download; no file
   bytes and no `document.downloaded` event.
5. One view writes one `document.downloaded` with `disposition: inline`,
   the version, `sha256` and file name; a download now writes
   `disposition: attachment`.
6. An earlier version can be viewed, and the bytes are that version's.
7. A SharePoint file paused as changed outside PDMS is refused at the view
   address exactly as at the download address.
8. The document page shows **View** for a PDF and not for a Word file
   (phase 1). The link has `target="_blank"`, `rel="noopener"`, and an
   accessible name with the file name and "opens in a new tab". The Files
   table does the same per version.
9. Every other page still carries the page policy (a page, a download, a
   static file, an error page), so only the view handler can use the view
   policy.
10. `csp_guard` still passes.

**Phase 2**, with the mock Graph answering `content?format=pdf` with a
redirect to the converted bytes:

11. A Word file with SharePoint storage on: **View as PDF** shows;
    the view answers `application/pdf`, inline, named `….pdf`, with
    `converted_to: pdf` in the audit.
12. Size or hash differs in SharePoint: refused, recorded as
    `sharepoint.changed_outside`, and nothing converted.
13. Graph fails to convert: the "couldn't make a PDF" page, with a
    Download link.
14. The version is not in SharePoint yet: the "not copied yet" page.
15. With storage on disk: no **View as PDF** button, and the address sends
    the file as a download.

## 8. Documentation

- **README** (features) and **ARCHITECTURE** (the view address and the
  view policy).
- **ADMIN_GUIDE:**
  - which files can be viewed and that views are audited as downloads with
    `disposition`;
  - for phase 2: that "View as PDF" needs SharePoint storage, and what
    people see when a conversion fails.
- **DECISIONS: D-118**, "Files are shown in the browser only for a short
  list of types". It covers:
  - the list (§2);
  - the view policy and its sandbox (§3);
  - the audit as `document.downloaded` with `disposition` (§6);
  - phase 2, if approved: conversion through Graph, never a SharePoint link.

  It amends D-027 ("Downloads are always `attachment`"), which gets
  "(amended by D-118)".
- **ROADMAP:** §0.3, §0.5 and §0.6 as the work moves, and §0.1 when done.

## 9. Phases and size

1. **Phase 1:**
   - the view address and its headers;
   - the middleware's view policy;
   - **View** on the document page and in the Files table, with the
     existing `templates/icons/eye.html`;
   - the audit field;
   - the browser check on a county PC (§4);
   - tests 1 to 10, and the docs.

   Small. It touches `routes/documents.rs`, `middleware.rs`, the route table
   in `lib.rs`, `templates/documents/show.html`, `documents/service.rs`
   (`open_file`) in `pdms-core`, and web tests. No migration.
2. **Phase 2** (if approved):
   - the tenant check (§5);
   - the conversion in `pdms-core`'s `sharepoint/files.rs`, beside `open`;
   - **View as PDF**, the two message pages, and the mock Graph endpoint;
   - tests 11 to 15, and its docs.

   Small to medium. No migration.

Phase 1 is useful on its own and can be built and pushed before phase 2
starts.

## 10. The question for the owner, and the calls I made

**Question:** is phase 2 (Word, Excel, PowerPoint, OpenDocument, RTF and
TIFF shown as PDF through SharePoint's conversion, §5) part of DV, or left
for later? My recommendation is to include it, after phase 1: those formats
are most of what departments upload, and it needs no new server parts.
Building it doesn't depend on the live SharePoint test finishing. But
"View as PDF" only works where SharePoint storage is on, and §5's permission
check needs the test server.

**Calls I made** (each is cheap to change):

1. GIF and WebP join the roadmap's PDF, PNG and JPEG: they are images of the
   same kind, and every supported browser shows them.
2. Plain text, CSV and email stay download-only (§2).
3. A view is audited as `document.downloaded` with `disposition: inline`,
   not as a new event (§6).
4. **View** is the main button for viewable files, with **Download** beside
   it.
5. View opens a new tab, announced to screen readers as "opens in a new
   tab", not a frame on the page (§3).
6. No `Accept-Ranges`, so one view is one request and one audit event. A
   very large PDF loads fully before its first page shows.
7. The view policy is stricter than the pages' in every way except inline
   styles, which only the browser's own image page uses (§3).
8. A type that isn't on the list, reached at the view address, is sent as a
   download rather than refused.
9. The address doesn't carry the file name. File names would otherwise end
   up in proxy logs and browser history. The cost: Chrome's tab may read
   "view" for a PDF that has no title of its own.
10. The signatures page keeps its download link for signed copies; it can
    get **View** later.
11. No setting to turn viewing off.
12. Phase 2 keeps no converted copies (§5).
