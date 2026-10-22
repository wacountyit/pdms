# PADM UI redesign, round 2 (R2): implementation plan

- **Date:** 2026-10-22
- **Spec:** [R2 design](../specs/2026-10-22-ui-redesign-r2-design.md),
  approved 2026-10-22.
- **Status:** done; all nine parts merged in
  [PR #4](https://github.com/wacountyit/pdms/pull/4).
- **How it is built:** all nine parts on one branch, `r2-build`, as one pull
  request. The build started before the spec's formal approval; the owner
  approved it on 2026-10-22 (D-097).
- **Checks for every part:** `cargo fmt --all`; `cargo clippy --workspace
  --all-targets --all-features -- -D warnings`; `cargo test --workspace`;
  `cargo sqlx prepare --workspace --check -- --all-targets` when queries
  changed; the screenshot tool for the part's pages (spec §11).
- **For every form a part touches:** it keeps its `action`, field names,
  CSRF token and HTMX targets. The part's tests assert them.

## Part 1: appointments, plus the shared page parts

1. **Shared components** in `static/app.css`: `.page-head`, `.page-lede`,
   `.page-actions` and `.empty` (spec §5.2), from tokens only.
2. **One seat-state wording.**
   - `pdms-core`: expose the single classifier as
     `appointments::seat_state`, and `vacant_seats`.
   - `pdms-web/src/seats.rs`: `status(state)` gives the label and status
     class; `counts(...)` gives the five board counts.
   - The dashboard, the county overview and both appointments pages use them.
3. **`/positions`:**
   - page header with "Add a position";
   - count tiles (the overview's board counts);
   - one table panel per board or body, with a status column;
   - empty states;
   - the add form in its own disclosure panel.
4. **`/positions/{id}`:**
   - header with breadcrumb, status and actions;
   - main column: current appointees (one panel each, with "Manage") and
     history;
   - side column: position facts, reminders, requirements.
5. **Tests:**
   - the labels match on the dashboard and `/positions`;
   - the page markup;
   - every form's action and fields;
   - visibility is unchanged.

## Part 2: calendar and feeds

Spec §6.2: the header, filters in a panel, month grid with "+N more",
agenda as the phone layout, the feeds page panels.

## Part 3: report detail and the overview totals

Spec §6.3: report header and options panel, pager at 100 rows. "Other"
excludes superseded documents; the overdue tile is relabelled. Tests for the
totals.

## Part 4: administration, with sub-project 3

Spec §5:
- the grouped admin menu (`partials/admin_nav.html`), using
  `Layout.admin_page`;
- every admin page moved to the pattern;
- a line in the P3/P4 plan index telling P4 to use the pattern.

**Sub-project 3 (D-087, D-088)** is built here too, at the owner's request on
2026-10-22:
1. **Migration `0011_small_changes.sql`:**
   - the seeded type becomes `interlocal_agreement`, "Interlocal Agreement
     (ILA)", prefix `ILA`, keeping a name or prefix an administrator changed;
   - queued Outlook sync jobs are deleted;
   - `graph_calendar_links` is dropped.
2. **The Outlook sync is removed:** `pdms-core::graph_calendar`, its config
   and validation, its worker job, the `/admin/outlook` routes (the old
   address redirects to `/admin/ocr`), its tests, the Compose secret, and the
   installer step (a re-run tidies the old settings).
3. **OCR:** the status moves to `/admin/ocr`, "Text extraction (OCR)", in the
   *System* group.
4. **Curated tags:**
   - `[documents] free_form_tags` defaults to `false`;
   - the new-document form offers curated tags as checkboxes, and the
     document page offers a choice of curated tags (a text field only for tag
     managers, or when free-form tags are on);
   - the Tags page lists uncurated tags under "Needs review".
5. **Docs:** DECISIONS (D-087 and D-088 done), ADMIN_GUIDE, README,
   ARCHITECTURE, ROADMAP §0.3, §0.5 and §0.6, and the P3/P4 plan index
   (migration numbers).

## Part 5: records requests

Spec §6.4: the three-step "New package" panel, and the packages table.

## Part 6: notifications, audit log and account

Spec §6.5 to §6.7. The sidebar link becomes "Your account".

## Part 7: document forms and document page sections

Spec §6.8: the new and edit forms in panels, plus the workflow, signatures
and comments sections.

## Part 8: activity feed and tab icon

Spec §8 and §9:
- a new `pdms-core` feed module with the event allow-list and the visibility
  and name rules;
- the dashboard panel;
- `static/icon.svg` and the icon links.

## Part 9: dark mode

Spec §7:
- the migration for `users.color_scheme` (next free number);
- the `data-theme` attribute, the dark token blocks, and the theme
  derivation in both modes;
- the switch on the account page and in the sidebar, with its audit event;
- the Branding check in both modes;
- the screenshot tool's `--theme` option.
