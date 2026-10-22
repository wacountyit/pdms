# P3: SharePoint document storage. Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Pre-flight done (2026-10-22, WACountyIT).** This plan was written on 2026-10-15, before P2, R1, R2 and sub-project 3 were built. It was checked against `main` at `0ade802`. Most of it matches: P2's Graph client, `GraphError`, the mock Graph, the documents service and the jobs API are as described. These corrections apply; the text below already carries the numeric ones.
> - **Migration `0013`** (`0013_sharepoint_files.sql`). `main` ends at `0012`, and P2's is `0010`, not `0009`.
> - **New decisions start at D-102.** `main` ends at D-101.
> - **Task 4 (`tests/sharepoint.rs`):** put the two SPDX lines **first**, then the `#![allow(` block, as CLAUDE.md requires and as `tests/graph.rs` does. Reuse `mock_graph::certificate()` instead of defining another `certificate()`.
> - **Task 7 (`flag_changed`):** update `sharepoint_files.state` and write the `sharepoint.changed_outside` event **in one transaction** (`begin`, `UPDATE`, `pdms_audit::append`, `commit`), not with a separate `pdms_audit::record`. CLAUDE.md: "every change of state writes its audit event in the same database transaction".
> - **Task 7 Step 5(d) (integrity):** `files_checked` must not count the SharePoint files it skips. Subtract them, or report them separately as the step's output line already does.
> - **Task 9 Step 4 (`maintenance::worker()`):** since P2, the function no longer ends in one builder chain. It ends with `let worker = worker.handle(entra_sync…)…;` and then the expression `if core.config.entra_sync.enabled { worker.every(TICK_JOB…) } else { worker }`. Bind that `if` as `let worker = if … { … } else { worker };`, then end with `crate::sharepoint::register_jobs(worker, core)`.
> - **Task 6 / spec §7.1:** files are uploaded under a staging name and renamed by the mirror job. The spec says "upload to the final name". Add this to the deviations at the end, and record it as a decision.
> - **Task 11 (records):** also update the DECISIONS status lines for D-081, D-082, D-084 and D-085; ARCHITECTURE (migration table, the `sharepoint` and `filestore` modules); ADMIN_GUIDE (`[sharepoint]`, the 503 message, empty files); README; and ROADMAP §0.5 and §0.6.
> - **Test binaries:** `pub use sp::{…}` in `mock_graph/mod.rs` is compiled into the `graph.rs` and `entra_sync.rs` test binaries too. If `-D warnings` flags unused imports there, add `#[allow(unused_imports)]` to that line.
>
> **The owner's decisions (2026-10-22), recorded as D-102:**
> 1. **Long text is truncated in SharePoint.** Titles can be 500 characters, but single-line columns (and `Title`) hold 255, so `layout::columns()` caps every single-line text value at `TEXT_MAX` (255). PADM keeps the full value. *Built in Task 2.*
> 2. **A SharePoint outage keeps the person's form.** `CoreError::Unavailable` is handled like a validation error on the new-document and edit forms, and in `views::done` (a message on the page instead of a 503). *Built in Task 3*; Task 6's upload-failure test asserts the form comes back.
> 3. **Folder moves are audited**, with one action, `sharepoint.folder_moved` (category `system`, context: from and to path, and the reason: visibility, department, deleted or restored, or the mirror), written in the same transaction as the `sharepoint_document_folders` update. Applies in Tasks 9 and 10.
> 4. **No real data is stored yet.** Switching the backend makes files stored locally unreadable (routing follows `core.sharepoint`, not each blob's backend), so the test server is reset when SharePoint is turned on, as the spec's rollout says.
> 5. **Orphans are accepted for now, and noted.** These are staged uploads whose database transaction then fails, and a narrowing move made before an `update` that fails. They are left for the nightly mirror sweep and P4's change check. Listed under "Known gaps" in ROADMAP §4 in Task 11.
> 6. **Graph calls during `set_visibility`'s row lock are accepted**: they are rare and short.

**Goal:** Store every document file uploaded through PADM in one SharePoint library. Files are organised as `Departments/` / `County-wide/` / `Restricted/` → department → document folder, with one file per version. Downloads are streamed back through PADM and checked against the recorded SHA-256. PADM's metadata is mirrored into SharePoint columns, and folders move whenever a document's visibility, department, or deletion state changes.

**Architecture:**

- **`pdms-core::sharepoint`** (new) holds everything SharePoint-specific:
  - pure naming and layout rules (`layout`);
  - Microsoft's QuickXorHash (`quickxor`);
  - folder creation and renaming, tracked by Graph item ID (`folders`);
  - uploads and verified downloads (`files`);
  - the idempotent mirror job (`mirror`).
- **`pdms-core::documents::filestore`** (new) is the only place that decides where a document file goes: SharePoint, or `core.storage` (local/S3, for development and tests). Every read and write of a document file goes through it.
- **Uploads go to SharePoint synchronously, under a temporary name**, before the version's database transaction. The `sharepoint.mirror_document` job then gives each file its final name, sets its columns, and moves folders.
- **Moves that narrow who can browse a document happen before the change is saved, and refuse the change if SharePoint is down.** That covers visibility tightened, a department transfer, and a deletion.

**Tech Stack:** Rust 2024, SQLx 0.9 (`query!` macros, `.sqlx/` offline data), Tokio, Microsoft Graph v1.0 through P2's `GraphClient`, PostgreSQL 15/16.

**Spec:** [docs/superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md](../specs/2026-10-14-sharepoint-storage-entra-sync-design.md): §2 (D1–D9), §5, §6 `sharepoint_files`, §7.1–7.3, §7.7 (only what keeps `verify-integrity` working; P4 does the real SharePoint check), §8, §10, §12. **Index:** [2026-10-14-padm-sharepoint-entra-index.md](2026-10-14-padm-sharepoint-entra-index.md) ("Produced by P3"; this plan's deviations are listed at the end and Task 11 records them in the index). **Depends on:** P1 (`Visibility`, `set_visibility`, `NewDocument.visibility`, migration 0008) and P2 (`GraphClient`, `GraphError`, `Core.graph`, `[graph]` config, migration 0010) merged.

## Global Constraints

- **Library layout (spec §5):** one library. The top-level folders are exactly `Departments`, `County-wide`, `Restricted`, then the department folder (the PADM department name), then `{document ID} – {title}`, then one file per version named `{document ID} v{n} – {label}.{ext}`, where `{n}` is the PADM version number. Files are never overwritten (`@microsoft.graph.conflictBehavior` is always `fail`).
- **Version labels:** "Original", "Revision", "Amendment", "Signed copy", "Restored".
- **Visibility to area:** Department → `Departments`, County-wide → `County-wide`, Restricted → `Restricted`. Soft-deleted documents go to `Restricted` until restored.
- **Names:**
  - The characters `" * : < > ? / \ | # %` become `_`.
  - Control characters become spaces, and runs of whitespace collapse to one space.
  - `_vti_` becomes `_vti-`.
  - Leading `~`, `.` and spaces are trimmed, as are trailing `.` and spaces.
  - An empty result becomes `Untitled`.
  - Caps: department folder 60 characters, document folder 100, file name ≤ 120. The document ID is never cut. A full path stays ≤ 400 characters (SharePoint's limit).
- **Columns (spec §5):** the built-in `Title`, plus `PadmDocumentId`, `PadmDocumentType`, `PadmTags`, `PadmStatus`, `PadmVisibility`, `PadmDepartment`, `PadmOwner`, `PadmEffectiveDate`, `PadmReviewDate`, `PadmRenewalDate`, `PadmExpirationDate`, `PadmVersion`, `PadmCurrent`, `PadmLink`.
  - Display names: "Document ID", "Document Type", "Tags", "Status", "Visibility", "Department", "Owner", "Effective Date", "Review Date", "Renewal Date", "Expiration Date", "Version", "Current", "Open in PADM".
  - Dates are written as `YYYY-MM-DDT12:00:00Z`: noon UTC shows the same date in every US time zone.
  - "Open in PADM" is a **text** column, because Graph cannot write hyperlink columns.
- **Upload sessions (Graph):** every upload uses `createUploadSession` and is sent in chunks of `[sharepoint] upload_chunk_bytes`. That value must be a multiple of 327,680 bytes (320 KiB) and at most 62,914,560 (60 MiB); the default is 10,485,760. Chunks are PUT to `uploadUrl` without an `Authorization` header (P2's `put_bytes`).
- **Verification:**
  - Uploads: the size and QuickXorHash that SharePoint reports must equal PADM's. If not, the item is deleted and the upload fails.
  - Downloads: SHA-256 and size are checked while streaming. A mismatch ends the stream with an error, sets `sharepoint_files.state = 'changed_outside'`, and audits `sharepoint.changed_outside`.
- **User-facing failures:** use `CoreError::Unavailable` with exactly one of these messages:
  - "File storage (SharePoint) is unavailable right now, so nothing was saved. Try again in a few minutes." (upload)
  - "File storage (SharePoint) is unavailable right now. Try again in a few minutes." (download)
  - "File storage (SharePoint) is unavailable right now, so the change was not made. Try again in a few minutes." (narrowing move)
  - The web maps this variant to 503 "Temporarily unavailable", and the API to 503 `unavailable`.
- **Audit order (D-004):** `pdms_audit::append` stays the **last** statement before `tx.commit()`. Enqueue mirror jobs *before* it.
- **Empty files** cannot be stored in SharePoint mode. Reject them with "This file is empty. Empty files can't be stored.".
- **Migration:** `migrations/0013_sharepoint_files.sql`. Never edit 0001–0012. Migrations carry no SPDX header (D-080).
- **New Rust files** start with `// SPDX-License-Identifier: AGPL-3.0-or-later` and `// Copyright (C) 2026 PDMS contributors`. Integration-test files start with those two lines, then the `#![allow(` block (as `crates/pdms-core/tests/graph.rs` does).
- **Lints:** `unwrap`, `expect`, `panic`, and `dbg` are denied outside tests; `unsafe` is forbidden. Every task must pass `cargo clippy --workspace --all-targets --all-features -- -D warnings`, so introduce `pub(crate)` items in the task that first uses them.
- **Dev environment:** follow "Shared development setup" in the index. `DATABASE_URL` must point at the dev database with migrations through 0013 applied before building.
- **P2 names used here** (from `2026-10-14-p2-graph-client-entra-sync.md`):
  - **Types:** `pdms_core::graph::{GraphClient, GraphError, cert}`.
  - **`GraphError` variants** include unit `NotFound` and `Conflict`. `From<GraphError> for CoreError` maps `Conflict` → `CoreError::Conflict` and **everything else → `CoreError::Internal`**; a Graph `NotFound` is never a 404. User-facing SharePoint failures are therefore converted explicitly to `CoreError::Unavailable` in this plan.
  - **`GraphClient` methods:** `get`, `post`, `patch`, `delete`, `put_bytes`, `download`.
    - `put_bytes(absolute_url, Bytes, (first_byte, last_byte_inclusive, total))` returns `Ok(None)` for HTTP 202 and `Ok(Some(item_json))` when the upload completes. It sends no `Authorization` header.
    - `download(path)` follows Graph's 302 to the pre-authenticated URL without the token.
    - Links to other hosts are refused.
  - **`Core.graph: Option<GraphClient>`** is bound in `Core::new` as `let graph = …;`.
  - **`[graph]` (`GraphConfig`):** `tenant_id`, `client_id`, `certificate: Secret` (the PEM itself; in deployment `PDMS_GRAPH__CERTIFICATE_FILE`), `authority_url`, `api_url`, `timeout_seconds`, `max_retries`.
  - **Tests** reuse P2's mock Graph, `crates/pdms-core/tests/mock_graph/` (`MockGraph::start()`, `MockGraph::config(pem) -> GraphConfig`, `requests()`, with requests recorded as `"METHOD /v1.0/path?query"`). Test certificates come from `pdms_core::graph::cert::generate(name, days).pem`.

## Review Focus

Five inputs the spec implies but its happy path doesn't exercise. Each has a test in its owning task.

1. **Titles and department names SharePoint rejects:** quotes, `:`, `#`, `%`, `/`, `_vti_`, a leading `~`, trailing dots, or 500 characters. They must still produce valid, unique folder names with the document ID intact, and a path under 400 characters. Tests: Task 2 (`sanitize_*`, `paths_stay_under_the_sharepoint_limit`) and Task 6 (`awkward_titles_still_get_safe_folders`).
2. **SharePoint unreachable during an upload.** The user gets the "nothing was saved" message, and no document or version row is created. An empty file is refused with a clear message. Task 6 (`upload_failure_saves_nothing`, `empty_files_are_refused`).
3. **A file edited directly in SharePoint, then downloaded.** The download must not complete as if nothing happened: the stream ends with an error, the file is flagged, and the event is audited. Task 7 (`changed_files_stop_the_download_and_are_flagged`).
4. **Two departments whose names clean up to the same folder name, or a department renamed.** Each department gets its own folder (the second one is suffixed with its code), and a rename renames the folder in place without creating a new one. Task 5 (`departments_with_the_same_folder_name_get_their_own_folders`, `renaming_a_department_renames_its_folder`).
5. **Visibility tightened while SharePoint is down.** The change is refused with the "change was not made" message, PADM still shows the old visibility, and the folder hasn't moved. Task 10 (`tightening_is_refused_when_sharepoint_is_down`).

---

### Task 1: QuickXorHash

SharePoint reports `file.hashes.quickXorHash` for every file. PADM computes the same value while uploading, so it can confirm SharePoint stored exactly what was sent.

**Files:**
- Create: `crates/pdms-core/src/sharepoint/mod.rs`
- Create: `crates/pdms-core/src/sharepoint/quickxor.rs`
- Modify: `crates/pdms-core/src/lib.rs` (module list)

**Interfaces:**
- Consumes: nothing.
- Produces: `pdms_core::sharepoint::quickxor::QuickXorHash` with `new()`, `update(&mut self, &[u8])`, `finish(&self) -> [u8; 20]`, and `finish_base64(&self) -> String` (standard base64, as Graph reports it).

- [x] **Step 1: Create the module skeleton.** `crates/pdms-core/src/sharepoint/mod.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document files in a SharePoint library, through Microsoft Graph (D-081 to D-084).

pub mod quickxor;
```

In `crates/pdms-core/src/lib.rs`, add `pub mod sharepoint;` between `pub mod schedule;` and `pub mod signatures;`.

- [x] **Step 2: Write the failing tests.** Create `crates/pdms-core/src/sharepoint/quickxor.rs` with only the tests for now. The vectors come from rclone's QuickXorHash test suite (`backend/onedrive/quickxorhash/quickxorhash_test.go`). They were cross-checked against Microsoft's reference algorithm, including inputs longer than 160 bytes and every chunking used below.

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Microsoft's QuickXorHash: the content hash SharePoint and OneDrive report
//! for files (`file.hashes.quickXorHash`). Reference algorithm:
//! <https://learn.microsoft.com/onedrive/developer/code-snippets/quickxorhash>

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::QuickXorHash;

    /// (input, expected), both base64.
    const VECTORS: &[(&str, &str)] = &[
        ("", "AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        ("Sg==", "SgAAAAAAAAAAAAAAAQAAAAAAAAA="),
        ("tbQ=", "taAFAAAAAAAAAAAAAgAAAAAAAAA="),
        ("0pZP", "0rDEEwAAAAAAAAAAAwAAAAAAAAA="),
        ("/aNzzCFPS/A=", "/RjFHJgRgicsAR4ACAAAAAAAAAA="),
        ("T6LYJIfDh81JrAK309H2JMJTXis=", "zBTHrspn3mEcohlJdIUAbjGNaNg="),
        ("DWAAX5/CIfrmErgZa8ot6ZraeSbu", "LR2Z0PjuRYGKQB/mhQAuMrAGZbQ="),
        (
            "Mb7EGva2rEE5fENDL85P+BsapHEEjv2/siVhKjvAQe02feExVOQSkfmuYzU/kTF1MaKjPmKF/w+cbvwfdWL8aQ==",
            "n1anP5NfvD4XDYWIeRPW3ZkPv1Y=",
        ),
        (
            "PfxMcUd0vIW6VbHG/uj/Y0W6qEoKmyBD0nYebEKazKaKG+UaDqBEcmQjbfQeVnVLuodMoPp7P7TR1htX5n2VnkHh22xDyoJ8C/ZQKiSNqQfXvh83judf4RVr9exJCud8Uvgip6aVZTaPrJHVjQhMCp/dEnGvqg0oN5OVkM2qqAXvA0teKUDhgNM71sDBVBCGXxNOR2bpbD1iM4dnuT0ey4L+loXEHTL0fqMeUcEi2asgImnlNakwenDzz0x57aBwyq3AspCFGB1ncX4yYCr/OaCcS5OKi/00WH+wNQU3",
            "QX/YEpG0gDsmhEpCdWhsxDzsfVE=",
        ),
        (
            "w0LGhqU1WXFbdavqDE4kAjEzWLGGzmTNikzqnsiXHx2KRReKVTxkv27u3UcEz9+lbMvYl4xFf2Z4aE1xRBBNd1Ke5C0zToSaYw5o4B/7X99nKK2/XaUX1byLow2aju2XJl2OpKpJg+tSJ2fmjIJTkfuYUz574dFX6/VXxSxwGH/xQEAKS5TCsBK3CwnuG1p5SAsQq3gGVozDWyjEBcWDMdy8/AIFrj/y03Lfc/RNRCQTAfZbnf2QwV7sluw4fH3XJr07UoD0YqN+7XZzidtrwqMY26fpLZnyZjnBEt1FAZWO7RnKG5asg8xRk9YaDdedXdQSJAOy6bWEWlABj+tVAigBxavaluUH8LOj+yfCFldJjNLdi90fVHkUD/m4Mr5OtmupNMXPwuG3EQlqWUVpQoYpUYKLsk7a5Mvg6UFkiH596y5IbJEVCI1Kb3D1",
            "e3+wo77iKcILiZegnzyUNcjCdoQ=",
        ),
    ];

    fn decode(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    #[test]
    fn matches_published_vectors() {
        for (input, expected) in VECTORS {
            let mut h = QuickXorHash::new();
            h.update(&decode(input));
            assert_eq!(&h.finish_base64(), expected, "input {input}");
        }
    }

    #[test]
    fn chunking_does_not_change_the_hash() {
        for (input, expected) in VECTORS {
            let data = decode(input);
            for size in [1, 7, 160, 161, 1000] {
                let mut h = QuickXorHash::new();
                for chunk in data.chunks(size) {
                    h.update(chunk);
                }
                assert_eq!(&h.finish_base64(), expected, "input {input}, chunks of {size}");
            }
        }
    }

    #[test]
    fn length_is_folded_into_the_last_eight_bytes() {
        let mut h = QuickXorHash::new();
        h.update(&[0u8; 3]);
        let out = h.finish();
        assert_eq!(out[12], 3);
        assert!(out[..12].iter().all(|b| *b == 0));
    }
}
```

- [x] **Step 3: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --lib sharepoint::quickxor`
Expected: compile error `cannot find type QuickXorHash in this scope` (or `unresolved import super::QuickXorHash`).

- [x] **Step 4: Implement.** Insert above the `#[cfg(test)]` module:

```rust
use base64::Engine as _;

const WIDTH: u64 = 160;
const SHIFT: u64 = 11;

/// Incremental QuickXorHash. Feed bytes with [`update`](Self::update) in any
/// chunking; [`finish_base64`](Self::finish_base64) returns the value Graph reports.
#[derive(Clone, Debug, Default)]
pub struct QuickXorHash {
    /// The 160-bit register: bits 0–63, 64–127, and 128–159 (low 32 bits of the last cell).
    cells: [u64; 3],
    length: u64,
}

impl QuickXorHash {
    pub fn new() -> Self {
        Self::default()
    }

    /// Byte `k` of the input is XORed into the register at bit `(k * 11) mod 160`.
    /// Bytes 160 apart land on the same bit, so they are combined first.
    pub fn update(&mut self, data: &[u8]) {
        let start = (self.length % WIDTH) * SHIFT % WIDTH;
        for i in 0..data.len().min(WIDTH as usize) {
            let x = data.iter().skip(i).step_by(WIDTH as usize).fold(0u8, |a, b| a ^ b);
            self.place(x, (start + SHIFT * i as u64) % WIDTH);
        }
        self.length += data.len() as u64;
    }

    fn place(&mut self, x: u8, at: u64) {
        for bit in 0..8u64 {
            if (x >> bit) & 1 == 1 {
                let p = (at + bit) % WIDTH;
                if let Some(cell) = self.cells.get_mut((p / 64) as usize) {
                    *cell ^= 1u64 << (p % 64);
                }
            }
        }
    }

    /// The 20-byte digest: the register in little-endian order, with the
    /// input length (little-endian u64) XORed into the last 8 bytes.
    pub fn finish(&self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[..8].copy_from_slice(&self.cells[0].to_le_bytes());
        out[8..16].copy_from_slice(&self.cells[1].to_le_bytes());
        out[16..].copy_from_slice(&self.cells[2].to_le_bytes()[..4]);
        for (o, l) in out[12..].iter_mut().zip(self.length.to_le_bytes()) {
            *o ^= l;
        }
        out
    }

    pub fn finish_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.finish())
    }
}
```

- [x] **Step 5: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --lib sharepoint::quickxor`
Expected: `test result: ok. 3 passed`.

- [x] **Step 6: Commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core/src/lib.rs crates/pdms-core/src/sharepoint
git commit -m "sharepoint: QuickXorHash for verifying uploads"
```

---

### Task 2: Layout and naming rules

These are pure functions with no input or output, so every naming rule is unit-tested here.

**Files:**
- Create: `crates/pdms-core/src/sharepoint/layout.rs`
- Modify: `crates/pdms-core/src/sharepoint/mod.rs` (add `pub mod layout;`)

**Interfaces:**
- Consumes: `pdms_authz::Visibility` (P1); `crate::documents::VersionKind`.
- Produces (all `pub` in `pdms_core::sharepoint::layout`):
  - `enum Area { Departments, CountyWide, Restricted }`, with `ALL`, `folder_name()` and `as_str()` (`departments`, `county_wide`, `restricted`);
  - `area_for(Visibility) -> Area`;
  - `struct Spot { area: Area, department_id: Uuid }` and `Spot::of(Visibility, Uuid, deleted: bool) -> Spot`;
  - `narrows(from: Spot, to: Spot) -> bool`;
  - `sanitize_segment(&str, max_chars: usize) -> String`;
  - `department_folder_name(&str) -> String` and `department_folder_name_with_code(name: &str, code: &str) -> String`;
  - `document_folder_name(external_id, title) -> String`;
  - `file_name(external_id, version_no: i32, label, ext) -> String`;
  - `staging_name(external_id, token, ext) -> String`;
  - `version_label(VersionKind) -> &'static str`;
  - `extension_for(media_type, file_name) -> String`;
  - `struct PlacementDoc<'a>`, `struct PlacementFile { version_no: i32, current: bool }`;
  - `columns(&PlacementDoc, &PlacementFile) -> serde_json::Map<String, Value>` and `column_definitions() -> Vec<Value>`;
  - column-name constants;
  - `parse_site_url(&str) -> Result<(String, String), String>` (host, `/sites/<name>`);
  - `encode_segment(&str) -> String`;
  - the constants `DEPARTMENT_MAX`, `FOLDER_MAX`, `FILE_MAX`, `ID_MAX`, `LABEL_MAX`, `PATH_LIMIT`.

- [x] **Step 1: Write the failing tests.** Add `pub mod layout;` to `sharepoint/mod.rs` (above `pub mod quickxor;`). Create `crates/pdms-core/src/sharepoint/layout.rs` with the header, a doc comment, and this test module:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Where a document's files go in SharePoint and what they are called (spec §5).
//! Pure functions, unit-tested here.

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pdms_authz::Visibility;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::documents::VersionKind;

    const A: Uuid = Uuid::from_u128(1);
    const B: Uuid = Uuid::from_u128(2);

    #[test]
    fn visibility_picks_the_area_and_deleted_documents_leave_view() {
        assert_eq!(area_for(Visibility::Department), Area::Departments);
        assert_eq!(area_for(Visibility::CountyWide), Area::CountyWide);
        assert_eq!(area_for(Visibility::Restricted), Area::Restricted);
        assert_eq!(Spot::of(Visibility::CountyWide, A, true).area, Area::Restricted);
        assert_eq!(Area::ALL.map(Area::folder_name), ["Departments", "County-wide", "Restricted"]);
    }

    #[test]
    fn narrowing_is_any_move_that_takes_the_document_from_someone() {
        let s = |area, d| Spot { area, department_id: d };
        use Area::*;
        assert!(narrows(s(Departments, A), s(Restricted, A)));
        assert!(narrows(s(CountyWide, A), s(Restricted, A)));
        assert!(narrows(s(CountyWide, A), s(Departments, A)));
        assert!(narrows(s(Departments, A), s(Departments, B)));
        assert!(!narrows(s(Departments, A), s(CountyWide, A)));
        assert!(!narrows(s(Restricted, A), s(Departments, A)));
        assert!(!narrows(s(Restricted, A), s(Restricted, B)));
        assert!(!narrows(s(CountyWide, A), s(CountyWide, B)));
        assert!(!narrows(s(Departments, A), s(Departments, A)));
    }

    #[test]
    fn sanitize_replaces_forbidden_characters_and_trims() {
        assert_eq!(
            sanitize_segment("Q3 \"Final\" Agreement: Road #5 / Bridge 50% *draft* <a|b>?\\", 200),
            "Q3 _Final_ Agreement_ Road _5 _ Bridge 50_ _draft_ _a_b___"
        );
        assert_eq!(sanitize_segment("~~ .hidden. ", 50), "hidden");
        assert_eq!(sanitize_segment("tab\there\nnewline", 50), "tab here newline");
        assert_eq!(sanitize_segment("my_vti_file and _VTI_x", 50), "my_vti-file and _VTI-x");
        assert_eq!(sanitize_segment(":::", 50), "___");
        assert_eq!(sanitize_segment("...", 50), "Untitled");
        assert_eq!(sanitize_segment("", 50), "Untitled");
    }

    #[test]
    fn sanitize_cuts_by_characters_not_bytes() {
        let s = sanitize_segment(&"é".repeat(300), 10);
        assert_eq!(s.chars().count(), 10);
        assert_eq!(sanitize_segment("abc def", 4), "abc");
    }

    #[test]
    fn names_keep_the_document_id_intact() {
        let long = "x".repeat(500);
        let folder = document_folder_name("CON-2026-000012", &long);
        assert!(folder.starts_with("CON-2026-000012 – x"));
        assert!(folder.chars().count() <= FOLDER_MAX);
        assert_eq!(
            document_folder_name("CON-2026-000012", "Fuel Supply Agreement"),
            "CON-2026-000012 – Fuel Supply Agreement"
        );
        assert_eq!(
            file_name("CON-2026-000012", 3, version_label(VersionKind::SignedCopy), "pdf"),
            "CON-2026-000012 v3 – Signed copy.pdf"
        );
        assert_eq!(
            staging_name("CON-2026-000012", "1a2b3c4d", "pdf"),
            "CON-2026-000012 (uploading 1a2b3c4d).pdf"
        );
        assert_eq!(department_folder_name("Road & Bridge"), "Road & Bridge");
        assert_eq!(
            department_folder_name_with_code("Road:Bridge", "RB2"),
            "Road_Bridge (RB2)"
        );
        assert!(department_folder_name_with_code(&"d".repeat(200), "CODE").chars().count() <= DEPARTMENT_MAX);
    }

    #[test]
    fn paths_stay_under_the_sharepoint_limit() {
        // Worst case: an 80-character site path, the library, the longest area,
        // and every segment at its cap.
        let site_and_library = format!("/sites/{}/Shared Documents", "s".repeat(60));
        let dept = department_folder_name(&"d".repeat(500));
        let folder = document_folder_name(&"I".repeat(80), &"t".repeat(500));
        let file = file_name(&"I".repeat(80), i32::MAX, &"l".repeat(200), "docx");
        let path = format!("{site_and_library}/County-wide/{dept}/{folder}/{file}");
        assert!(file.chars().count() <= FILE_MAX, "{}", file.chars().count());
        assert!(path.chars().count() <= PATH_LIMIT, "{}", path.chars().count());
    }

    #[test]
    fn labels_and_extensions() {
        assert_eq!(version_label(VersionKind::Initial), "Original");
        assert_eq!(version_label(VersionKind::Restore), "Restored");
        assert_eq!(extension_for("application/pdf", "whatever.bin"), "pdf");
        assert_eq!(
            extension_for("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "x"),
            "docx"
        );
        // The sniffed type wins over the client's name (D-027).
        assert_eq!(extension_for("text/plain", "payload.bat"), "txt");
        assert_eq!(extension_for("application/unknown", "Report.ODF"), "odf");
        assert_eq!(extension_for("application/unknown", "no-extension"), "bin");
        assert_eq!(extension_for("application/unknown", "weird.ex e"), "bin");
    }

    #[test]
    fn columns_carry_padm_metadata() {
        let tags = vec!["Roads".to_owned(), "FY2026".to_owned()];
        let doc = PlacementDoc {
            external_id: "CON-2026-000012",
            title: "Fuel Supply Agreement",
            type_name: "Contract",
            tags: &tags,
            status: "Active",
            visibility: Visibility::CountyWide,
            department: "Road and Bridge",
            owner: "Alice Ops",
            effective_date: NaiveDate::from_ymd_opt(2026, 1, 15),
            review_date: None,
            renewal_date: NaiveDate::from_ymd_opt(2027, 1, 14),
            expiration_date: None,
            link: "https://padm.example.org/documents/abc",
        };
        let m = columns(&doc, &PlacementFile { version_no: 4, current: true });
        assert_eq!(m["Title"], json!("Fuel Supply Agreement"));
        assert_eq!(m[DOCUMENT_ID], json!("CON-2026-000012"));
        assert_eq!(m[TAGS], json!("Roads; FY2026"));
        assert_eq!(m[VISIBILITY], json!("County-wide"));
        assert_eq!(m[EFFECTIVE_DATE], json!("2026-01-15T12:00:00Z"));
        assert_eq!(m[REVIEW_DATE], json!(null));
        assert_eq!(m[VERSION], json!(4));
        assert_eq!(m[CURRENT], json!(true));
        assert_eq!(m[LINK], json!("https://padm.example.org/documents/abc"));
        // Every column written (except the built-in Title) is provisioned, once.
        let defined: Vec<String> = column_definitions()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_owned())
            .collect();
        for key in m.keys().filter(|k| *k != "Title") {
            assert_eq!(defined.iter().filter(|d| *d == key).count(), 1, "{key}");
        }
    }

    #[test]
    fn site_urls() {
        assert_eq!(
            parse_site_url("https://contoso.sharepoint.com/sites/PADMRecords/").unwrap(),
            ("contoso.sharepoint.com".to_owned(), "/sites/PADMRecords".to_owned())
        );
        assert!(parse_site_url("https://contoso.sharepoint.com/teams/PADM").is_ok());
        assert!(parse_site_url("http://contoso.sharepoint.com/sites/PADM").is_err());
        assert!(parse_site_url("https://contoso.sharepoint.com/").is_err());
        assert!(parse_site_url("https://contoso.sharepoint.com/sites/PADM/Shared%20Documents").is_err());
        assert!(parse_site_url("not a url").is_err());
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_segment("Road and Bridge"), "Road%20and%20Bridge");
        assert_eq!(encode_segment("a#b%c"), "a%23b%25c");
        assert_eq!(encode_segment("CON-1 – x"), "CON-1%20%E2%80%93%20x");
        assert_eq!(encode_segment("safe-._~"), "safe-._~");
    }
}
```

- [x] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --lib sharepoint::layout`
Expected: compile errors such as `cannot find function area_for in this scope`.

- [x] **Step 3: Implement.** Insert above the test module in `layout.rs`:

```rust
use chrono::NaiveDate;
use pdms_authz::Visibility;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::documents::VersionKind;

/// SharePoint Online's limit on a decoded path, in characters.
pub const PATH_LIMIT: usize = 400;
pub const DEPARTMENT_MAX: usize = 60;
pub const FOLDER_MAX: usize = 100;
pub const FILE_MAX: usize = 120;
/// Longest document ID kept in a name (IDs are like `CON-2026-000012`).
pub const ID_MAX: usize = 40;
pub const LABEL_MAX: usize = 40;

/// The three top-level folders (D-084).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Area {
    Departments,
    CountyWide,
    Restricted,
}

impl Area {
    pub const ALL: [Area; 3] = [Area::Departments, Area::CountyWide, Area::Restricted];

    /// The folder's name in the library.
    pub fn folder_name(self) -> &'static str {
        match self {
            Area::Departments => "Departments",
            Area::CountyWide => "County-wide",
            Area::Restricted => "Restricted",
        }
    }

    /// Stored in `sharepoint_department_folders.area`.
    pub fn as_str(self) -> &'static str {
        match self {
            Area::Departments => "departments",
            Area::CountyWide => "county_wide",
            Area::Restricted => "restricted",
        }
    }
}

pub fn area_for(v: Visibility) -> Area {
    match v {
        Visibility::Department => Area::Departments,
        Visibility::CountyWide => Area::CountyWide,
        Visibility::Restricted => Area::Restricted,
    }
}

/// Where a document's folder belongs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    pub area: Area,
    pub department_id: Uuid,
}

impl Spot {
    /// Soft-deleted documents leave everyone's view: they wait in `Restricted`.
    pub fn of(visibility: Visibility, department_id: Uuid, deleted: bool) -> Spot {
        let area = if deleted {
            Area::Restricted
        } else {
            area_for(visibility)
        };
        Spot { area, department_id }
    }
}

/// Whether moving a folder from `from` to `to` takes it away from someone who
/// can browse it now. Such moves happen before the change is saved (spec §7.3).
pub fn narrows(from: Spot, to: Spot) -> bool {
    if from == to {
        return false;
    }
    match (from.area, to.area) {
        (Area::Restricted, _) => false,
        (_, Area::Restricted) => true,
        (Area::CountyWide, Area::Departments) => true,
        (Area::Departments, Area::Departments) => from.department_id != to.department_id,
        _ => false,
    }
}

/// Make `s` safe as one SharePoint file or folder name, at most `max_chars`.
pub fn sanitize_segment(s: &str, max_chars: usize) -> String {
    let replaced: String = s
        .chars()
        .map(|c| match c {
            '"' | '*' | ':' | '<' | '>' | '?' | '/' | '\\' | '|' | '#' | '%' => '_',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let mut out = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    // SharePoint rejects `_vti_` anywhere in a name.
    while let Some(i) = out.to_ascii_lowercase().find("_vti_") {
        let keep = out[i..i + 4].to_owned();
        out.replace_range(i..i + 5, &format!("{keep}-"));
    }
    let cut: String = out.chars().take(max_chars).collect();
    let trimmed = cut
        .trim_start_matches(['~', ' ', '.'])
        .trim_end_matches([' ', '.']);
    if trimmed.is_empty() {
        "Untitled".to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub fn department_folder_name(name: &str) -> String {
    sanitize_segment(name, DEPARTMENT_MAX)
}

/// Used when another department already has the plain name.
pub fn department_folder_name_with_code(name: &str, code: &str) -> String {
    let code = sanitize_segment(code, 32);
    let room = DEPARTMENT_MAX.saturating_sub(code.chars().count() + 3).max(1);
    format!("{} ({code})", sanitize_segment(name, room))
}

pub fn document_folder_name(external_id: &str, title: &str) -> String {
    let id = sanitize_segment(external_id, ID_MAX);
    let room = FOLDER_MAX - id.chars().count() - 3;
    format!("{id} – {}", sanitize_segment(title, room))
}

pub fn file_name(external_id: &str, version_no: i32, label: &str, ext: &str) -> String {
    format!(
        "{} v{version_no} – {}.{ext}",
        sanitize_segment(external_id, ID_MAX),
        sanitize_segment(label, LABEL_MAX)
    )
}

/// The name a file carries between upload and the mirror job (spec §7.1).
pub fn staging_name(external_id: &str, token: &str, ext: &str) -> String {
    format!(
        "{} (uploading {token}).{ext}",
        sanitize_segment(external_id, ID_MAX)
    )
}

pub fn version_label(kind: VersionKind) -> &'static str {
    match kind {
        VersionKind::Initial => "Original",
        VersionKind::Revision => "Revision",
        VersionKind::Amendment => "Amendment",
        VersionKind::SignedCopy => "Signed copy",
        VersionKind::Restore => "Restored",
        VersionKind::Metadata => "File",
    }
}

/// The extension a stored file gets: from the type sniffed from its content
/// (D-027), so SharePoint never shows a script name for a text file.
pub fn extension_for(media_type: &str, file_name: &str) -> String {
    let known = match media_type {
        "application/pdf" => "pdf",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/msword" => "doc",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.ms-outlook" => "msg",
        "application/vnd.oasis.opendocument.text" => "odt",
        "application/vnd.oasis.opendocument.spreadsheet" => "ods",
        "application/vnd.oasis.opendocument.presentation" => "odp",
        "application/rtf" => "rtf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/tiff" => "tif",
        "image/webp" => "webp",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "message/rfc822" => "eml",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".to_owned())
}

// Column internal names (spec §5). `Title` is the library's built-in column.
pub const DOCUMENT_ID: &str = "PadmDocumentId";
pub const DOCUMENT_TYPE: &str = "PadmDocumentType";
pub const TAGS: &str = "PadmTags";
pub const STATUS: &str = "PadmStatus";
pub const VISIBILITY: &str = "PadmVisibility";
pub const DEPARTMENT: &str = "PadmDepartment";
pub const OWNER: &str = "PadmOwner";
pub const EFFECTIVE_DATE: &str = "PadmEffectiveDate";
pub const REVIEW_DATE: &str = "PadmReviewDate";
pub const RENEWAL_DATE: &str = "PadmRenewalDate";
pub const EXPIRATION_DATE: &str = "PadmExpirationDate";
pub const VERSION: &str = "PadmVersion";
pub const CURRENT: &str = "PadmCurrent";
pub const LINK: &str = "PadmLink";

/// What PADM shows about a document, for the SharePoint columns.
pub struct PlacementDoc<'a> {
    pub external_id: &'a str,
    pub title: &'a str,
    pub type_name: &'a str,
    pub tags: &'a [String],
    pub status: &'a str,
    pub visibility: Visibility,
    pub department: &'a str,
    pub owner: &'a str,
    pub effective_date: Option<NaiveDate>,
    pub review_date: Option<NaiveDate>,
    pub renewal_date: Option<NaiveDate>,
    pub expiration_date: Option<NaiveDate>,
    pub link: &'a str,
}

pub struct PlacementFile {
    pub version_no: i32,
    /// The newest file of the document.
    pub current: bool,
}

/// Column values for one file. Dates are noon UTC, so every US time zone
/// shows the same day.
pub fn columns(doc: &PlacementDoc<'_>, file: &PlacementFile) -> Map<String, Value> {
    let date = |d: Option<NaiveDate>| d.map_or(Value::Null, |d| Value::String(format!("{d}T12:00:00Z")));
    let mut m = Map::new();
    m.insert("Title".into(), json!(doc.title));
    m.insert(DOCUMENT_ID.into(), json!(doc.external_id));
    m.insert(DOCUMENT_TYPE.into(), json!(doc.type_name));
    m.insert(TAGS.into(), json!(doc.tags.join("; ")));
    m.insert(STATUS.into(), json!(doc.status));
    m.insert(VISIBILITY.into(), json!(doc.visibility.label()));
    m.insert(DEPARTMENT.into(), json!(doc.department));
    m.insert(OWNER.into(), json!(doc.owner));
    m.insert(EFFECTIVE_DATE.into(), date(doc.effective_date));
    m.insert(REVIEW_DATE.into(), date(doc.review_date));
    m.insert(RENEWAL_DATE.into(), date(doc.renewal_date));
    m.insert(EXPIRATION_DATE.into(), date(doc.expiration_date));
    m.insert(VERSION.into(), json!(file.version_no));
    m.insert(CURRENT.into(), json!(file.current));
    m.insert(LINK.into(), json!(doc.link));
    m
}

/// Library columns PADM creates when they are missing
/// (`POST /sites/{site}/lists/{list}/columns`).
pub fn column_definitions() -> Vec<Value> {
    let text = |name: &str, display: &str| {
        json!({"name": name, "displayName": display, "text": {"allowMultipleLines": false, "maxLength": 255}})
    };
    let date = |name: &str, display: &str, indexed: bool| {
        json!({"name": name, "displayName": display, "indexed": indexed, "dateTime": {"format": "dateOnly"}})
    };
    vec![
        text(DOCUMENT_ID, "Document ID"),
        text(DOCUMENT_TYPE, "Document Type"),
        json!({"name": TAGS, "displayName": "Tags", "text": {"allowMultipleLines": true}}),
        text(STATUS, "Status"),
        json!({"name": VISIBILITY, "displayName": "Visibility",
               "choice": {"choices": ["Department", "County-wide", "Restricted"], "displayAs": "dropDownMenu"}}),
        text(DEPARTMENT, "Department"),
        text(OWNER, "Owner"),
        date(EFFECTIVE_DATE, "Effective Date", false),
        date(REVIEW_DATE, "Review Date", true),
        date(RENEWAL_DATE, "Renewal Date", false),
        date(EXPIRATION_DATE, "Expiration Date", false),
        json!({"name": VERSION, "displayName": "Version", "number": {}}),
        json!({"name": CURRENT, "displayName": "Current", "indexed": true, "boolean": {}}),
        text(LINK, "Open in PADM"),
    ]
}

/// `https://<tenant>.sharepoint.com/sites/<Name>` → (host, `/sites/<Name>`).
pub fn parse_site_url(s: &str) -> Result<(String, String), String> {
    let url = url::Url::parse(s.trim()).map_err(|_| {
        "enter the site's full address, e.g. https://contoso.sharepoint.com/sites/PADMRecords"
            .to_owned()
    })?;
    if url.scheme() != "https" {
        return Err("the site address must start with https://".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "the site address has no host".to_owned())?
        .to_owned();
    let segs: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    match segs.as_slice() {
        [kind @ ("sites" | "teams"), name] => Ok((host, format!("/{kind}/{name}"))),
        _ => Err("the site address must look like https://<tenant>.sharepoint.com/sites/<SiteName>".into()),
    }
}

/// Percent-encode one path segment for Graph path addressing (RFC 3986:
/// everything except unreserved characters).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
```

> Note on `sanitize_segment`: the replacement keeps the original letter case of `_vti` (`_VTI_x` becomes `_VTI-x`). `to_ascii_lowercase` changes no byte offsets, so `i..i + 5` is always on character boundaries.

- [x] **Step 4: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --lib sharepoint::layout`
Expected: `test result: ok. 11 passed` (10, plus `long_text_is_cut_to_sharepoints_limit` for the owner's decision 1).

- [x] **Step 5: Commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core/src/sharepoint
git commit -m "sharepoint: layout, naming, and column rules"
```

---

### Task 3: Configuration and the "unavailable" error

**Files:**
- Modify: `crates/pdms-core/src/config.rs` (`StorageBackend`, new `SharePointConfig`, `Config.sharepoint`, `validate`)
- Modify: `crates/pdms-core/src/error.rs` (`CoreError::Unavailable`)
- Modify: `crates/pdms-web/src/error.rs` (503 page)
- Modify: `crates/pdms-web/src/routes/api.rs` (503 JSON)

**Interfaces:**
- Consumes: `layout::parse_site_url` (Task 2).
- Produces:
  - `StorageBackend::SharePoint` (TOML `"sharepoint"`);
  - `pdms_core::config::SharePointConfig { site_url: String, library: String, delta_interval_minutes: u64, upload_chunk_bytes: u64 }`;
  - `Config.sharepoint`;
  - `CoreError::Unavailable(String)`.

- [x] **Step 1: Write the failing tests.** Append to `crates/pdms-core/src/config.rs`:

```rust
#[cfg(test)]
mod sharepoint_config_tests {
    use super::*;

    fn sharepoint_config() -> Config {
        let mut c = Config::default();
        c.storage.backend = StorageBackend::SharePoint;
        c.sharepoint.site_url = "https://contoso.sharepoint.com/sites/PADMRecords".into();
        c
    }

    #[test]
    fn defaults_are_valid_and_chunks_follow_graph_rules() {
        let c = sharepoint_config();
        assert_eq!(c.sharepoint.library, "Documents");
        assert_eq!(c.sharepoint.upload_chunk_bytes, 10_485_760);
        assert_eq!(c.sharepoint.upload_chunk_bytes % 327_680, 0);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn bad_sharepoint_settings_are_rejected() {
        let mut c = sharepoint_config();
        c.sharepoint.upload_chunk_bytes = 1_000_000;
        assert!(c.validate().is_err(), "not a multiple of 320 KiB");
        let mut c = sharepoint_config();
        c.sharepoint.upload_chunk_bytes = 327_680 * 193;
        assert!(c.validate().is_err(), "over 60 MiB");
        let mut c = sharepoint_config();
        c.sharepoint.site_url = "http://contoso.sharepoint.com/sites/PADM".into();
        assert!(c.validate().is_err());
        let mut c = sharepoint_config();
        c.sharepoint.library = " ".into();
        assert!(c.validate().is_err());
        // Ignored unless SharePoint is the backend.
        let mut c = Config::default();
        c.sharepoint.site_url = "nonsense".into();
        assert!(c.validate().is_ok());
    }

    #[test]
    fn backend_name_in_toml_is_sharepoint() {
        let b: StorageBackend = serde_json::from_str("\"sharepoint\"").unwrap();
        assert_eq!(b, StorageBackend::SharePoint);
    }
}
```

Append to `crates/pdms-web/src/error.rs`:

```rust
#[cfg(test)]
mod unavailable_tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;

    #[test]
    fn unavailable_is_a_503_page() {
        let r = AppError::Core(CoreError::Unavailable("File storage is down".into())).into_response();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
```

Append to `crates/pdms-web/src/routes/api.rs`:

```rust
#[cfg(test)]
mod unavailable_tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;

    #[test]
    fn unavailable_is_a_503_json_error() {
        let r = ApiError::from(CoreError::Unavailable("File storage is down".into())).into_response();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
```

- [x] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --lib sharepoint_config_tests; cargo test -p pdms-web --lib unavailable_tests`
Expected: compile errors `no variant named SharePoint`, `no field sharepoint`, and `no variant named Unavailable`.

- [x] **Step 3: Implement the configuration.** In `crates/pdms-core/src/config.rs`:

(a) Add the variant to `StorageBackend`. The `rename_all = "lowercase"` already maps it to `sharepoint`.

```rust
pub enum StorageBackend {
    Local,
    S3,
    /// Document files in a SharePoint library through Microsoft Graph
    /// (`[sharepoint]` and `[graph]`). Export packages stay in `local_path`.
    SharePoint,
}
```

(b) Add `pub sharepoint: SharePointConfig,` to `Config` after `pub storage: StorageConfig,`.

(c) Add the section struct after `S3StorageConfig`'s `Default` impl:

```rust
/// Where document files live when `[storage] backend = "sharepoint"` (spec §9).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct SharePointConfig {
    /// e.g. `https://contoso.sharepoint.com/sites/PADMRecords`.
    pub site_url: String,
    /// The document library (drive) name.
    pub library: String,
    /// How often the change check runs (P4).
    pub delta_interval_minutes: u64,
    /// Upload chunk size: a multiple of 327,680 bytes (320 KiB), at most 60 MiB.
    pub upload_chunk_bytes: u64,
}

impl Default for SharePointConfig {
    fn default() -> Self {
        SharePointConfig {
            site_url: String::new(),
            library: "Documents".to_owned(),
            delta_interval_minutes: 5,
            upload_chunk_bytes: 10_485_760,
        }
    }
}
```

(d) In `validate`, after the S3 bucket check, add:

```rust
        if self.storage.backend == StorageBackend::SharePoint {
            if let Err(e) = crate::sharepoint::layout::parse_site_url(&self.sharepoint.site_url) {
                return bad(format!("sharepoint.site_url: {e}"));
            }
            if self.sharepoint.library.trim().is_empty() {
                return bad("sharepoint.library must not be empty".into());
            }
            let c = self.sharepoint.upload_chunk_bytes;
            if c == 0 || c % 327_680 != 0 || c > 62_914_560 {
                return bad(
                    "sharepoint.upload_chunk_bytes must be a multiple of 327680 (320 KiB), at most 62914560 (60 MiB)"
                        .into(),
                );
            }
        }
```

(e) In `crates/pdms-core/src/lib.rs` `open_storage`, change the `StorageBackend::Local => …` arm's pattern to `StorageBackend::Local | StorageBackend::SharePoint => …`. Export packages always need a local store.

- [x] **Step 4: Implement the error variant and its mappings.**

`crates/pdms-core/src/error.rs`, after `Conflict(String)`:

```rust
    /// A service PADM depends on (SharePoint) is unreachable; nothing was changed.
    #[error("{0}")]
    Unavailable(String),
```

`crates/pdms-web/src/error.rs`, in the `AppError::Core(e) => match e { … }` block, before `other =>`:

```rust
                CoreError::Unavailable(m) => {
                    error_page(StatusCode::SERVICE_UNAVAILABLE, "Temporarily unavailable", &m)
                }
```

`crates/pdms-web/src/routes/api.rs`, in `impl From<CoreError> for ApiError`, before `other =>`:

```rust
            CoreError::Unavailable(m) => ApiError(StatusCode::SERVICE_UNAVAILABLE, "unavailable", m),
```

Then search for other exhaustive matches: `grep -rn "CoreError::Conflict(" crates --include=*.rs`. Every match that lists `Conflict` without a catch-all needs an `Unavailable` arm that behaves the same way.

- [x] **Step 5: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --lib sharepoint_config_tests && cargo test -p pdms-web --lib unavailable_tests`
Expected: `test result: ok. 3 passed` and `test result: ok. 2 passed`.

- [x] **Step 6: Commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core/src/config.rs crates/pdms-core/src/error.rs crates/pdms-core/src/lib.rs crates/pdms-web/src
git commit -m "config: sharepoint storage backend; CoreError::Unavailable as 503"
```

---

### Task 4: The SharePoint handle, a mock Graph, and first-use setup

On first use, PADM:
- finds the site, library, and list;
- creates any missing columns;
- creates any missing area folders.

It does this lazily, so the server still starts when SharePoint is down (the same reasoning as D-021).

**Files:**
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Create: `crates/pdms-core/src/sharepoint/folders.rs`
- Modify: `crates/pdms-core/src/lib.rs` (`Core.sharepoint`, built in `Core::new`)
- Create: `crates/pdms-core/tests/mock_graph/sp.rs`: SharePoint endpoints for P2's mock Graph
- Modify: `crates/pdms-core/tests/mock_graph/mod.rs`: the `sp` state field and three hooks
- Create: `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Consumes:
  - From P2:
    - `GraphClient::{get, post}`, `GraphError::{NotFound, Conflict}`;
    - `Core.graph`, bound as `let graph = …;` in `Core::new`, and `Config.graph: GraphConfig`;
    - test helpers `mock_graph::{MockGraph, MockState}` with `MockGraph::{start, config, requests}`, plus `pdms_core::graph::cert::generate`.
  - From Tasks 2–3: `layout::*`, `SharePointConfig`.
- Produces:
  - `pdms_core::sharepoint::SharePoint`: `Clone`, with `new(GraphClient, &SharePointConfig) -> CoreResult<SharePoint>`, `graph(&self) -> &GraphClient`, `chunk_bytes(&self) -> u64` and `async resolved(&self) -> CoreResult<&Resolved>`;
  - `pub struct Resolved { site_id, drive_id, list_id, departments_id, county_wide_id, restricted_id: String, county_wide_created: bool }`, with `area_id(&self, Area) -> &str`;
  - `Core.sharepoint: Option<SharePoint>`;
  - in tests: `mock_graph::{SITE_URL, SITE, DRIVE, LIST, SpItem}` and `MockGraph::{names, item, fields, columns, tamper, fail, heal}`.

- [x] **Step 1: Teach P2's mock Graph about SharePoint.** P2's `crates/pdms-core/tests/mock_graph/mod.rs` is the shared mock: an axum fallback handler with the token endpoint and the pre-authenticated `/upload/…` and `/blob/…` URLs. P3 adds its endpoints in a child module, so P2's tests are unaffected.

Create `crates/pdms-core/tests/mock_graph/sp.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! SharePoint endpoints for the mock Graph (plan P3): site, drive, list
//! columns, folders, upload sessions, downloads (a 302 to a pre-authenticated
//! `/blob/…` URL), moves, and list item fields. Upload and download URLs are
//! served through the hooks in `mod.rs`.

use std::collections::{BTreeMap, HashMap};

use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use pdms_core::sharepoint::quickxor::QuickXorHash;
use serde_json::{Map, Value, json};

use super::{MockGraph, MockState, graph_error, json_resp};

pub const SITE_URL: &str = "https://contoso.sharepoint.com/sites/PADMTest";
pub const SITE: &str = "site-1";
pub const DRIVE: &str = "drive-1";
pub const LIST: &str = "list-1";

#[derive(Clone, Debug)]
pub struct SpItem {
    pub id: String,
    pub name: String,
    pub parent: Option<String>,
    pub folder: bool,
    pub content: Vec<u8>,
    pub fields: Map<String, Value>,
    pub list_item: String,
}

struct Session {
    parent: String,
    name: String,
    data: Vec<u8>,
}

#[derive(Default)]
pub struct SpState {
    items: BTreeMap<String, SpItem>,
    columns: Vec<String>,
    sessions: HashMap<String, Session>,
    next: u64,
    /// While set, SharePoint requests whose "METHOD path" (path without
    /// `/v1.0`) starts with this prefix get HTTP 500.
    fail_prefix: Option<String>,
}

impl SpState {
    fn ensure_root(&mut self) {
        self.items.entry("root".into()).or_insert_with(|| SpItem {
            id: "root".into(),
            name: String::new(),
            parent: None,
            folder: true,
            content: vec![],
            fields: Map::new(),
            list_item: "sp-li-root".into(),
        });
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}-{}", self.next)
    }

    fn child(&self, parent: &str, name: &str) -> Option<&SpItem> {
        self.items
            .values()
            .find(|i| i.parent.as_deref() == Some(parent) && i.name.eq_ignore_ascii_case(name))
    }

    /// The item at a `/`-separated path from the library root.
    fn at(&self, path: &str) -> Option<&SpItem> {
        let mut current = self.items.get("root")?;
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            current = self.child(&current.id, seg)?;
        }
        Some(current)
    }

    fn json(&self, i: &SpItem) -> Value {
        let mut v = json!({
            "id": i.id, "name": i.name, "size": i.content.len(),
            "parentReference": {"id": i.parent, "driveId": DRIVE},
            "listItem": {"id": i.list_item},
        });
        if i.folder {
            v["folder"] = json!({"childCount": 0});
        } else {
            let mut h = QuickXorHash::new();
            h.update(&i.content);
            v["file"] = json!({"hashes": {"quickXorHash": h.finish_base64()}});
        }
        v
    }

    fn insert(&mut self, parent: &str, name: String, folder: bool, content: Vec<u8>) -> Value {
        let id = self.id("sp-item");
        let list_item = self.id("sp-li");
        let item = SpItem {
            id: id.clone(),
            name,
            parent: Some(parent.to_owned()),
            folder,
            content,
            fields: Map::new(),
            list_item,
        };
        let out = self.json(&item);
        self.items.insert(id, item);
        out
    }
}

impl MockGraph {
    /// Names under a folder path ("" is the library root), sorted.
    pub fn names(&self, path: &str) -> Vec<String> {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        let Some(parent) = s.sp.at(path).map(|i| i.id.clone()) else {
            return vec![];
        };
        let mut v: Vec<String> = s
            .sp
            .items
            .values()
            .filter(|i| i.parent.as_deref() == Some(parent.as_str()))
            .map(|i| i.name.clone())
            .collect();
        v.sort();
        v
    }

    pub fn item(&self, path: &str) -> Option<SpItem> {
        let mut s = self.state.lock().unwrap();
        s.sp.ensure_root();
        s.sp.at(path).cloned()
    }

    pub fn fields(&self, path: &str) -> Map<String, Value> {
        self.item(path).map(|i| i.fields).unwrap_or_default()
    }

    pub fn columns(&self) -> Vec<String> {
        self.state.lock().unwrap().sp.columns.clone()
    }

    /// Change a file's bytes, as someone editing in SharePoint would.
    pub fn tamper(&self, path: &str, bytes: &[u8]) {
        let mut s = self.state.lock().unwrap();
        let id = s.sp.at(path).unwrap().id.clone();
        s.sp.items.get_mut(&id).unwrap().content = bytes.to_vec();
    }

    /// Fail every SharePoint request whose "METHOD path" starts with `prefix`
    /// (for example "POST /drives/drive-1/items/") until [`heal`](Self::heal).
    pub fn fail(&self, prefix: &str) {
        self.state.lock().unwrap().sp.fail_prefix = Some(prefix.to_owned());
    }

    pub fn heal(&self) {
        self.state.lock().unwrap().sp.fail_prefix = None;
    }
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An authenticated `/v1.0` SharePoint request (`api` is the path after
/// `/v1.0`, without the query). `None` if the path isn't one of these.
pub(super) fn route(s: &mut MockState, base: &str, method: &Method, api: &str, body: &[u8]) -> Option<Response> {
    let sp = &mut s.sp;
    sp.ensure_root();
    let line = format!("{method} {api}");
    let segs: Vec<&str> = api.trim_start_matches('/').split('/').collect();
    let ours = matches!(segs.first(), Some(&"drives")) || matches!(segs.get(..2), Some(["sites", _]));
    if !ours {
        return None;
    }
    if sp.fail_prefix.as_deref().is_some_and(|p| line.starts_with(p)) {
        return Some(graph_error(500, "generalException"));
    }
    let r = match (method.as_str(), segs.as_slice()) {
        ("GET", ["sites", host, "sites", name]) if host.ends_with(':') => {
            if format!("https://{}/sites/{name}", host.trim_end_matches(':')) == SITE_URL {
                json_resp(200, json!({"id": SITE}))
            } else {
                graph_error(404, "itemNotFound")
            }
        }
        ("GET", ["sites", SITE, "drives"]) => {
            json_resp(200, json!({"value": [{"id": DRIVE, "name": "Documents"}]}))
        }
        ("GET", ["drives", DRIVE, "list"]) => json_resp(200, json!({"id": LIST})),
        ("GET", ["sites", SITE, "lists", LIST, "columns"]) => {
            let mut v = vec![json!({"name": "Title"})];
            v.extend(sp.columns.iter().map(|c| json!({"name": c})));
            json_resp(200, json!({"value": v}))
        }
        ("POST", ["sites", SITE, "lists", LIST, "columns"]) => {
            let v: Value = serde_json::from_slice(body).unwrap();
            let name = v["name"].as_str().unwrap().to_owned();
            sp.columns.push(name.clone());
            json_resp(201, json!({"name": name}))
        }
        ("PATCH", ["sites", SITE, "lists", LIST, "items", li, "fields"]) => {
            let v: Value = serde_json::from_slice(body).unwrap();
            match sp.items.values_mut().find(|i| i.list_item == *li) {
                Some(item) => {
                    for (k, val) in v.as_object().unwrap() {
                        item.fields.insert(k.clone(), val.clone());
                    }
                    json_resp(200, Value::Object(item.fields.clone()))
                }
                None => graph_error(404, "itemNotFound"),
            }
        }
        ("GET", ["drives", DRIVE, "root:", rest @ ..]) => {
            let p = rest.iter().map(|s| decode(s)).collect::<Vec<_>>().join("/");
            match sp.at(&p).cloned() {
                Some(i) => json_resp(200, sp.json(&i)),
                None => graph_error(404, "itemNotFound"),
            }
        }
        ("GET", ["drives", DRIVE, "items", parent, name]) if parent.ends_with(':') => {
            match sp.child(parent.trim_end_matches(':'), &decode(name)).cloned() {
                Some(i) => json_resp(200, sp.json(&i)),
                None => graph_error(404, "itemNotFound"),
            }
        }
        ("POST", ["drives", DRIVE, "root", "children"]) => create_folder(sp, "root", body),
        ("POST", ["drives", DRIVE, "items", parent, "children"]) => create_folder(sp, parent, body),
        ("POST", ["drives", DRIVE, "items", parent, name, "createUploadSession"])
            if parent.ends_with(':') && name.ends_with(':') =>
        {
            let sid = sp.id("sp-session");
            sp.sessions.insert(
                sid.clone(),
                Session {
                    parent: parent.trim_end_matches(':').to_owned(),
                    name: decode(name.trim_end_matches(':')),
                    data: Vec::new(),
                },
            );
            json_resp(200, json!({"uploadUrl": format!("{base}/upload/{sid}"), "expirationDateTime": "2099-01-01T00:00:00Z"}))
        }
        ("GET", ["drives", DRIVE, "items", id]) => match sp.items.get(*id).cloned() {
            Some(i) => json_resp(200, sp.json(&i)),
            None => graph_error(404, "itemNotFound"),
        },
        ("GET", ["drives", DRIVE, "items", id, "content"]) => {
            if sp.items.contains_key(*id) {
                let mut r = StatusCode::FOUND.into_response();
                r.headers_mut()
                    .insert(header::LOCATION, format!("{base}/blob/{id}").parse().unwrap());
                r
            } else {
                graph_error(404, "itemNotFound")
            }
        }
        ("PATCH", ["drives", DRIVE, "items", id]) => move_item(sp, id, body),
        ("DELETE", ["drives", DRIVE, "items", id]) => {
            sp.items.remove(*id);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => return None,
    };
    Some(r)
}

/// A chunk PUT to `/upload/<session>`; `None` if it isn't a SharePoint session.
pub(super) fn upload(s: &mut MockState, session: &str, headers: &HeaderMap, body: &[u8]) -> Option<Response> {
    let sp = &mut s.sp;
    let current = sp.sessions.get_mut(session)?;
    let range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let parsed = range.strip_prefix("bytes ").and_then(|r| {
        let (span, total) = r.split_once('/')?;
        let (start, _end) = span.split_once('-')?;
        Some((start.parse::<u64>().ok()?, total.parse::<u64>().ok()?))
    });
    let Some((start, total)) = parsed else {
        return Some(graph_error(400, "invalidRange"));
    };
    if start != current.data.len() as u64 {
        return Some(graph_error(416, "invalidRange"));
    }
    current.data.extend_from_slice(body);
    if (current.data.len() as u64) < total {
        let next = current.data.len();
        return Some(json_resp(202, json!({"nextExpectedRanges": [format!("{next}-")]})));
    }
    let done = sp.sessions.remove(session)?;
    if sp.child(&done.parent, &done.name).is_some() {
        return Some(graph_error(409, "nameAlreadyExists"));
    }
    Some(json_resp(201, sp.insert(&done.parent, done.name, false, done.data)))
}

/// A pre-authenticated download `/blob/<item>`; `None` if not a SharePoint item.
pub(super) fn blob(s: &MockState, item: &str) -> Option<Response> {
    let i = s.sp.items.get(item)?;
    Some((StatusCode::OK, i.content.clone()).into_response())
}

fn create_folder(sp: &mut SpState, parent: &str, body: &[u8]) -> Response {
    let v: Value = serde_json::from_slice(body).unwrap();
    let name = v["name"].as_str().unwrap().to_owned();
    if !sp.items.contains_key(parent) {
        return graph_error(404, "itemNotFound");
    }
    if sp.child(parent, &name).is_some() {
        return graph_error(409, "nameAlreadyExists");
    }
    json_resp(201, sp.insert(parent, name, true, vec![]))
}

fn move_item(sp: &mut SpState, id: &str, body: &[u8]) -> Response {
    let v: Value = serde_json::from_slice(body).unwrap();
    let Some(current) = sp.items.get(id).cloned() else {
        return graph_error(404, "itemNotFound");
    };
    let parent = v["parentReference"]["id"]
        .as_str()
        .map(str::to_owned)
        .or(current.parent.clone())
        .unwrap();
    let name = v["name"].as_str().map_or(current.name.clone(), str::to_owned);
    if sp.child(&parent, &name).is_some_and(|other| other.id != id) {
        return graph_error(409, "nameAlreadyExists");
    }
    let item = sp.items.get_mut(id).unwrap();
    item.parent = Some(parent);
    item.name = name;
    let item = item.clone();
    json_resp(200, sp.json(&item))
}
```

Then, in P2's `crates/pdms-core/tests/mock_graph/mod.rs`:

(a) after the `use` lines, add:

```rust
mod sp;
pub use sp::{DRIVE, LIST, SITE, SITE_URL, SpItem};
```

(b) add the field `pub sp: sp::SpState,` to `MockState`. Its `#[derive(Default)]` covers it.

(c) in `handle`'s `/blob/` branch, directly after the `if has_auth { … }` check, add:

```rust
        if let Some(r) = sp::blob(&s, name) {
            return r;
        }
```

(d) in `handle`'s `/upload/` branch, directly after the `if has_auth { … }` check, add:

```rust
        if let Some(r) = sp::upload(&mut s, name, &headers, &body) {
            return r;
        }
```

(e) in `handle`, directly before `match (method, api) {`, after the `fail_next` block, add:

```rust
    if let Some(r) = sp::route(&mut s, &base, &method, api, &body) {
        return r;
    }
```

The test certificate comes from P2's generator, once per test binary (RSA key generation is the slow part). PEM files are git-ignored, so nothing is committed.

- [x] **Step 2: Write the failing integration test.** Create `crates/pdms-core/tests/sharepoint.rs`. Later tasks add more tests to this file.

```rust
#![allow(
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! SharePoint document storage (P3) against the in-process mock Graph.

mod common;
mod mock_graph;

use std::sync::OnceLock;

use common::*;
use mock_graph::MockGraph;
use pdms_core::config::StorageBackend;
use pdms_core::{Config, Core, sharepoint};

/// One certificate per test binary (RSA key generation is the slow part).
fn certificate() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| pdms_core::graph::cert::generate("PADM test", 30).unwrap().pem)
}

async fn core_with(f: impl FnOnce(&mut Config)) -> (Core, pdms_testkit::TestDb) {
    let (core, db) = core().await;
    let mut config = (*core.config).clone();
    f(&mut config);
    (Core::new(core.db.clone(), config).unwrap(), db)
}

/// A `Core` that stores document files in the mock SharePoint, in 320 KiB chunks.
async fn sp_core(sp: &MockGraph) -> (Core, pdms_testkit::TestDb) {
    let graph = sp.config(certificate());
    core_with(|c| {
        c.graph = graph;
        c.storage.backend = StorageBackend::SharePoint;
        c.sharepoint.site_url = mock_graph::SITE_URL.into();
        c.sharepoint.upload_chunk_bytes = 327_680;
    })
    .await
}

#[tokio::test]
async fn first_use_creates_the_area_folders_and_columns() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let handle = core.sharepoint.clone().expect("sharepoint backend");
    let r = handle.resolved().await.unwrap();
    assert_eq!(sp.names(""), vec!["County-wide", "Departments", "Restricted"]);
    assert!(r.county_wide_created, "PADM had to create County-wide itself");
    for c in ["PadmDocumentId", "PadmVisibility", "PadmCurrent", "PadmLink"] {
        assert!(sp.columns().contains(&c.to_owned()), "{c}");
    }
    // Another process finds everything instead of creating duplicates.
    let other = sharepoint::SharePoint::new(core.graph.clone().unwrap(), &core.config.sharepoint).unwrap();
    let r2 = other.resolved().await.unwrap();
    assert_eq!(r2.departments_id, r.departments_id);
    assert!(!r2.county_wide_created);
    assert_eq!(sp.columns().len(), 14, "no column created twice");
}

#[tokio::test]
async fn the_sharepoint_backend_needs_the_graph_section() {
    let (core, _db) = core().await;
    let mut config = (*core.config).clone();
    config.storage.backend = StorageBackend::SharePoint;
    config.sharepoint.site_url = mock_graph::SITE_URL.into();
    let err = Core::new(core.db.clone(), config).unwrap_err();
    assert!(err.to_string().contains("[graph]"), "{err}");
}
```

- [x] **Step 3: Run the test and confirm it fails.**

Run: `cargo test -p pdms-core --test sharepoint`
Expected: compile errors `no field sharepoint on type Core` and `cannot find SharePoint in sharepoint`.

- [x] **Step 4: Implement `folders.rs` (area folders only, for now).** Create `crates/pdms-core/src/sharepoint/folders.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Area, department, and document folders. They are created on demand and
//! tracked by Graph item ID, so renames and moves never depend on paths.

use serde::Deserialize;
use serde_json::json;

use super::layout::{self, Area};
use crate::error::CoreResult;
use crate::graph::{GraphClient, GraphError};

#[derive(Deserialize)]
pub(super) struct Item {
    pub id: String,
}

/// A top-level area folder, created if missing. Returns its ID and whether
/// PADM created it.
pub(super) async fn ensure_area_folder(g: &GraphClient, drive: &str, area: Area) -> CoreResult<(String, bool)> {
    let path = format!(
        "/drives/{drive}/root:/{}?$select=id",
        layout::encode_segment(area.folder_name())
    );
    match g.get::<Item>(&path).await {
        Ok(item) => return Ok((item.id, false)),
        Err(GraphError::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    let body = json!({"name": area.folder_name(), "folder": {}, "@microsoft.graph.conflictBehavior": "fail"});
    match g.post::<_, Item>(&format!("/drives/{drive}/root/children"), &body).await {
        Ok(item) => Ok((item.id, true)),
        // Another process created it first.
        Err(GraphError::Conflict) => Ok((g.get::<Item>(&path).await?.id, false)),
        Err(e) => Err(e.into()),
    }
}
```

- [x] **Step 5: Implement the handle.** Replace `crates/pdms-core/src/sharepoint/mod.rs` with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Document files in a SharePoint library, through Microsoft Graph (D-081 to D-084).
//!
//! PADM is the only writer. Files live in `Departments/`, `County-wide/`, or
//! `Restricted/`, then a department folder, then one folder per document with
//! one file per version (spec §5). Uploads are synchronous. Renames, moves, and
//! column values are applied by the `sharepoint.mirror_document` job, except
//! moves that narrow who can browse a document: those happen before the
//! change is saved (spec §7.3).

mod folders;
pub mod layout;
pub mod quickxor;

use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::OnceCell;

use crate::config::SharePointConfig;
use crate::error::{CoreError, CoreResult};
use crate::graph::GraphClient;

pub use layout::{Area, Spot};

/// SharePoint storage, shared by the web server and the worker.
#[derive(Clone)]
pub struct SharePoint {
    inner: Arc<Inner>,
}

struct Inner {
    graph: GraphClient,
    host: String,
    site_path: String,
    library: String,
    chunk_bytes: u64,
    resolved: OnceCell<Resolved>,
}

/// Graph IDs, looked up once per process on first use.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub site_id: String,
    pub drive_id: String,
    pub list_id: String,
    pub departments_id: String,
    pub county_wide_id: String,
    pub restricted_id: String,
    /// PADM created `County-wide/` itself, so it lacks the "Everyone except
    /// external users" read grant an admin adds at setup (spec Appendix A, step 5).
    pub county_wide_created: bool,
}

impl Resolved {
    pub fn area_id(&self, area: Area) -> &str {
        match area {
            Area::Departments => &self.departments_id,
            Area::CountyWide => &self.county_wide_id,
            Area::Restricted => &self.restricted_id,
        }
    }
}

impl SharePoint {
    pub fn new(graph: GraphClient, cfg: &SharePointConfig) -> CoreResult<SharePoint> {
        let (host, site_path) = layout::parse_site_url(&cfg.site_url)
            .map_err(|e| CoreError::validation(format!("sharepoint.site_url: {e}")))?;
        Ok(SharePoint {
            inner: Arc::new(Inner {
                graph,
                host,
                site_path,
                library: cfg.library.trim().to_owned(),
                chunk_bytes: cfg.upload_chunk_bytes,
                resolved: OnceCell::new(),
            }),
        })
    }

    pub fn graph(&self) -> &GraphClient {
        &self.inner.graph
    }

    pub fn chunk_bytes(&self) -> u64 {
        self.inner.chunk_bytes
    }

    /// Site, library, list, and area folder IDs. Missing columns and area
    /// folders are created. A failure is not cached; the next call retries.
    pub async fn resolved(&self) -> CoreResult<&Resolved> {
        self.inner.resolved.get_or_try_init(|| resolve(&self.inner)).await
    }
}

#[derive(Deserialize)]
struct IdOnly {
    id: String,
}

#[derive(Deserialize)]
struct Values<T> {
    value: Vec<T>,
}

#[derive(Deserialize)]
struct Drive {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct Column {
    name: String,
}

async fn resolve(inner: &Inner) -> CoreResult<Resolved> {
    let g = &inner.graph;
    // `site_path` comes from a parsed URL, so it is already percent-encoded.
    let site: IdOnly = g
        .get(&format!("/sites/{}:{}?$select=id", inner.host, inner.site_path))
        .await?;
    let drives: Values<Drive> = g
        .get(&format!("/sites/{}/drives?$select=id,name", site.id))
        .await?;
    let drive = drives
        .value
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(&inner.library))
        .ok_or_else(|| {
            CoreError::validation(format!(
                "the SharePoint library {:?} was not found on the site",
                inner.library
            ))
        })?;
    let list: IdOnly = g.get(&format!("/drives/{}/list?$select=id", drive.id)).await?;
    let have: Values<Column> = g
        .get(&format!("/sites/{}/lists/{}/columns?$select=name", site.id, list.id))
        .await?;
    for def in layout::column_definitions() {
        let name = def.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        if !have.value.iter().any(|c| c.name == name) {
            let _: serde_json::Value = g
                .post(&format!("/sites/{}/lists/{}/columns", site.id, list.id), &def)
                .await?;
        }
    }
    let (departments_id, _) = folders::ensure_area_folder(g, &drive.id, Area::Departments).await?;
    let (county_wide_id, county_wide_created) =
        folders::ensure_area_folder(g, &drive.id, Area::CountyWide).await?;
    let (restricted_id, _) = folders::ensure_area_folder(g, &drive.id, Area::Restricted).await?;
    if county_wide_created {
        tracing::warn!(
            "PADM created the County-wide folder in SharePoint; give \"Everyone except external users\" read access to it (see the SharePoint setup guide)"
        );
    }
    Ok(Resolved {
        site_id: site.id,
        drive_id: drive.id,
        list_id: list.id,
        departments_id,
        county_wide_id,
        restricted_id,
        county_wide_created,
    })
}
```

- [x] **Step 6: Wire it into `Core`.** In `crates/pdms-core/src/lib.rs`:

(a) Add the field to `Core`, after `pub ocr: …`:

```rust
    /// Document files in SharePoint (`[storage] backend = "sharepoint"`).
    pub sharepoint: Option<sharepoint::SharePoint>,
```

(b) In `Core::new`, P2 binds `let graph = if config.graph.is_configured() { … } else { None };` just before `Ok(Core {`. Directly after that binding, add:

```rust
        let sharepoint = match config.storage.backend {
            StorageBackend::SharePoint => {
                let graph = graph.clone().ok_or_else(|| {
                    CoreError::validation(
                        "storage.backend = \"sharepoint\" needs the [graph] section (tenant_id, client_id, certificate)",
                    )
                })?;
                Some(sharepoint::SharePoint::new(graph, &config.sharepoint)?)
            }
            _ => None,
        };
```

and add `sharepoint,` to the `Core { … }` literal.

- [x] **Step 7: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint`
Expected: `test result: ok. 2 passed`. The mock answers `POST /tenant-1/oauth2/v2.0/token` only for a well-formed client assertion (P2), so a token failure here points at the `[graph]` settings, not at SharePoint.

- [x] **Step 8: Commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core/src crates/pdms-core/tests/mock_graph crates/pdms-core/tests/sharepoint.rs
git commit -m "sharepoint: resolve site, library, columns, and area folders on first use"
```

---

### Task 5: Migration 0013 and department folders

**Files:**
- Create: `migrations/0013_sharepoint_files.sql`
- Modify: `crates/pdms-core/src/sharepoint/folders.rs`
- Modify: `crates/pdms-core/src/sharepoint/mod.rs` (export)
- Test: `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Consumes: `SharePoint::resolved` and `Resolved::area_id` (Task 4).
- Produces:
  - tables `sharepoint_department_folders`, `sharepoint_document_folders`, `sharepoint_files`;
  - `pub async fn department_folder_id(core: &Core, area: Area, department_id: Uuid) -> CoreResult<String>` (index contract), re-exported as `pdms_core::sharepoint::department_folder_id`;
  - `pub(super) async fn create_folder(db, g, drive, parent, name, fallback) -> CoreResult<(String, String)>`;
  - `pub(super) async fn move_item(g, drive, item, parent: Option<&str>, name, fallback) -> CoreResult<String>`.

- [x] **Step 1: Write the migration.** Create `migrations/0013_sharepoint_files.sql`:

```sql
-- SharePoint document storage (spec §5–§7, plan P3). Files live in one
-- SharePoint library; these tables remember where, by Graph item ID, so
-- renames and moves never depend on paths.

-- One folder per department inside each area folder.
CREATE TABLE sharepoint_department_folders (
    area          text NOT NULL CHECK (area IN ('departments', 'county_wide', 'restricted')),
    department_id uuid NOT NULL REFERENCES departments (id),
    item_id       text NOT NULL UNIQUE,
    name          text NOT NULL,
    PRIMARY KEY (area, department_id)
);

-- One folder per document; it moves between department folders.
CREATE TABLE sharepoint_document_folders (
    document_id    uuid PRIMARY KEY REFERENCES documents (id),
    item_id        text NOT NULL UNIQUE,
    parent_item_id text NOT NULL,
    name           text NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

-- One file per version that introduced a file. Never deleted.
CREATE TABLE sharepoint_files (
    document_version_id uuid PRIMARY KEY REFERENCES document_versions (id),
    document_id         uuid NOT NULL REFERENCES documents (id),
    drive_id            text NOT NULL,
    item_id             text NOT NULL UNIQUE,
    list_item_id        text NOT NULL,
    name                text NOT NULL,
    quick_xor_hash      text NOT NULL,          -- recorded at upload, compared by change detection (P4)
    size                bigint NOT NULL CHECK (size >= 0),
    last_known_path     text NOT NULL,
    fields_hash         text,                   -- SHA-256 of the column values last written
    state               text NOT NULL DEFAULT 'ok'
                        CHECK (state IN ('ok', 'changed_outside', 'missing')),
    uploaded_at         timestamptz NOT NULL DEFAULT now(),
    checked_at          timestamptz
);
CREATE INDEX sharepoint_files_document_idx ON sharepoint_files (document_id);

GRANT SELECT, INSERT, UPDATE ON sharepoint_department_folders, sharepoint_document_folders,
                                sharepoint_files TO pdms_app;
```

Apply it to the dev database: `sqlx migrate run --source migrations`
Expected: `Applied 13/migrate sharepoint files`.

- [x] **Step 2: Write the failing tests.** Add these imports to `tests/sharepoint.rs`: `use pdms_core::directory;`, `use pdms_core::sharepoint::Area;` and `use uuid::Uuid;`. Then add:

```rust
async fn new_department(core: &Core, code: &str, name: &str) -> Uuid {
    directory::create_department(&core.db, &system(), code, name).await.unwrap()
}

#[tokio::test]
async fn department_folders_are_created_once_per_area() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let rb = new_department(&core, "RB", "Road and Bridge").await;
    let a = sharepoint::department_folder_id(&core, Area::Departments, rb).await.unwrap();
    let b = sharepoint::department_folder_id(&core, Area::Departments, rb).await.unwrap();
    assert_eq!(a, b);
    sharepoint::department_folder_id(&core, Area::CountyWide, rb).await.unwrap();
    assert_eq!(sp.names("Departments"), vec!["Road and Bridge"]);
    assert_eq!(sp.names("County-wide"), vec!["Road and Bridge"]);
}

#[tokio::test]
async fn departments_with_the_same_folder_name_get_their_own_folders() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let one = new_department(&core, "RB1", "Road:Bridge").await;
    let two = new_department(&core, "RB2", "Road|Bridge").await;
    let a = sharepoint::department_folder_id(&core, Area::Departments, one).await.unwrap();
    let b = sharepoint::department_folder_id(&core, Area::Departments, two).await.unwrap();
    assert_ne!(a, b);
    assert_eq!(sp.names("Departments"), vec!["Road_Bridge", "Road_Bridge (RB2)"]);
}

#[tokio::test]
async fn renaming_a_department_renames_its_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let rb = new_department(&core, "RB", "Road and Bridge").await;
    let before = sharepoint::department_folder_id(&core, Area::Departments, rb).await.unwrap();
    sqlx::query("UPDATE departments SET name = 'Roads' WHERE id = $1")
        .bind(rb)
        .execute(&core.db)
        .await
        .unwrap();
    let after = sharepoint::department_folder_id(&core, Area::Departments, rb).await.unwrap();
    assert_eq!(before, after, "renamed in place");
    assert_eq!(sp.names("Departments"), vec!["Roads"]);
}

#[tokio::test]
async fn an_untracked_folder_left_by_an_interrupted_run_is_adopted() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let rb = new_department(&core, "RB", "Road and Bridge").await;
    let first = sharepoint::department_folder_id(&core, Area::Departments, rb).await.unwrap();
    // Forget the row, as if the process died after creating the folder.
    sqlx::query("DELETE FROM sharepoint_department_folders")
        .execute(&_db.owner)
        .await
        .unwrap();
    let again = sharepoint::department_folder_id(&core, Area::Departments, rb).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(sp.names("Departments"), vec!["Road and Bridge"]);
}
```

- [x] **Step 3: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --test sharepoint department`
Expected: compile error `cannot find function department_folder_id in module sharepoint`.

- [x] **Step 4: Implement.** In `folders.rs`, extend the imports:

```rust
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::layout::{self, Area};
use crate::Core;
use crate::error::{CoreError, CoreResult};
use crate::graph::{GraphClient, GraphError};
```

and append:

```rust
/// The folder for `department_id` inside `area`: created if missing, renamed
/// when the department's name changed. Another department may already have
/// the plain name; in that case the folder is suffixed with the department code.
pub async fn department_folder_id(core: &Core, area: Area, department_id: Uuid) -> CoreResult<String> {
    let sp = core
        .sharepoint
        .as_ref()
        .ok_or_else(|| CoreError::internal("SharePoint storage is not configured"))?;
    let r = sp.resolved().await?;
    let dept = sqlx::query!("SELECT name, code FROM departments WHERE id = $1", department_id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(CoreError::NotFound)?;
    let want = layout::department_folder_name(&dept.name);
    let fallback = layout::department_folder_name_with_code(&dept.name, &dept.code);
    let row = sqlx::query!(
        "SELECT item_id, name FROM sharepoint_department_folders WHERE area = $1 AND department_id = $2",
        area.as_str(),
        department_id
    )
    .fetch_optional(&core.db)
    .await?;
    if let Some(row) = row {
        if row.name != want && row.name != fallback {
            let name = move_item(sp.graph(), &r.drive_id, &row.item_id, None, &want, &fallback).await?;
            sqlx::query!(
                "UPDATE sharepoint_department_folders SET name = $3 WHERE area = $1 AND department_id = $2",
                area.as_str(),
                department_id,
                name
            )
            .execute(&core.db)
            .await?;
        }
        return Ok(row.item_id);
    }
    let (item_id, name) =
        create_folder(&core.db, sp.graph(), &r.drive_id, r.area_id(area), &want, &fallback).await?;
    sqlx::query!(
        "INSERT INTO sharepoint_department_folders (area, department_id, item_id, name)
         VALUES ($1, $2, $3, $4) ON CONFLICT (area, department_id) DO NOTHING",
        area.as_str(),
        department_id,
        item_id,
        name
    )
    .execute(&core.db)
    .await?;
    // A concurrent caller may have won; use whatever was recorded.
    Ok(sqlx::query_scalar!(
        "SELECT item_id FROM sharepoint_department_folders WHERE area = $1 AND department_id = $2",
        area.as_str(),
        department_id
    )
    .fetch_one(&core.db)
    .await?)
}

/// Create folder `name` under `parent`. If the name is taken by a folder no
/// PADM record points at (left by an interrupted run), adopt it. If it
/// belongs to another record, try `fallback`.
pub(super) async fn create_folder(
    db: &PgPool,
    g: &GraphClient,
    drive: &str,
    parent: &str,
    name: &str,
    fallback: &str,
) -> CoreResult<(String, String)> {
    for candidate in [name, fallback] {
        let body = json!({"name": candidate, "folder": {}, "@microsoft.graph.conflictBehavior": "fail"});
        match g.post::<_, Item>(&format!("/drives/{drive}/items/{parent}/children"), &body).await {
            Ok(item) => return Ok((item.id, candidate.to_owned())),
            Err(GraphError::Conflict) => {
                let existing: Item = g
                    .get(&format!(
                        "/drives/{drive}/items/{parent}:/{}?$select=id",
                        layout::encode_segment(candidate)
                    ))
                    .await?;
                if !is_tracked(db, &existing.id).await? {
                    return Ok((existing.id, candidate.to_owned()));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(CoreError::Conflict(format!(
        "SharePoint already has folders named {name:?} and {fallback:?} that belong to other records"
    )))
}

async fn is_tracked(db: &PgPool, item_id: &str) -> CoreResult<bool> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM sharepoint_department_folders WHERE item_id = $1)
               OR EXISTS (SELECT 1 FROM sharepoint_document_folders WHERE item_id = $1) AS "tracked!""#,
        item_id
    )
    .fetch_one(db)
    .await?)
}

/// Rename an item, and with `parent` also move it. If the name is taken
/// there, try `fallback`. Returns the name it ended up with.
pub(super) async fn move_item(
    g: &GraphClient,
    drive: &str,
    item: &str,
    parent: Option<&str>,
    name: &str,
    fallback: &str,
) -> CoreResult<String> {
    for candidate in [name, fallback] {
        let mut body = json!({"name": candidate});
        if let (Some(p), Some(o)) = (parent, body.as_object_mut()) {
            o.insert("parentReference".into(), json!({"id": p}));
        }
        match g.patch::<_, serde_json::Value>(&format!("/drives/{drive}/items/{item}"), &body).await {
            Ok(_) => return Ok(candidate.to_owned()),
            Err(GraphError::Conflict) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(CoreError::Conflict(format!(
        "SharePoint already has items named {name:?} and {fallback:?} in that folder"
    )))
}
```

In `mod.rs`, add `pub use folders::department_folder_id;` after `pub use layout::{Area, Spot};`.

- [x] **Step 5: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint`
Expected: `test result: ok. 6 passed`.

- [x] **Step 6: Refresh the offline query data and commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add migrations/0013_sharepoint_files.sql crates/pdms-core .sqlx
git commit -m "sharepoint: migration 0013 and department folders tracked by item ID"
```

---

### Task 6: Uploads go to SharePoint

**Files:**
- Create: `crates/pdms-core/src/sharepoint/files.rs`
- Create: `crates/pdms-core/src/documents/filestore.rs`
- Modify: `crates/pdms-core/src/sharepoint/folders.rs` (document folders)
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Modify: `crates/pdms-core/src/documents/mod.rs` (`pub mod filestore;`)
- Modify: `crates/pdms-core/src/documents/files.rs` (`ingest` stops storing)
- Modify: `crates/pdms-core/src/documents/service.rs` (`create`, `add_version`)
- Test: `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Consumes: `department_folder_id`, `create_folder` (Task 5); P2's `GraphClient::{post, get, put_bytes, delete}`.
- Produces:
  - `pub struct UploadTarget<'a> { document_id: Option<Uuid>, external_id: &'a str, title: &'a str, spot: Spot }`;
  - `pub struct UploadedFile { folder: DocumentFolder, drive_id, item_id, list_item_id, name, quick_xor_hash: String, size: u64 }`;
  - `pub struct DocumentFolder { item_id, parent_item_id, name: String }`;
  - `pub(crate) async fn upload_staged(core, &UploadTarget, src: &Path, size: u64, ext: &str) -> CoreResult<UploadedFile>`;
  - `pub(crate) async fn record_upload(conn, document_id, version_no, &UploadedFile) -> CoreResult<()>`;
  - `pub(crate) fn unavailable(message, error) -> CoreError` and `UPLOAD_UNAVAILABLE`;
  - `documents::filestore`:
    - `pub enum FileLocation { Blob, SharePoint(Box<UploadedFile>) }`;
    - `pub struct PutTarget<'a> { document_id: Option<Uuid>, external_id: &'a str, title: &'a str, visibility: Visibility, department_id: Uuid }`;
    - `pub async fn put_version_file(core, &PutTarget, &StoredFile, src: &Path) -> CoreResult<FileLocation>`;
    - `pub(crate) async fn record_location(conn, document_id, version_no, &FileLocation) -> CoreResult<()>`;
    - `pub fn backend_name(core) -> &'static str`.

- [x] **Step 1: Write the failing tests.** Add to the imports of `tests/sharepoint.rs`:

```rust
use pdms_authz::{Permission, Scope, Subject, Visibility};
use pdms_core::documents::{self, DocumentStatus, MetadataInput, NewDocument, types};
use pdms_core::{CoreError, Ctx, acl};
```

and add:

```rust
/// A department whose members may create and edit; returns (department, member).
async fn editing_department(core: &Core, code: &str, name: &str, email: &str) -> (Uuid, Uuid) {
    let dept = new_department(core, code, name).await;
    let user = directory::create_user(&core.db, &system(), email, email, Some(dept)).await.unwrap();
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(&core.db, &system(), Subject::Department(dept), Scope::Department(dept), p, None, "test")
            .await
            .unwrap();
    }
    (dept, user)
}

async fn try_new_doc(
    core: &Core,
    ctx: &Ctx,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    visibility: Visibility,
    file: Option<(&str, &[u8])>,
) -> Result<Uuid, CoreError> {
    let file = match file {
        Some((name, bytes)) => Some(upload(core, name, bytes).await),
        None => None,
    };
    let other = types::type_by_key(&core.db, "other").await.unwrap().id;
    documents::create(
        core,
        ctx,
        NewDocument {
            metadata: MetadataInput {
                title: title.into(),
                document_type_id: other,
                department_id: dept,
                ..Default::default()
            },
            owner_id: owner,
            status: DocumentStatus::Active,
            visibility,
            tags: vec![],
            file,
            note: String::new(),
        },
    )
    .await
}

async fn new_doc(
    core: &Core,
    ctx: &Ctx,
    dept: Uuid,
    owner: Uuid,
    title: &str,
    visibility: Visibility,
    file: Option<(&str, &[u8])>,
) -> Uuid {
    try_new_doc(core, ctx, dept, owner, title, visibility, file).await.unwrap()
}

async fn external_id(core: &Core, id: Uuid) -> String {
    sqlx::query_scalar("SELECT external_id FROM documents WHERE id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await
        .unwrap()
}

async fn document_count(core: &Core) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM documents").fetch_one(&core.db).await.unwrap()
}

#[tokio::test]
async fn an_upload_lands_in_its_department_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = pdf("Fuel supply agreement");
    let id = new_doc(&core, &ctx, dept, alice, "Fuel Supply Agreement", Visibility::Department, Some(("agreement.pdf", &bytes))).await;
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Fuel Supply Agreement");
    let names = sp.names(&folder);
    assert_eq!(names.len(), 1, "{names:?}");
    // Until the mirror job runs, the file carries its temporary name.
    assert!(names[0].starts_with(&format!("{ext} (uploading ")) && names[0].ends_with(".pdf"), "{names:?}");
    assert_eq!(sp.item(&format!("{folder}/{}", names[0])).unwrap().content, bytes);
    let backend: String = sqlx::query_scalar(
        "SELECT b.storage_backend FROM blobs b JOIN document_versions v ON v.blob_sha256 = b.sha256 WHERE v.document_id = $1",
    )
    .bind(id)
    .fetch_one(&core.db)
    .await
    .unwrap();
    assert_eq!(backend, "sharepoint");
    let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM sharepoint_files WHERE document_id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(recorded, 1);
}

#[tokio::test]
async fn large_files_are_sent_in_chunks() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = "PADM chunk test\n".repeat(50_000).into_bytes(); // 800,000 bytes: three 320 KiB chunks
    let id = new_doc(&core, &ctx, dept, alice, "Big notes", Visibility::Department, Some(("notes.txt", &bytes))).await;
    let puts = sp.requests().iter().filter(|r| r.starts_with("PUT /upload/")).count();
    assert_eq!(puts, 3);
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Big notes");
    let name = sp.names(&folder).pop().unwrap();
    assert!(name.ends_with(".txt"));
    assert_eq!(sp.item(&format!("{folder}/{name}")).unwrap().content, bytes);
}

#[tokio::test]
async fn a_new_version_goes_into_the_same_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("v1")))).await;
    documents::add_version(&core, &ctx, id, upload(&core, "b.pdf", &pdf("v2")).await, documents::VersionKind::Amendment, "", None)
        .await
        .unwrap();
    let ext = external_id(&core, id).await;
    assert_eq!(sp.names(&format!("Departments/Road and Bridge/{ext} – Fuel")).len(), 2);
}

#[tokio::test]
async fn awkward_titles_still_get_safe_folders() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let title = format!("~Q3 \"Final\" Agreement: Road #5 / Bridge 50% _vti_ *draft*... {}", "x".repeat(400));
    let id = new_doc(&core, &ctx, dept, alice, &title, Visibility::Department, Some(("a.pdf", &pdf("x")))).await;
    let ext = external_id(&core, id).await;
    let folders = sp.names("Departments/Road and Bridge");
    assert_eq!(folders.len(), 1);
    let f = &folders[0];
    assert!(f.starts_with(&format!("{ext} – Q3 _Final_ Agreement_ Road _5 _ Bridge 50_ _vti-")), "{f}");
    assert!(f.chars().count() <= 100);
    assert!(!f.chars().any(|c| "\"*:<>?/\\|#%".contains(c)));
}

#[tokio::test]
async fn upload_failure_saves_nothing() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    core.sharepoint.clone().unwrap().resolved().await.unwrap();
    let before = document_count(&core).await;
    sp.fail("POST /drives/drive-1/items/");
    let err = try_new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("x"))))
        .await
        .unwrap_err();
    assert!(matches!(&err, CoreError::Unavailable(m) if m.contains("nothing was saved")), "{err:?}");
    assert_eq!(document_count(&core).await, before);
}

#[tokio::test]
async fn empty_files_are_refused() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let err = try_new_doc(&core, &ctx, dept, alice, "Empty", Visibility::Department, Some(("empty.txt", b"")))
        .await
        .unwrap_err();
    assert!(matches!(&err, CoreError::Validation(m) if m.contains("empty")), "{err:?}");
}
```

- [x] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --test sharepoint`
Expected: the new upload tests FAIL. For example, `an_upload_lands_in_its_department_folder` panics because `sp.names(…)` is empty: files still go to the local blob store. `upload_failure_saves_nothing` fails because the document is created.

- [x] **Step 3: Check the `bytes` dependency.** P2 (Task 3) added `bytes = "1"` to `crates/pdms-core/Cargo.toml`. Confirm with `grep '^bytes' crates/pdms-core/Cargo.toml`, which should print one line. Nothing else to add.

- [x] **Step 4: Add document folders.** Append to `folders.rs`:

```rust
/// Where a document's files go. The folder is created on the first upload;
/// the row is written in the version's transaction (`record_upload`).
#[derive(Clone, Debug)]
pub struct DocumentFolder {
    pub item_id: String,
    pub parent_item_id: String,
    pub name: String,
}

/// Eight random hex characters, to keep a fallback name unique.
pub(super) fn token() -> String {
    Uuid::new_v4().simple().to_string().chars().take(8).collect()
}

pub(super) async fn document_folder(
    core: &Core,
    document_id: Option<Uuid>,
    external_id: &str,
    title: &str,
    spot: layout::Spot,
) -> CoreResult<DocumentFolder> {
    if let Some(id) = document_id {
        let existing = sqlx::query_as!(
            DocumentFolder,
            "SELECT item_id, parent_item_id, name FROM sharepoint_document_folders WHERE document_id = $1",
            id
        )
        .fetch_optional(&core.db)
        .await?;
        if let Some(f) = existing {
            return Ok(f);
        }
    }
    let sp = core
        .sharepoint
        .as_ref()
        .ok_or_else(|| CoreError::internal("SharePoint storage is not configured"))?;
    let r = sp.resolved().await?;
    let parent = department_folder_id(core, spot.area, spot.department_id).await?;
    let name = layout::document_folder_name(external_id, title);
    let fallback = layout::document_folder_name(external_id, &format!("{title} {}", token()));
    let (item_id, name) = create_folder(&core.db, sp.graph(), &r.drive_id, &parent, &name, &fallback).await?;
    Ok(DocumentFolder {
        item_id,
        parent_item_id: parent,
        name,
    })
}
```

- [x] **Step 5: Implement uploads.** Create `crates/pdms-core/src/sharepoint/files.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Uploading document files to SharePoint and reading them back, verified.

use std::path::Path;

use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgConnection;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::folders::{self, DocumentFolder, Item};
use super::layout::{self, Spot};
use super::quickxor::QuickXorHash;
use super::{UPLOAD_UNAVAILABLE, unavailable};
use crate::Core;
use crate::error::{CoreError, CoreResult};

pub struct UploadTarget<'a> {
    /// `None` while the document is being created.
    pub document_id: Option<Uuid>,
    pub external_id: &'a str,
    pub title: &'a str,
    pub spot: Spot,
}

/// A file stored in SharePoint and not yet recorded in the database.
#[derive(Clone, Debug)]
pub struct UploadedFile {
    pub folder: DocumentFolder,
    pub drive_id: String,
    pub item_id: String,
    pub list_item_id: String,
    pub name: String,
    pub quick_xor_hash: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct Session {
    #[serde(rename = "uploadUrl")]
    upload_url: String,
}

#[derive(Deserialize)]
struct Uploaded {
    id: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    file: Option<FileFacet>,
}

#[derive(Deserialize)]
struct FileFacet {
    #[serde(default)]
    hashes: Hashes,
}

#[derive(Deserialize, Default)]
struct Hashes {
    #[serde(rename = "quickXorHash", default)]
    quick_xor_hash: Option<String>,
}

#[derive(Deserialize)]
struct WithListItem {
    #[serde(rename = "listItem")]
    list_item: Option<Item>,
}

/// Upload `src` under a temporary name in the document's folder, which is
/// created if needed. The mirror job gives the file its final name once the
/// version number is known (spec §7.1). Nothing is written to the database
/// here. If SharePoint stores something other than what was sent, the item
/// is deleted and the upload fails.
pub(crate) async fn upload_staged(
    core: &Core,
    target: &UploadTarget<'_>,
    src: &Path,
    size: u64,
    ext: &str,
) -> CoreResult<UploadedFile> {
    let sp = core
        .sharepoint
        .as_ref()
        .ok_or_else(|| CoreError::internal("SharePoint storage is not configured"))?;
    let run = async {
        let r = sp.resolved().await?;
        let folder = folders::document_folder(core, target.document_id, target.external_id, target.title, target.spot).await?;
        let name = layout::staging_name(target.external_id, &folders::token(), ext);
        let g = sp.graph();
        let session: Session = g
            .post(
                &format!(
                    "/drives/{}/items/{}:/{}:/createUploadSession",
                    r.drive_id,
                    folder.item_id,
                    layout::encode_segment(&name)
                ),
                &json!({"item": {"@microsoft.graph.conflictBehavior": "fail", "name": name}}),
            )
            .await?;
        let mut file = tokio::fs::File::open(src).await.map_err(CoreError::internal)?;
        let mut buf = vec![0u8; usize::try_from(sp.chunk_bytes()).unwrap_or(327_680)];
        let mut qxh = QuickXorHash::new();
        let mut start = 0u64;
        let mut finished = None;
        while start < size {
            let n = read_full(&mut file, &mut buf).await?;
            let piece = buf.get(..n).unwrap_or_default();
            if piece.is_empty() {
                return Err(CoreError::internal("the spooled upload ended early"));
            }
            qxh.update(piece);
            let end = start + piece.len() as u64 - 1;
            finished = g
                .put_bytes(&session.upload_url, Bytes::copy_from_slice(piece), (start, end, size))
                .await?;
            start = end + 1;
        }
        let item: Uploaded = serde_json::from_value(
            finished.ok_or_else(|| CoreError::internal("SharePoint did not confirm the upload"))?,
        )
        .map_err(CoreError::internal)?;
        let local = qxh.finish_base64();
        let remote = item.file.and_then(|f| f.hashes.quick_xor_hash);
        if item.size != size || remote.as_deref() != Some(local.as_str()) {
            let _ = g.delete(&format!("/drives/{}/items/{}", r.drive_id, item.id)).await;
            return Err(CoreError::internal(format!(
                "SharePoint stored {} bytes with quickXorHash {remote:?}; PADM sent {size} bytes with {local}",
                item.size
            )));
        }
        let with: WithListItem = g
            .get(&format!(
                "/drives/{}/items/{}?$select=id&$expand=listItem($select=id)",
                r.drive_id, item.id
            ))
            .await?;
        let list_item_id = with
            .list_item
            .map(|l| l.id)
            .ok_or_else(|| CoreError::internal("SharePoint returned no list item for the upload"))?;
        Ok::<UploadedFile, CoreError>(UploadedFile {
            folder,
            drive_id: r.drive_id.clone(),
            item_id: item.id,
            list_item_id,
            name,
            quick_xor_hash: local,
            size,
        })
    };
    run.await.map_err(|e| match e {
        // Graph failures (including conflicts) arrive as Internal or Conflict (P2's
        // mapping); people see the plain "unavailable" message, the detail is logged.
        e @ (CoreError::Validation(_) | CoreError::NotFound) => e,
        other => unavailable(UPLOAD_UNAVAILABLE, other),
    })
}

async fn read_full(file: &mut tokio::fs::File, buf: &mut [u8]) -> CoreResult<usize> {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..) {
        if rest.is_empty() {
            break;
        }
        let n = file.read(rest).await.map_err(CoreError::internal)?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

/// Record an uploaded file against the version that now owns it. Runs in the
/// version's transaction.
pub(crate) async fn record_upload(
    conn: &mut PgConnection,
    document_id: Uuid,
    version_no: i32,
    up: &UploadedFile,
) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO sharepoint_document_folders (document_id, item_id, parent_item_id, name)
         VALUES ($1, $2, $3, $4) ON CONFLICT (document_id) DO NOTHING",
        document_id,
        up.folder.item_id,
        up.folder.parent_item_id,
        up.folder.name
    )
    .execute(&mut *conn)
    .await?;
    let inserted = sqlx::query!(
        "INSERT INTO sharepoint_files (document_version_id, document_id, drive_id, item_id, list_item_id, name,
                                       quick_xor_hash, size, last_known_path)
         SELECT v.id, $1, $3, $4, $5, $6, $7, $8, $9 FROM document_versions v
          WHERE v.document_id = $1 AND v.version_no = $2",
        document_id,
        version_no,
        up.drive_id,
        up.item_id,
        up.list_item_id,
        up.name,
        up.quick_xor_hash,
        i64::try_from(up.size).unwrap_or(i64::MAX),
        format!("{}/{}", up.folder.name, up.name)
    )
    .execute(&mut *conn)
    .await?;
    if inserted.rows_affected() != 1 {
        return Err(CoreError::internal("the new version was not found for its SharePoint file"));
    }
    Ok(())
}
```

In `mod.rs`:

- add `mod files;` above `mod folders;`;
- add `pub use files::{UploadTarget, UploadedFile};`, `pub use folders::DocumentFolder;` and `pub(crate) use files::{record_upload, upload_staged};`;
- add, after the `use` lines:

```rust
/// Shown when an upload cannot reach SharePoint.
pub const UPLOAD_UNAVAILABLE: &str =
    "File storage (SharePoint) is unavailable right now, so nothing was saved. Try again in a few minutes.";

/// Log the underlying failure and return the user-facing "unavailable" error.
pub(crate) fn unavailable(message: &'static str, e: impl std::fmt::Display) -> CoreError {
    tracing::error!(error = %e, "SharePoint request failed");
    CoreError::Unavailable(message.to_owned())
}
```

- [x] **Step 6: Implement the file-store router.** Create `crates/pdms-core/src/documents/filestore.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Where document files live: SharePoint (`[storage] backend = "sharepoint"`)
//! or the blob store (local disk or S3). Every read and write of a document
//! file goes through here; export packages always use `core.storage`.

use std::path::Path;

use pdms_authz::Visibility;
use sqlx::PgConnection;
use uuid::Uuid;

use super::files::StoredFile;
use crate::Core;
use crate::error::{CoreError, CoreResult};
use crate::sharepoint::{self, layout};

/// Where an accepted file was put, before its version is recorded.
#[derive(Debug)]
pub enum FileLocation {
    /// Content-addressed in `core.storage`; nothing more to record.
    Blob,
    SharePoint(Box<sharepoint::UploadedFile>),
}

/// The document a file belongs to.
pub struct PutTarget<'a> {
    /// `None` while the document is being created.
    pub document_id: Option<Uuid>,
    pub external_id: &'a str,
    pub title: &'a str,
    pub visibility: Visibility,
    pub department_id: Uuid,
}

/// Store an accepted upload (`src` is its spool file).
pub async fn put_version_file(
    core: &Core,
    target: &PutTarget<'_>,
    file: &StoredFile,
    src: &Path,
) -> CoreResult<FileLocation> {
    if core.sharepoint.is_none() {
        core.storage
            .put_file(file.sha256, src, file.size)
            .await
            .map_err(|e| CoreError::internal(format!("storing upload: {e}")))?;
        return Ok(FileLocation::Blob);
    }
    if file.size == 0 {
        return Err(CoreError::validation("This file is empty. Empty files can't be stored."));
    }
    let up = sharepoint::upload_staged(
        core,
        &sharepoint::UploadTarget {
            document_id: target.document_id,
            external_id: target.external_id,
            title: target.title,
            spot: layout::Spot::of(target.visibility, target.department_id, false),
        },
        src,
        file.size,
        &layout::extension_for(file.media_type, &file.file_name),
    )
    .await?;
    Ok(FileLocation::SharePoint(Box::new(up)))
}

/// Record where the file of `version_no` lives. Runs in the version's transaction.
pub(crate) async fn record_location(
    conn: &mut PgConnection,
    document_id: Uuid,
    version_no: i32,
    loc: &FileLocation,
) -> CoreResult<()> {
    match loc {
        FileLocation::Blob => Ok(()),
        FileLocation::SharePoint(up) => sharepoint::record_upload(conn, document_id, version_no, up).await,
    }
}

/// Recorded in `blobs.storage_backend`.
pub fn backend_name(core: &Core) -> &'static str {
    if core.sharepoint.is_some() {
        "sharepoint"
    } else {
        core.storage.backend()
    }
}
```

In `crates/pdms-core/src/documents/mod.rs`, add `pub mod filestore;` after `pub mod fields;`.

- [x] **Step 7: Stop storing in `ingest`.** In `crates/pdms-core/src/documents/files.rs`, delete this block from `ingest`:

```rust
    core.storage
        .put_file(upload.file.sha256, &upload.file.path, upload.file.size)
        .await
        .map_err(|e| CoreError::internal(format!("storing upload: {e}")))?;
```

Change the doc comment to `/// Validate and scan an upload. Rejections are audited. Storing it is filestore::put_version_file.` and the module doc to `//! Accepting an uploaded file: sniff its type and scan it.`

- [x] **Step 8: Route `create` through the file store.** In `crates/pdms-core/src/documents/service.rs`:

(a) Add `use super::filestore;` next to the other `super::` imports.

(b) In `create`, the document ID must be known before the file is stored (spec §7.1). Replace:

```rust
    let id = Uuid::now_v7();
```

with:

```rust
    let id = Uuid::now_v7();
    // The document ID is allocated before the file is stored, so the file can
    // be filed under it (spec §7.1). A number left unused by a failure is harmless.
    let prefix = sqlx::query_scalar!(
        "SELECT id_prefix FROM document_types WHERE id = $1",
        meta.document_type_id
    )
    .fetch_one(&core.db)
    .await?;
    let seq = sqlx::query_scalar!(r#"SELECT nextval('document_external_seq') AS "n!""#)
        .fetch_one(&core.db)
        .await?;
    let year = Utc::now()
        .with_timezone(&core.config.timezone())
        .format("%Y");
    let external_id = format!("{prefix}-{year}-{seq:06}");
    let location = match (&new.file, &stored) {
        (Some(u), Some(f)) => Some(
            filestore::put_version_file(
                core,
                &filestore::PutTarget {
                    document_id: None,
                    external_id: &external_id,
                    title: &meta.title,
                    visibility: new.visibility,
                    department_id: meta.department_id,
                },
                f,
                &u.file.path,
            )
            .await?,
        ),
        _ => None,
    };
```

(c) Delete the now-duplicate block directly after `let mut tx = core.db.begin().await?;`: the `let prefix = …`, `let seq = …`, `let year = …` and `let external_id = …` statements that used `&mut *tx`.

(d) Replace `files::record_blob(&mut tx, f, core.storage.backend()).await?;` with `files::record_blob(&mut tx, f, filestore::backend_name(core)).await?;`.

(e) Change the `append_version(` call in `create` to `let version = append_version(`. Directly after its `.await?;`, add:

```rust
    if let Some(loc) = &location {
        filestore::record_location(&mut tx, id, version, loc).await?;
    }
```

- [x] **Step 9: Route `add_version` through the file store.** In `add_version`, after `let stored = files::ingest(core, ctx, Some(id), &upload).await?;`, add:

```rust
    let location = filestore::put_version_file(
        core,
        &filestore::PutTarget {
            document_id: Some(id),
            external_id: &doc_ref.external_id,
            title: &doc_ref.title,
            visibility: doc_ref.attrs.visibility,
            department_id: doc_ref.attrs.department_id,
        },
        &stored,
        &upload.file.path,
    )
    .await?;
```

Then replace `files::record_blob(&mut tx, &stored, core.storage.backend()).await?;` with `files::record_blob(&mut tx, &stored, filestore::backend_name(core)).await?;`. After the `let version = append_version(…).await?;` statement, add:

```rust
    filestore::record_location(&mut tx, id, version, &location).await?;
```

- [x] **Step 10: Run the tests and confirm they pass, including the existing suites.**

Run: `cargo test -p pdms-core --test sharepoint && cargo test -p pdms-core --test documents && cargo test -p pdms-core --test phase4`
Expected: all `test result: ok.` The local-storage suites behave exactly as before, because `put_version_file` stores in `core.storage` when SharePoint isn't configured.

- [x] **Step 11: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core .sqlx
git commit -m "documents: store files through filestore; SharePoint uploads with QuickXorHash check"
```

---

### Task 7: Reading files back, verified

**Files:**
- Modify: `crates/pdms-core/src/sharepoint/files.rs` (`open`, verification, `flag_changed`)
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Modify: `crates/pdms-core/src/documents/filestore.rs` (`open`, `open_version_file`)
- Modify: `crates/pdms-core/src/documents/service.rs` (`open_file`)
- Modify: `crates/pdms-core/src/ocr/mod.rs` (text extraction read)
- Modify: `crates/pdms-core/src/exports.rs` (package file reads)
- Modify: `crates/pdms-core/src/integrity.rs`, `crates/pdms/src/main.rs`
- Test: `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Consumes: `sharepoint_files` rows (Task 6); P2's `GraphClient::download`.
- Produces:
  - `pub async fn filestore::open(core, document_id: Uuid, version_no: i32, hash: BlobHash, size: u64) -> CoreResult<ByteStream>`;
  - `pub async fn filestore::open_version_file(core, version_id: Uuid) -> CoreResult<ByteStream>` (index contract);
  - `pub(crate) async fn flag_changed(db, document_id, item_id, source, detail)` in `sharepoint::files` (P4 re-exports it);
  - `IntegrityReport.sharepoint_files_skipped: u64`;
  - audit action `sharepoint.changed_outside`.

- [x] **Step 1: Write the failing tests.** Add `use futures_util::StreamExt;` and `use pdms_core::exports;` to the imports of `tests/sharepoint.rs`, and add:

```rust
async fn read_all(mut s: pdms_storage::ByteStream) -> Result<Vec<u8>, std::io::Error> {
    let mut out = Vec::new();
    while let Some(c) = s.next().await {
        out.extend_from_slice(&c?);
    }
    Ok(out)
}

#[tokio::test]
async fn downloads_stream_from_sharepoint_and_are_audited() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = pdf("Fuel supply agreement");
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &bytes))).await;
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert_eq!(read_all(dl.stream).await.unwrap(), bytes);
    assert!(sp.requests().iter().any(|r| r.starts_with("GET /v1.0/drives/drive-1/items/") && r.ends_with("/content")));
    assert!(audit_actions(&core).await.contains(&"document.downloaded".to_owned()));
}

#[tokio::test]
async fn changed_files_stop_the_download_and_are_flagged() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("original")))).await;
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Fuel");
    let name = sp.names(&folder).pop().unwrap();
    sp.tamper(&format!("{folder}/{name}"), b"%PDF-1.4 edited directly in SharePoint");
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert!(read_all(dl.stream).await.is_err(), "the download must not complete");
    let state: String = sqlx::query_scalar("SELECT state FROM sharepoint_files WHERE document_id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(state, "changed_outside");
    assert!(audit_actions(&core).await.contains(&"sharepoint.changed_outside".to_owned()));
}

#[tokio::test]
async fn text_extraction_reads_the_file_from_sharepoint() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("Fuel supply agreement")))).await;
    pdms_core::ocr::extract(&core, id, 1, false).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status::text FROM document_texts WHERE document_id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await
        .unwrap();
    assert_eq!(status, "done");
}

#[tokio::test]
async fn records_packages_read_files_from_sharepoint() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("x")))).await;
    let officer = directory::create_user(&core.db, &system(), "Dana", "dana@x.org", Some(dept)).await.unwrap();
    let role = directory::role_id_by_key(&core.db, "records_officer").await.unwrap();
    directory::assign_role(&core.db, &system(), officer, role, None).await.unwrap();
    let ctx_o = as_user(&core, officer).await;
    let pkg = exports::build(
        &core,
        &ctx_o,
        exports::PackageRequest {
            request_reference: "REQ-2026-001".into(),
            document_ids: vec![id],
            fields: vec![],
            versions: exports::Versions::Current,
            include_audit: false,
            note: String::new(),
        },
    )
    .await
    .unwrap();
    // The package re-hashes every file against PADM's record while writing (D-067).
    exports::open(&core, &ctx_o, pkg).await.unwrap();
}

#[tokio::test]
async fn integrity_check_leaves_sharepoint_files_to_the_change_check() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("x")))).await;
    let r = pdms_core::integrity::verify(&core, true).await.unwrap();
    assert!(r.problems.is_empty(), "{} problems", r.problems.len());
    assert_eq!(r.sharepoint_files_skipped, 1);
}
```

> **As built:** `text_extraction_…` uploads a `.txt` file, because the test PDFs are not real enough for the PDF text reader (it fails them whatever the storage). `records_packages_…` reads the package back. `flag_changed` updates the state and writes the audit event in one transaction (pre-flight correction), and `IntegrityReport.files_checked` leaves out the skipped SharePoint files.

- [x] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --test sharepoint`
Expected:
- `downloads_stream_…`, `text_extraction_…` and `records_packages_…` FAIL with `reading blob: blob … not found`, because reads still go to the local store;
- `integrity_check_…` fails to compile: `no field sharepoint_files_skipped`.

- [x] **Step 3: Implement verified downloads.** Add these imports to `sharepoint/files.rs`:

```rust
use futures_util::StreamExt;
use pdms_audit::{Actor, AuditEvent, Category, Outcome};
use pdms_storage::{BlobHash, ByteStream};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
```

Change `use super::{UPLOAD_UNAVAILABLE, unavailable};` to `use super::{READ_UNAVAILABLE, UPLOAD_UNAVAILABLE, unavailable};`. Then append:

```rust
/// Stream a version's file from SharePoint, checked against the SHA-256 and
/// size PADM recorded. For a version that carried an earlier file forward,
/// the newest file at or before `version_no` is the one read. A mismatch ends
/// the stream with an error (the client sees a failed download), marks the
/// file `changed_outside`, and is audited.
pub(crate) async fn open(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    expected: BlobHash,
    size: u64,
) -> CoreResult<ByteStream> {
    let sp = core
        .sharepoint
        .as_ref()
        .ok_or_else(|| CoreError::internal("SharePoint storage is not configured"))?;
    let f = sqlx::query!(
        "SELECT sf.drive_id, sf.item_id FROM sharepoint_files sf
           JOIN document_versions v ON v.id = sf.document_version_id
          WHERE v.document_id = $1 AND v.version_no <= $2
          ORDER BY v.version_no DESC LIMIT 1",
        document_id,
        version_no
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or_else(|| {
        CoreError::internal(format!("document {document_id} v{version_no} has no file in SharePoint"))
    })?;
    let stream = sp
        .graph()
        .download(&format!("/drives/{}/items/{}/content", f.drive_id, f.item_id))
        .await
        .map_err(|e| unavailable(READ_UNAVAILABLE, e))?;
    Ok(verified(stream, core.db.clone(), document_id, f.item_id, expected, size))
}

struct Check {
    inner: ByteStream,
    hasher: Sha256,
    seen: u64,
    expected: BlobHash,
    size: u64,
    db: PgPool,
    document_id: Uuid,
    item_id: String,
    done: bool,
}

fn verified(
    inner: ByteStream,
    db: PgPool,
    document_id: Uuid,
    item_id: String,
    expected: BlobHash,
    size: u64,
) -> ByteStream {
    let state = Check {
        inner,
        hasher: Sha256::new(),
        seen: 0,
        expected,
        size,
        db,
        document_id,
        item_id,
        done: false,
    };
    Box::pin(futures_util::stream::unfold(state, |mut s| async move {
        if s.done {
            return None;
        }
        match s.inner.next().await {
            Some(Ok(chunk)) => {
                s.hasher.update(&chunk);
                s.seen += chunk.len() as u64;
                Some((Ok(chunk), s))
            }
            Some(Err(e)) => {
                s.done = true;
                Some((Err(e), s))
            }
            None => {
                s.done = true;
                let got: [u8; 32] = s.hasher.clone().finalize().into();
                if s.seen == s.size && got == s.expected.0 {
                    return None;
                }
                let detail = format!(
                    "read {} bytes with SHA-256 {}; PADM recorded {} bytes with {}",
                    s.seen,
                    hex::encode(got),
                    s.size,
                    s.expected.hex()
                );
                flag_changed(&s.db, s.document_id, &s.item_id, "download_check", &detail).await;
                Some((
                    Err(std::io::Error::other(
                        "the file in SharePoint was changed outside PADM; the download was stopped and an administrator was alerted",
                    )),
                    s,
                ))
            }
        }
    }))
}

/// Mark a SharePoint file as changed outside PADM and audit it. Failures to
/// record are logged; the caller has already refused the content.
pub(crate) async fn flag_changed(db: &PgPool, document_id: Uuid, item_id: &str, source: &str, detail: &str) {
    tracing::error!(%document_id, item_id, detail, "SharePoint file does not match PADM's record");
    let r = async {
        sqlx::query!(
            "UPDATE sharepoint_files SET state = 'changed_outside', checked_at = now() WHERE item_id = $1",
            item_id
        )
        .execute(db)
        .await?;
        pdms_audit::record(
            db,
            vec![
                AuditEvent::new(Category::System, "sharepoint.changed_outside", Actor::system("sharepoint"), "document")
                    .object(document_id, None)
                    .document(document_id)
                    .outcome(Outcome::Failure)
                    .context("source", source)
                    .context("item_id", item_id)
                    .context("detail", detail),
            ],
        )
        .await?;
        Ok::<(), CoreError>(())
    }
    .await;
    if let Err(e) = r {
        tracing::error!(error = %e, "could not record a SharePoint mismatch");
    }
}
```

In `mod.rs`:

- change `pub(crate) use files::{record_upload, upload_staged};` to `pub(crate) use files::{open, record_upload, upload_staged};` (`flag_changed` stays private to `files` until P4 uses it from the change check; an unused `pub(crate) use` would fail `-D warnings`);
- add the constant:

```rust
/// Shown when a download cannot reach SharePoint.
pub const READ_UNAVAILABLE: &str =
    "File storage (SharePoint) is unavailable right now. Try again in a few minutes.";
```

- [x] **Step 4: Add the read side of the file-store router.** Append to `documents/filestore.rs` (adding `use pdms_storage::{BlobHash, ByteStream};`):

```rust
/// Open the file current at `version_no`.
pub async fn open(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    hash: BlobHash,
    size: u64,
) -> CoreResult<ByteStream> {
    if core.sharepoint.is_some() {
        return sharepoint::open(core, document_id, version_no, hash, size).await;
    }
    core.storage
        .get(hash)
        .await
        .map_err(|e| CoreError::internal(format!("reading blob: {e}")))
}

/// Open the file of one `document_versions` row.
pub async fn open_version_file(core: &Core, version_id: Uuid) -> CoreResult<ByteStream> {
    let v = sqlx::query!(
        r#"SELECT v.document_id, v.version_no, v.blob_sha256 AS "sha!", b.size_bytes
             FROM document_versions v JOIN blobs b ON b.sha256 = v.blob_sha256
            WHERE v.id = $1"#,
        version_id
    )
    .fetch_optional(&core.db)
    .await?
    .ok_or(CoreError::NotFound)?;
    let hash = BlobHash::from_slice(&v.sha).ok_or_else(|| CoreError::internal("bad blob hash"))?;
    open(core, v.document_id, v.version_no, hash, u64::try_from(v.size_bytes).unwrap_or(0)).await
}
```

- [x] **Step 5: Rewire the readers.**

(a) `service.rs` `open_file`: replace

```rust
    let stream = core
        .storage
        .get(hash)
        .await
        .map_err(|e| CoreError::internal(format!("reading blob: {e}")))?;
```

with

```rust
    let stream = filestore::open(core, id, v.version_no, hash, u64::try_from(v.size_bytes).unwrap_or(0)).await?;
```

(b) `ocr/mod.rs` `extract`: replace `let mut s = core.storage.get(hash).await.map_err(CoreError::internal)?;` with:

```rust
                let mut s = crate::documents::filestore::open(
                    core,
                    document_id,
                    version_no,
                    hash,
                    u64::try_from(v.size_bytes).unwrap_or(0),
                )
                .await?;
```

An error here makes the job retry later, which is right when SharePoint is briefly down.

(c) `exports.rs`: replace the read inside the `for v in chosen` loop. It is the line indented 20 spaces; do **not** change the package read in `open` further down.

```rust
                    let stream = crate::documents::filestore::open(
                        core,
                        d.id,
                        v.version_no,
                        hash,
                        v.size_bytes.and_then(|s| u64::try_from(s).ok()).unwrap_or(0),
                    )
                    .await?;
```

(d) `integrity.rs`:

- add the field to `IntegrityReport`:

```rust
    /// Files stored in SharePoint. They are checked against SharePoint by the
    /// change check (P4), not re-read here.
    pub sharepoint_files_skipped: u64,
```

- in `verify`, change the blob query to `"SELECT sha256, size_bytes, storage_backend FROM blobs ORDER BY created_at"`;
- add `let mut sharepoint_files_skipped = 0u64;` before the loop, and make the loop's first statement:

```rust
        if b.storage_backend == "sharepoint" {
            sharepoint_files_skipped += 1;
            continue;
        }
```

- add `sharepoint_files_skipped,` to the `IntegrityReport { … }` literal that `verify` returns.

(e) `crates/pdms/src/main.rs`, in `VerifyIntegrity`'s non-JSON branch, after the `println!("Files: {} checked …")` statement:

```rust
                if r.sharepoint_files_skipped > 0 {
                    println!(
                        "SharePoint files: {} (checked by the SharePoint change check, not re-read here).",
                        r.sharepoint_files_skipped
                    );
                }
```

- [x] **Step 6: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint && cargo test -p pdms-core --test phase4 && cargo test -p pdms-core --test phase5`
Expected: all `test result: ok.`

- [x] **Step 7: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates .sqlx
git commit -m "documents: read files through filestore; SharePoint downloads verified by SHA-256"
```

---

### Task 8: Restoring an earlier file

In SharePoint mode, a restored file needs its own file for the new version, because each version has one file.

- **Decision: copy out, then upload again.** PADM downloads the earlier file (verified against its SHA-256) into the spool and runs the normal staged upload.
- **Why not Graph's server-side `copy`?** It is asynchronous: it returns a monitor URL that has to be polled, and the result would need a second verification. Restores are rare, so one code path with end-to-end verification is worth the extra transfer.

**Files:**
- Modify: `crates/pdms-core/src/sharepoint/files.rs` (`spool_copy`), `crates/pdms-core/src/sharepoint/mod.rs`
- Modify: `crates/pdms-core/src/documents/filestore.rs` (`copy_for_restore`, `known_media_type`)
- Modify: `crates/pdms-core/src/documents/service.rs` (`restore_version`)
- Test: `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Consumes: `open` (Task 7) and `put_version_file`, `record_location` (Task 6).
- Produces:
  - `pub(crate) async fn sharepoint::spool_copy(core, document_id, version_no, expected: BlobHash, size: u64) -> CoreResult<SpooledFile>`;
  - `pub(crate) async fn filestore::copy_for_restore(…)`, with the same arguments;
  - `pub fn filestore::known_media_type(&str) -> &'static str`.

- [x] **Step 1: Write the failing test.**

```rust
#[tokio::test]
async fn restoring_a_file_makes_a_new_file() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let original = pdf("original terms");
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &original))).await;
    documents::add_version(&core, &ctx, id, upload(&core, "b.pdf", &pdf("amended")).await, documents::VersionKind::Amendment, "", None)
        .await
        .unwrap();
    let v = documents::restore_version(&core, &ctx, id, 1, documents::RestorePart::File).await.unwrap();
    assert_eq!(v, 3);
    let ext = external_id(&core, id).await;
    assert_eq!(sp.names(&format!("Departments/Road and Bridge/{ext} – Fuel")).len(), 3);
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert_eq!(dl.version_no, 3);
    assert_eq!(read_all(dl.stream).await.unwrap(), original);
}
```

- [x] **Step 2: Run the test and confirm it fails.**

Run: `cargo test -p pdms-core --test sharepoint restoring_a_file`
Expected: FAIL. With no `sharepoint_files` row for version 3, the download returns version 2's bytes (the newest recorded file), so the content doesn't match. Only 2 files are in the folder.

- [x] **Step 3: Implement.** Append to `sharepoint/files.rs` (adding `use pdms_storage::spool::{SpoolWriter, SpooledFile};`):

```rust
/// Copy a version's file out of SharePoint into the spool directory,
/// verified, so it can be uploaded again as a new version.
pub(crate) async fn spool_copy(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    expected: BlobHash,
    size: u64,
) -> CoreResult<SpooledFile> {
    let mut stream = open(core, document_id, version_no, expected, size).await?;
    let mut w = SpoolWriter::create(&core.config.storage.spool_path, size.max(1))
        .await
        .map_err(CoreError::internal)?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            CoreError::Conflict(format!("The earlier file could not be read: {e}"))
        })?;
        w.write(&chunk).await.map_err(CoreError::internal)?;
    }
    let f = w.finish().await.map_err(CoreError::internal)?;
    if f.sha256 != expected {
        return Err(CoreError::internal("the copied file does not match its recorded SHA-256"));
    }
    Ok(f)
}
```

In `mod.rs`, add `spool_copy` to the `pub(crate) use files::{…}` list.

Append to `documents/filestore.rs` (adding `use pdms_storage::spool::SpooledFile;`):

```rust
/// A verified local copy of an earlier version's file (SharePoint mode).
pub(crate) async fn copy_for_restore(
    core: &Core,
    document_id: Uuid,
    version_no: i32,
    hash: BlobHash,
    size: u64,
) -> CoreResult<SpooledFile> {
    sharepoint::spool_copy(core, document_id, version_no, hash, size).await
}

/// The allowlisted media type matching `s`, as the `&'static str` that
/// [`StoredFile`] holds ("" if not allowlisted).
pub fn known_media_type(s: &str) -> &'static str {
    pdms_storage::sniff::ALLOWED
        .iter()
        .find(|(m, _)| *m == s)
        .map_or("", |(m, _)| *m)
}
```

In `service.rs` `restore_version`, before `let mut tx = core.db.begin().await?;`, add:

```rust
    // With SharePoint, the restored file becomes a new file in the document's
    // folder: copy the earlier one out (verified) and upload it again.
    let restored = match (
        part,
        core.sharepoint.is_some(),
        old.blob_sha256.as_deref(),
        old.file_name.as_deref(),
    ) {
        (RestorePart::File, true, Some(sha), Some(name)) => {
            let hash = BlobHash::from_slice(sha).ok_or_else(|| CoreError::internal("bad blob hash"))?;
            let size = u64::try_from(old.size_bytes.unwrap_or(0)).unwrap_or(0);
            let copy = filestore::copy_for_restore(core, id, version_no, hash, size).await?;
            Some(
                filestore::put_version_file(
                    core,
                    &filestore::PutTarget {
                        document_id: Some(id),
                        external_id: &doc_ref.external_id,
                        title: &doc_ref.title,
                        visibility: doc_ref.attrs.visibility,
                        department_id: doc_ref.attrs.department_id,
                    },
                    &StoredFile {
                        sha256: hash,
                        size,
                        media_type: filestore::known_media_type(old.media_type.as_deref().unwrap_or("")),
                        file_name: name.to_owned(),
                        scan: "previously_accepted",
                    },
                    &copy.path,
                )
                .await?,
            )
        }
        _ => None,
    };
```

In the `RestorePart::File` arm, after `let v = append_version(…).await?;`, add:

```rust
            if let Some(loc) = &restored {
                filestore::record_location(&mut tx, id, v, loc).await?;
            }
```

- [x] **Step 4: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint && cargo test -p pdms-core --test documents`
Expected: all `test result: ok.`

- [x] **Step 5: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core .sqlx
git commit -m "documents: restoring a file in SharePoint uploads a verified copy as a new version"
```

---

### Task 9: The mirror job

This job gives each file its final name and sets its columns. It also moves and renames folders and marks the newest file `Current`. Every document change queues it, and a nightly sweep catches renames made elsewhere: departments, types, tags, and owner names.

**Files:**
- Create: `crates/pdms-core/src/sharepoint/mirror.rs`
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Modify: `crates/pdms-core/src/maintenance.rs` (register jobs)
- Modify: `crates/pdms-core/src/documents/service.rs`, `crates/pdms-core/src/workflow.rs`, `crates/pdms-core/src/reviews.rs`, `crates/pdms-core/src/ocr/mod.rs` (queue the mirror)
- Test: `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Consumes: `department_folder_id`, `move_item`, and `token` (Tasks 5–6); `layout::{columns, file_name, document_folder_name, …}`; `crate::documents::fetch`.
- Produces:
  - `pub const MIRROR_JOB: &str = "sharepoint.mirror_document"` and `pub const MIRROR_SWEEP_JOB: &str = "sharepoint.mirror_sweep"`;
  - `pub async fn enqueue_mirror<'e>(exec: impl PgExecutor<'e>, document_id: Uuid) -> CoreResult<()>` (index contract): queues one job unless one is already pending;
  - `pub(crate) async fn mirror_later(core, conn: &mut PgConnection, document_id) -> CoreResult<()>`: a no-op without SharePoint;
  - `pub async fn mirror_document(core, document_id) -> CoreResult<()>`;
  - `pub async fn mirror_sweep(core) -> CoreResult<u64>`;
  - `pub fn register_jobs(worker: Worker, core: &Core) -> Worker`.

- [ ] **Step 1: Write the failing tests.**

```rust
async fn pending_mirrors(core: &Core, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'sharepoint.mirror_document' AND status = 'pending'
           AND payload->>'document_id' = $1",
    )
    .bind(id.to_string())
    .fetch_one(&core.db)
    .await
    .unwrap()
}

async fn clear_mirrors(core: &Core) {
    sqlx::query("DELETE FROM jobs WHERE kind = 'sharepoint.mirror_document'")
        .execute(&core.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_mirror_names_files_and_fills_columns() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel Supply Agreement", Visibility::Department, Some(("a.pdf", &pdf("v1")))).await;
    documents::add_version(&core, &ctx, id, upload(&core, "b.pdf", &pdf("v2")).await, documents::VersionKind::Amendment, "", None)
        .await
        .unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "one pending job, however many changes");
    sharepoint::mirror_document(&core, id).await.unwrap();
    let ext = external_id(&core, id).await;
    let folder = format!("Departments/Road and Bridge/{ext} – Fuel Supply Agreement");
    assert_eq!(
        sp.names(&folder),
        vec![format!("{ext} v1 – Original.pdf"), format!("{ext} v2 – Amendment.pdf")]
    );
    let v1 = sp.fields(&format!("{folder}/{ext} v1 – Original.pdf"));
    let v2 = sp.fields(&format!("{folder}/{ext} v2 – Amendment.pdf"));
    assert_eq!(v1["PadmCurrent"], serde_json::json!(false));
    assert_eq!(v2["PadmCurrent"], serde_json::json!(true));
    assert_eq!(v2["PadmDocumentId"], serde_json::json!(ext));
    assert_eq!(v2["PadmDepartment"], serde_json::json!("Road and Bridge"));
    assert_eq!(v2["PadmVisibility"], serde_json::json!("Department"));
    assert!(v2["PadmLink"].as_str().unwrap().ends_with(&format!("/documents/{id}")));
    // Running it again changes nothing in SharePoint.
    let before = sp.requests().len();
    sharepoint::mirror_document(&core, id).await.unwrap();
    let patches = sp.requests()[before..].iter().filter(|r| r.starts_with("PATCH")).count();
    assert_eq!(patches, 0);
}

#[tokio::test]
async fn retitling_renames_the_document_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("v1")))).await;
    let (doc, _) = documents::for_edit(&core, &ctx, id).await.unwrap();
    let input = MetadataInput {
        title: "Fuel Supply Agreement".into(),
        document_type_id: doc.document_type_id,
        department_id: doc.department_id,
        ..Default::default()
    };
    documents::update(&core, &ctx, id, input, doc.row_version).await.unwrap();
    sharepoint::mirror_document(&core, id).await.unwrap();
    let ext = external_id(&core, id).await;
    assert_eq!(sp.names("Departments/Road and Bridge"), vec![format!("{ext} – Fuel Supply Agreement")]);
}

#[tokio::test]
async fn every_kind_of_change_queues_the_mirror() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let bob = directory::create_user(&core.db, &system(), "Bob", "bob@x.org", Some(dept)).await.unwrap();
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("v1")))).await;
    clear_mirrors(&core).await;

    documents::add_tag(&core, &ctx, id, "Roads").await.unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "tag added");
    clear_mirrors(&core).await;

    documents::change_status(&core, &ctx, id, DocumentStatus::Archived, "").await.unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "status");
    clear_mirrors(&core).await;

    documents::transfer_owner(&core, &system(), id, bob, "handover").await.unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "owner");
    clear_mirrors(&core).await;

    documents::set_visibility(&core, &as_user(&core, bob).await, id, Visibility::CountyWide).await.unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "visibility");
    clear_mirrors(&core).await;

    documents::delete(&core, &system(), id, "duplicate").await.unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "deleted");
    clear_mirrors(&core).await;

    documents::restore_deleted(&core, &system(), id).await.unwrap();
    assert_eq!(pending_mirrors(&core, id).await, 1, "restored");
}

#[tokio::test]
async fn the_nightly_sweep_queues_every_document_once() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let a = new_doc(&core, &ctx, dept, alice, "A", Visibility::Department, Some(("a.pdf", &pdf("a")))).await;
    let b = new_doc(&core, &ctx, dept, alice, "B", Visibility::Department, Some(("b.pdf", &pdf("b")))).await;
    new_doc(&core, &ctx, dept, alice, "No file", Visibility::Department, None).await;
    clear_mirrors(&core).await;
    assert_eq!(sharepoint::mirror_sweep(&core).await.unwrap(), 2);
    assert_eq!(sharepoint::mirror_sweep(&core).await.unwrap(), 0, "already pending");
    assert_eq!(pending_mirrors(&core, a).await + pending_mirrors(&core, b).await, 2);
}
```

- [ ] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --test sharepoint mirror`
Expected: compile errors `cannot find function mirror_document in module sharepoint`.

- [ ] **Step 3: Implement the job.** Create `crates/pdms-core/src/sharepoint/mirror.rs`:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Keeping SharePoint in step with PADM: folder placement, file names, and
//! column values (spec §7.3). PADM is authoritative. The job is idempotent and
//! convergent: running it again after a crash or a manual change in
//! SharePoint restores the desired state.

use std::time::Duration;

use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use super::folders::{department_folder_id, move_item};
use super::layout::{self, PlacementDoc, PlacementFile, Spot};
use crate::Core;
use crate::documents::VersionKind;
use crate::error::{CoreError, CoreResult};
use crate::jobs::Worker;

pub const MIRROR_JOB: &str = "sharepoint.mirror_document";
pub const MIRROR_SWEEP_JOB: &str = "sharepoint.mirror_sweep";

/// Queue a mirror run for the document unless one is already pending.
/// (`jobs.idempotency_key` is unique across all job states, so it can't be
/// used to deduplicate a job that runs many times.)
pub async fn enqueue_mirror<'e>(exec: impl PgExecutor<'e>, document_id: Uuid) -> CoreResult<()> {
    sqlx::query!(
        "INSERT INTO jobs (id, kind, payload, max_attempts)
         SELECT $1, 'sharepoint.mirror_document', jsonb_build_object('document_id', $2::uuid), 10
          WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE kind = 'sharepoint.mirror_document'
                              AND status = 'pending' AND payload->>'document_id' = $3)",
        Uuid::now_v7(),
        document_id,
        document_id.to_string()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Call inside every transaction that changes something shown in SharePoint,
/// before the audit append. Does nothing unless SharePoint is the backend.
pub(crate) async fn mirror_later(core: &Core, conn: &mut PgConnection, document_id: Uuid) -> CoreResult<()> {
    if core.sharepoint.is_some() {
        enqueue_mirror(&mut *conn, document_id).await?;
    }
    Ok(())
}

/// Queue a mirror run for every document with files in SharePoint. Picks up
/// renamed departments, types, tags, and people. Returns how many were queued.
pub async fn mirror_sweep(core: &Core) -> CoreResult<u64> {
    if core.sharepoint.is_none() {
        return Ok(0);
    }
    let r = sqlx::query!(
        "INSERT INTO jobs (id, kind, payload, max_attempts)
         SELECT gen_random_uuid(), 'sharepoint.mirror_document', jsonb_build_object('document_id', f.document_id), 10
           FROM sharepoint_document_folders f
          WHERE NOT EXISTS (SELECT 1 FROM jobs j WHERE j.kind = 'sharepoint.mirror_document'
                              AND j.status = 'pending' AND j.payload->>'document_id' = f.document_id::text)"
    )
    .execute(&core.db)
    .await?;
    Ok(r.rows_affected())
}

/// Eight characters of an ID, for stable fallback names.
fn short(id: Uuid) -> String {
    id.simple().to_string().chars().take(8).collect()
}

/// Bring one document's folder and files in SharePoint in line with PADM.
pub async fn mirror_document(core: &Core, document_id: Uuid) -> CoreResult<()> {
    let Some(sp) = core.sharepoint.as_ref() else {
        return Ok(());
    };
    let mut conn = core.db.acquire().await?;
    let Some(doc) = crate::documents::fetch(&mut conn, document_id).await? else {
        return Ok(());
    };
    let Some(folder) = sqlx::query!(
        "SELECT item_id, parent_item_id, name FROM sharepoint_document_folders WHERE document_id = $1",
        document_id
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(()); // no files in SharePoint yet
    };
    let files = sqlx::query!(
        r#"SELECT sf.document_version_id, sf.item_id, sf.list_item_id, sf.name, sf.fields_hash,
                  v.version_no, v.kind AS "kind: VersionKind", v.file_name AS "file_name!", b.media_type
             FROM sharepoint_files sf
             JOIN document_versions v ON v.id = sf.document_version_id
             JOIN blobs b ON b.sha256 = v.blob_sha256
            WHERE sf.document_id = $1
            ORDER BY v.version_no"#,
        document_id
    )
    .fetch_all(&mut *conn)
    .await?;
    drop(conn);
    let r = sp.resolved().await?;
    let g = sp.graph();

    // 1. The folder: the right area, department, and name.
    let spot = Spot::of(doc.visibility, doc.department_id, doc.deleted_at.is_some());
    let parent = department_folder_id(core, spot.area, spot.department_id).await?;
    let want = layout::document_folder_name(&doc.external_id, &doc.title);
    let mut folder_name = folder.name.clone();
    if folder.parent_item_id != parent || folder.name != want {
        let fallback = layout::document_folder_name(&doc.external_id, &format!("{} {}", doc.title, short(document_id)));
        let moving = (folder.parent_item_id != parent).then_some(parent.as_str());
        folder_name = move_item(g, &r.drive_id, &folder.item_id, moving, &want, &fallback).await?;
        sqlx::query!(
            "UPDATE sharepoint_document_folders SET parent_item_id = $2, name = $3, updated_at = now()
              WHERE document_id = $1",
            document_id,
            parent,
            folder_name
        )
        .execute(&core.db)
        .await?;
    }

    // 2. Files: final names, `Current` on the newest, column values.
    let newest = files.iter().map(|f| f.version_no).max();
    let tags: Vec<String> = doc.tags.iter().map(|t| t.name.clone()).collect();
    let link = format!(
        "{}/documents/{}",
        core.config.server.public_url.as_str().trim_end_matches('/'),
        doc.id
    );
    let placement = PlacementDoc {
        external_id: &doc.external_id,
        title: &doc.title,
        type_name: &doc.type_name,
        tags: &tags,
        status: doc.status.label(),
        visibility: doc.visibility,
        department: &doc.department_name,
        owner: &doc.owner_name,
        effective_date: doc.effective_date,
        review_date: doc.next_review_date,
        renewal_date: doc.renewal_date,
        expiration_date: doc.expiration_date,
        link: &link,
    };
    for f in &files {
        let ext = layout::extension_for(&f.media_type, &f.file_name);
        let label = layout::version_label(f.kind);
        let want = layout::file_name(&doc.external_id, f.version_no, label, &ext);
        let mut name = f.name.clone();
        if name != want {
            let fallback = layout::file_name(
                &doc.external_id,
                f.version_no,
                &format!("{label} {}", short(f.document_version_id)),
                &ext,
            );
            name = move_item(g, &r.drive_id, &f.item_id, None, &want, &fallback).await?;
        }
        let fields = layout::columns(
            &placement,
            &PlacementFile {
                version_no: f.version_no,
                current: Some(f.version_no) == newest,
            },
        );
        let hash = hex::encode(Sha256::digest(serde_json::to_vec(&fields).map_err(CoreError::internal)?));
        if f.fields_hash.as_deref() != Some(hash.as_str()) {
            let _: serde_json::Value = g
                .patch(
                    &format!("/sites/{}/lists/{}/items/{}/fields", r.site_id, r.list_id, f.list_item_id),
                    &fields,
                )
                .await?;
        }
        sqlx::query!(
            "UPDATE sharepoint_files SET name = $2, fields_hash = $3, last_known_path = $4
              WHERE document_version_id = $1",
            f.document_version_id,
            name,
            hash,
            format!("{folder_name}/{name}")
        )
        .execute(&core.db)
        .await?;
    }
    Ok(())
}

/// Add the SharePoint jobs to the worker (only when SharePoint is the backend).
pub fn register_jobs(worker: Worker, core: &Core) -> Worker {
    if core.sharepoint.is_none() {
        return worker;
    }
    let (c1, c2) = (core.clone(), core.clone());
    worker
        .handle(MIRROR_JOB, move |job| {
            let core = c1.clone();
            async move {
                let id: Uuid = job
                    .payload
                    .get("document_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
                    .ok_or("missing document_id")?;
                mirror_document(&core, id).await.map_err(|e| e.to_string())
            }
        })
        .handle(MIRROR_SWEEP_JOB, move |_| {
            let core = c2.clone();
            async move {
                let n = mirror_sweep(&core).await.map_err(|e| e.to_string())?;
                tracing::info!(documents = n, "SharePoint mirror sweep queued");
                Ok(())
            }
        })
        .every(MIRROR_SWEEP_JOB, Duration::from_secs(24 * 3600))
}
```

In `mod.rs`, add `mod mirror;` above `pub mod quickxor;`, and:

```rust
pub use mirror::{
    MIRROR_JOB, MIRROR_SWEEP_JOB, enqueue_mirror, mirror_document, mirror_sweep, register_jobs,
};
pub(crate) use mirror::mirror_later;
```

- [ ] **Step 4: Register the jobs in the worker.** In `crates/pdms-core/src/maintenance.rs` `worker()`, the function's body ends in one long builder chain that starts with `worker` (the variable) and ends with `.every(API_LOG_PURGE, Duration::from_secs(24 * 3600))`. P2 may have added handlers to this chain. Whatever its current form:

1. Bind the whole chain to a variable: change the leading `worker` of the chain to `let worker = worker`.
2. Put a `;` after its final `.every(…)`.
3. End the function with:

```rust
    crate::sharepoint::register_jobs(worker, core)
```

- [ ] **Step 5: Queue the mirror at every change.** In each function below, add `crate::sharepoint::mirror_later(core, &mut tx, <document id>).await?;` immediately **before** the `pdms_audit::append(…)` that precedes the final `tx.commit()`. The audit write must stay last (D-004).

`crates/pdms-core/src/documents/service.rs`. The document ID variable is `id` in every function here:

- `create`, `update`, `change_status`, `transfer_owner`, `set_visibility`;
- `delete`, `restore_deleted`;
- `add_tag`, `remove_tag`;
- `add_version`, `restore_version` (once, before the final `pdms_audit::append(&mut tx, events)`).

`crates/pdms-core/src/workflow.rs`:

- `submit`: before `pdms_audit::append(&mut tx, events).await?;` (the one directly followed by `notify_stage(&mut tx, core, ctx, &doc, id, 1)`), using `doc.id()`.
- `decide_step`: before `if !events.is_empty() {` (the block that ends just before `tx.commit()`), using `doc.id()`.
- `cancel`: before the `pdms_audit::append(` whose list starts with `status_event,`, using `document_id`.

`crates/pdms-core/src/reviews.rs`, in the task-completion function that calls `documents::apply_schedule_change`: before its final `pdms_audit::append(` (just before `tx.commit()`), using `t.document_id`.

`crates/pdms-core/src/ocr/mod.rs` `accept`: before the `pdms_audit::append(` whose list starts with `change,`, using `doc_id`.

- [ ] **Step 6: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint && cargo test -p pdms-core --test workflow && cargo test -p pdms-core --test notifications && cargo test -p pdms-core --test phase5`
Expected: all `test result: ok.` Without SharePoint, `mirror_later` queues nothing, so the other suites are unchanged.

- [ ] **Step 7: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core .sqlx
git commit -m "sharepoint: mirror job for names, columns, and folder placement; queued on every change"
```

---

### Task 10: Moves that narrow access happen first

Some changes must move the folder **before** the change is saved, and refuse the change if SharePoint can't be reached:

- tightening visibility;
- transferring a department-visible document to another department;
- deleting a document.

Loosening changes wait for the mirror job (spec §7.3). If the database change fails after the move, SharePoint is narrower than PADM, which is the safe direction; the next mirror run or the nightly sweep puts the folder back.

**Files:**
- Modify: `crates/pdms-core/src/sharepoint/mirror.rs` (`apply_placement_now`)
- Modify: `crates/pdms-core/src/sharepoint/mod.rs`
- Modify: `crates/pdms-core/src/documents/service.rs` (`set_visibility`, `update`, `delete`)
- Test: `crates/pdms-core/tests/sharepoint.rs`

**Interfaces:**
- Consumes: `layout::narrows`, `Spot`, `department_folder_id`, `move_item`.
- Produces:
  - `pub async fn apply_placement_now(core: &Core, document_id: Uuid, to: Spot) -> CoreResult<()>`. This deviates from the index: it takes the target, because it runs before the change is saved;
  - the constant `MOVE_UNAVAILABLE`.

- [ ] **Step 1: Write the failing tests.**

```rust
#[tokio::test]
async fn tightening_visibility_moves_the_folder_before_saving() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::CountyWide, Some(("a.pdf", &pdf("x")))).await;
    let ext = external_id(&core, id).await;
    assert_eq!(sp.names("County-wide/Road and Bridge"), vec![format!("{ext} – Fuel")]);
    documents::set_visibility(&core, &ctx, id, Visibility::Restricted).await.unwrap();
    // No mirror job has run: the move happened as part of the change.
    assert!(sp.names("County-wide/Road and Bridge").is_empty());
    assert_eq!(sp.names("Restricted/Road and Bridge"), vec![format!("{ext} – Fuel")]);
}

#[tokio::test]
async fn tightening_is_refused_when_sharepoint_is_down() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::CountyWide, Some(("a.pdf", &pdf("x")))).await;
    sp.fail("PATCH /drives/drive-1/items/");
    let err = documents::set_visibility(&core, &ctx, id, Visibility::Restricted).await.unwrap_err();
    assert!(matches!(&err, CoreError::Unavailable(m) if m.contains("the change was not made")), "{err:?}");
    let doc = documents::view(&core, &ctx, id).await.unwrap().document;
    assert_eq!(doc.visibility, Visibility::CountyWide, "PADM unchanged");
    assert_eq!(sp.names("County-wide/Road and Bridge").len(), 1, "folder unchanged");
}

#[tokio::test]
async fn loosening_waits_for_the_mirror_job() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("x")))).await;
    documents::set_visibility(&core, &ctx, id, Visibility::CountyWide).await.unwrap();
    assert_eq!(sp.names("Departments/Road and Bridge").len(), 1, "not moved yet");
    sharepoint::mirror_document(&core, id).await.unwrap();
    assert!(sp.names("Departments/Road and Bridge").is_empty());
    assert_eq!(sp.names("County-wide/Road and Bridge").len(), 1);
}

#[tokio::test]
async fn department_transfer_moves_the_folder_now() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ops = new_department(&core, "OPS", "Operations").await;
    acl::grant(&core.db, &system(), Subject::User(alice), Scope::Department(ops), Permission::Create, None, "test")
        .await
        .unwrap();
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("x")))).await;
    let (doc, _) = documents::for_edit(&core, &ctx, id).await.unwrap();
    let input = MetadataInput {
        title: doc.title.clone(),
        document_type_id: doc.document_type_id,
        department_id: ops,
        ..Default::default()
    };
    documents::update(&core, &ctx, id, input, doc.row_version).await.unwrap();
    assert!(sp.names("Departments/Road and Bridge").is_empty());
    assert_eq!(sp.names("Departments/Operations").len(), 1);
}

#[tokio::test]
async fn deleted_documents_leave_the_department_folder() {
    let sp = MockGraph::start().await;
    let (core, _db) = sp_core(&sp).await;
    let (dept, alice) = editing_department(&core, "RB", "Road and Bridge", "alice@x.org").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc(&core, &ctx, dept, alice, "Fuel", Visibility::Department, Some(("a.pdf", &pdf("x")))).await;
    documents::delete(&core, &system(), id, "duplicate").await.unwrap();
    assert!(sp.names("Departments/Road and Bridge").is_empty());
    assert_eq!(sp.names("Restricted/Road and Bridge").len(), 1);
    documents::restore_deleted(&core, &system(), id).await.unwrap();
    sharepoint::mirror_document(&core, id).await.unwrap();
    assert_eq!(sp.names("Departments/Road and Bridge").len(), 1);
}
```

- [ ] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-core --test sharepoint -- tightening loosening department_transfer deleted_documents`
Expected:
- `tightening_visibility_…`, `department_transfer_…` and `deleted_documents_…` FAIL: the folder hasn't moved before the mirror runs.
- `tightening_is_refused_…` FAILS because the change succeeds.
- `loosening_waits_…` already PASSES.

- [ ] **Step 3: Implement `apply_placement_now`.** Append to `mirror.rs`:

```rust
/// Move the document's folder to `to` now, before the change that requires
/// it is saved. Used when a change narrows who can browse the document
/// (`layout::narrows`). If SharePoint can't be reached, the error tells the
/// user the change was not made.
pub async fn apply_placement_now(core: &Core, document_id: Uuid, to: Spot) -> CoreResult<()> {
    let Some(sp) = core.sharepoint.as_ref() else {
        return Ok(());
    };
    let Some(folder) = sqlx::query!(
        "SELECT item_id, parent_item_id, name FROM sharepoint_document_folders WHERE document_id = $1",
        document_id
    )
    .fetch_optional(&core.db)
    .await?
    else {
        return Ok(()); // no files in SharePoint
    };
    let run = async {
        let r = sp.resolved().await?;
        let parent = department_folder_id(core, to.area, to.department_id).await?;
        if parent == folder.parent_item_id {
            return Ok::<(), CoreError>(());
        }
        let fallback = format!(
            "{} {}",
            layout::sanitize_segment(&folder.name, layout::FOLDER_MAX - 9),
            short(document_id)
        );
        let name = move_item(sp.graph(), &r.drive_id, &folder.item_id, Some(parent.as_str()), &folder.name, &fallback).await?;
        sqlx::query!(
            "UPDATE sharepoint_document_folders SET parent_item_id = $2, name = $3, updated_at = now()
              WHERE document_id = $1",
            document_id,
            parent,
            name
        )
        .execute(&core.db)
        .await?;
        Ok(())
    };
    run.await.map_err(|e| match e {
        // Graph failures (including conflicts) arrive as Internal or Conflict (P2's
        // mapping); people see the plain "unavailable" message, the detail is logged.
        e @ (CoreError::Validation(_) | CoreError::NotFound) => e,
        other => super::unavailable(super::MOVE_UNAVAILABLE, other),
    })
}
```

In `mod.rs`, add `apply_placement_now` to the `pub use mirror::{…}` list, plus:

```rust
/// Shown when a change that must move files first cannot reach SharePoint.
pub const MOVE_UNAVAILABLE: &str =
    "File storage (SharePoint) is unavailable right now, so the change was not made. Try again in a few minutes.";
```

- [ ] **Step 4: Call it from the three narrowing changes.** In `service.rs` (with `use crate::sharepoint::{self, layout::{Spot, narrows}};` added to the imports):

(a) `set_visibility`: directly after the `if from == to { return Ok(()); }` block, add:

```rust
    if core.sharepoint.is_some() {
        let dept = doc_ref.attrs.department_id;
        let target = Spot::of(to, dept, false);
        if narrows(Spot::of(from, dept, false), target) {
            sharepoint::apply_placement_now(core, id, target).await?;
        }
    }
```

(b) `update`: directly before `let mut tx = core.db.begin().await?;`, add:

```rust
    if core.sharepoint.is_some() {
        let v = before_doc.visibility;
        let target = Spot::of(v, meta.department_id, false);
        if narrows(Spot::of(v, before_doc.department_id, false), target) {
            sharepoint::apply_placement_now(core, id, target).await?;
        }
    }
```

(c) `delete`: directly before `let mut tx = core.db.begin().await?;`, add:

```rust
    if core.sharepoint.is_some() {
        let a = &doc_ref.attrs;
        let target = Spot::of(a.visibility, a.department_id, true);
        if narrows(Spot::of(a.visibility, a.department_id, false), target) {
            sharepoint::apply_placement_now(core, id, target).await?;
        }
    }
```

- [ ] **Step 5: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test sharepoint && cargo test -p pdms-core --test documents`
Expected: all `test result: ok.`

- [ ] **Step 6: Commit.**

```bash
cargo sqlx prepare --workspace -- --all-targets
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core .sqlx
git commit -m "sharepoint: narrowing moves happen before the change is saved"
```

---

### Task 11: Example configuration, live test, records, and full check

**Files:**
- Modify: `config/pdms.example.toml`
- Modify: `crates/pdms-core/tests/sharepoint.rs` (ignored live test)
- Modify: `docs/superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md` (record the P3 deviations)
- Modify: `docs/ROADMAP.md` §0.3 (sub-project 1 status)

**Interfaces:**
- Consumes: everything above.
- Produces: documentation only.

- [ ] **Step 1: Example configuration.** In `config/pdms.example.toml`, change the `backend` line of `[storage]` to:

```toml
backend = "local"               # "local", "s3", or "sharepoint" (document files in SharePoint; needs [graph] and [sharepoint])
```

and add after the `[storage.s3]` block:

```toml
[sharepoint]                    # used when [storage] backend = "sharepoint"; export packages stay in local_path
site_url = ""                   # e.g. "https://contoso.sharepoint.com/sites/PADMRecords"
library = "Documents"           # the document library (drive) name
delta_interval_minutes = 5      # change check interval (P4)
upload_chunk_bytes = 10485760   # 10 MiB; a multiple of 327680 (320 KiB), at most 62914560
```

- [ ] **Step 2: Add the opt-in live test.** Append to `tests/sharepoint.rs`:

```rust
/// Round trip against a real SharePoint **test** site. Not run by default.
///
/// ```text
/// PDMS_LIVE_TENANT_ID=… PDMS_LIVE_CLIENT_ID=… PDMS_LIVE_CERTIFICATE_FILE=/path/graph.pem \
/// PDMS_LIVE_SITE_URL=https://<tenant>.sharepoint.com/sites/PADMRecordsTest \
/// cargo test -p pdms-core --test sharepoint live_sharepoint_round_trip -- --ignored --nocapture
/// ```
///
/// It leaves one document folder in the site's `Restricted` area; the
/// printed path says where.
#[tokio::test]
#[ignore = "needs a real SharePoint test site and app registration"]
async fn live_sharepoint_round_trip() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let graph = pdms_core::config::GraphConfig {
        tenant_id: var("PDMS_LIVE_TENANT_ID"),
        client_id: var("PDMS_LIVE_CLIENT_ID"),
        certificate: pdms_core::config::Secret::new(
            std::fs::read_to_string(var("PDMS_LIVE_CERTIFICATE_FILE")).unwrap(),
        ),
        ..Default::default()
    };
    let site = var("PDMS_LIVE_SITE_URL");
    let (core, _db) = core_with(|c| {
        c.graph = graph;
        c.storage.backend = StorageBackend::SharePoint;
        c.sharepoint.site_url = site;
    })
    .await;
    let code = format!("LIVE{}", Uuid::new_v4().simple().to_string().chars().take(6).collect::<String>().to_uppercase());
    let (dept, alice) = editing_department(&core, &code, &format!("PADM live test {code}"), "live@x.org").await;
    let ctx = as_user(&core, alice).await;
    let bytes = pdf("PADM live SharePoint test");
    let id = new_doc(&core, &ctx, dept, alice, "Live test", Visibility::Department, Some(("live.pdf", &bytes))).await;
    sharepoint::mirror_document(&core, id).await.unwrap();
    let dl = documents::open_file(&core, &ctx, id, None).await.unwrap();
    assert_eq!(read_all(dl.stream).await.unwrap(), bytes);
    documents::set_visibility(&core, &ctx, id, Visibility::Restricted).await.unwrap();
    println!("left in SharePoint: Restricted/PADM live test {code}/{} – Live test", external_id(&core, id).await);
}
```

- [ ] **Step 3: Record the deviations from the index.** In `docs/superpowers/plans/2026-10-14-padm-sharepoint-entra-index.md`, update the "Produced by P3" table:

- the `documents::filestore` row becomes: `put_version_file(core, &PutTarget, &StoredFile, src: &Path) -> CoreResult<FileLocation>`; `open(core, document_id, version_no, hash, size) -> CoreResult<ByteStream>`; `open_version_file(core, version_id) -> CoreResult<ByteStream>`; `record_location`, `backend_name`, `copy_for_restore`, `known_media_type`;
- the Hook row becomes `apply_placement_now(core, document_id, to: Spot)`. It is called by `set_visibility`, by `update` (department change), and by `delete` whenever `layout::narrows(from, to)`;
- add rows for:
  - `SharePoint::resolved()` (lazy, retried after failure);
  - `MIRROR_SWEEP_JOB` (nightly) and `mirror_later`;
  - the tables `sharepoint_department_folders` and `sharepoint_document_folders`;
  - the extra `sharepoint_files` columns `list_item_id`, `name`, `fields_hash`;
  - `CoreError::Unavailable` (web and API 503);
  - `sharepoint::flag_changed` (P4 reuses it for delta findings).

- [ ] **Step 4: Update the roadmap.** In `docs/ROADMAP.md` §0.3, set sub-project 1's status to "**In progress**: P1 and P3 done; P4 (access, change detection, setup) next". If P4 is also done by then, set it to "Done".

- [ ] **Step 5: Full verification.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo sqlx prepare --workspace --check -- --all-targets
cargo test --workspace
```

Expected:
- `fmt` and `clippy` produce no output and exit 0;
- `prepare --check` exits 0;
- every `test result: ok.` line has `0 failed`, and the new `live_sharepoint_round_trip` shows as ignored.

- [ ] **Step 6: Commit.**

```bash
git add config/pdms.example.toml crates/pdms-core/tests/sharepoint.rs docs
git commit -m "sharepoint: example config, opt-in live test, and plan/roadmap status"
```

---

## Deviations from the index and the spec (for the reviewer)

**Index contracts:**

- `apply_placement_now(core, document_id)` became `apply_placement_now(core, document_id, to: Spot)`. It runs before the change is saved, so it can't read the target from the database.
- `put_version_file(core, tx_ctx: &PutCtx, file)` became `put_version_file(core, &PutTarget, &StoredFile, src: &Path)`. It is joined by `filestore::open(core, document_id, version_no, hash, size)` for callers that already have the version number.
- `SharePoint` resolves the site, library, and area folders **lazily** on first use rather than at startup. This keeps the server starting when SharePoint is down, as D-021 does for OIDC.

**Spec additions and gaps:**

- **New tables beyond spec §6:** `sharepoint_department_folders` and `sharepoint_document_folders`, which track folders by item ID. `sharepoint_files` gains `list_item_id`, `name` and `fields_hash`.
- **Soft-deleted documents** (the spec is silent): they move to `Restricted/<department>` at deletion and back on restore.
- **Department transfers** of department-visible documents also move first, extending §7.3's rule that tightening changes are synchronous to every change that narrows access.
- **"Open in PADM"** is a text column, because Graph cannot write hyperlink columns.
- **The "Current documents" view** can't be created through Graph v1.0, so P4's setup guide must include it as a manual step.
- **Empty files** can't be stored in SharePoint mode.
- **The version number in file names** is the PADM version number, so numbers skip over metadata-only versions.
- **Extensions** come from the sniffed content type.
- **Temporary upload names (D-103):** spec §7.1 says to upload to the final name. Files are uploaded as `<external ID> (uploading <token>).<ext>` instead, and the mirror job renames them, because the version number is only known inside the version's transaction, which starts after the upload.
