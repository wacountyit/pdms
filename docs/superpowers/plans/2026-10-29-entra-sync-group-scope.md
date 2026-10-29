# GS: Entra ID sync limited to one group. Implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When an administrator sets one Entra group, only its members
(nested groups count) have PDMS accounts. People who leave the group are
departed, people who come back are reactivated, and non-members are refused
at sign-in with a message that says why.

**Architecture:** A new setting, `entra_sync.group_id`, and a new Graph
paging helper. Each sync run reads the group's whole transitive member list
before it changes anything. Non-members are skipped while the delta is
applied. Active non-members are then departed by a guarded "group sweep",
which holds back a mass departure for an administrator to confirm. A new
`users.departed_from_group` flag marks the departures the sync may undo. A
mapping lock closes the P2 race between Department mapping changes and the
sync.

**Tech Stack:** Rust 2024, Axum, Askama, SQLx (PostgreSQL), Microsoft Graph
(app-only), and the in-process mock Graph in
`crates/pdms-core/tests/mock_graph/`.

**Spec:** [2026-10-27-entra-sync-group-scope-design.md](../specs/2026-10-27-entra-sync-group-scope-design.md)
(approved 2026-10-28; its §9 holds the owner's answers). Read it before
any task.

## Global Constraints

- Work goes straight to `main` in small commits. Each commit builds and
  passes the checks under "Commands" in `CLAUDE.md`. Commit messages start
  with `GS:`. Pull before you push, and never force-push.
- Keep the GS row in ROADMAP §0.1a current: the plan, then each task, marked
  In progress or Done with the date.
- **Migration:** `0016_entra_group_scope.sql`, the next number after `0015` on
  `main` (pull and re-check before Task 3). No license header (D-080).
- **Decision:** D-117, the next after D-116 (re-check before Task 7).
- Every new source file starts with `SPDX-License-Identifier: AGPL-3.0-or-later`
  and `Copyright (C) 2026 PDMS contributors`. Test files put those two lines
  first, then the `#![allow(...)]` block the other tests use.
- No `unwrap`, `expect`, `panic` or `dbg` outside tests.
- Templates: no `style="…"` attributes, no inline `<script>`, no `on…=`
  handlers. WCAG 2.1 AA, and one `<h1>` per page. Use the R2 admin page
  pattern (R2 spec §5.2): `.panel`, `.panel-head`, `.grid-form`, `.help`,
  `.toolbar`, `role="status"` for results.
- Every change of state writes its audit event in the same transaction as
  the change.
- After changing any `sqlx::query!` or migration, run
  `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/`.
- **The setting:** `entra_sync.group_id`, text. Empty means the whole tenant,
  and then behaviour is exactly as today. A value in `config/pdms.toml`
  (`[entra_sync] group_id`) wins and shows read-only (D-111).
- **Graph:** the new application permission is `GroupMember.Read.All`, needed
  only when a group is set. The calls are:
  - `GET /groups/{id}/transitiveMembers/microsoft.graph.user?$select=id&$top=999`,
    paged;
  - `POST /users/{id}/checkMemberGroups` with `{"groupIds":[id]}`;
  - `GET /groups/{id}?$select=id,displayName`.
- **The D-096 safety rules still hold:**
  - departures are held back if more than max(5, 20%) of linked accounts
    would leave at once;
  - the last administrator is never departed automatically;
  - an account with a local password is departed and reported;
  - the break-glass account (no directory link) is never touched.
- **Exact text the spec fixes:**
  - the missing-permission message: "PDMS can't read the group's members:
    add the GroupMember.Read.All permission (ADMIN_GUIDE, Entra ID sync)";
  - the departure reason: "is no longer in the PDMS group";
  - the reactivation reason: "is back in the PDMS group";
  - the button: "Depart these N people";
  - the sign-in refusal's audit reason: `not_in_group`.

## Review Focus

These are the conditions most likely to bite an administrator that the
spec's own test list doesn't cover. Each has a test in the task named.

1. **The member list can't be read partway through a run** (403, throttling,
   an outage). Nothing may change: no account created, updated or departed,
   and the saved delta cursor stays. The member list is read before the delta
   is applied. (Task 4)
2. **The group lists no members** (a wrong ID, or a dynamic group still
   filling). Nobody is departed, no **Depart these N people** button is
   offered, and "Needs attention" says to check the group. (Task 4)
3. **The group ID typed with capitals or braces** (`{6F1B…}`) works like the
   canonical lowercase form. Changing only its case doesn't start a full
   round. (Task 2)
4. **Clearing the group** (back to the whole tenant) reactivates nobody.
   People departed for leaving the group stay departed until an administrator
   reactivates them, or until a group is set again and they are in it.
   (Task 4)
5. **A group member who is disabled in Entra, or a guest while guests are
   off,** is not created, and doesn't add a problem line on every run.
   (Task 4)

## Before you start

The dev database was removed on 2026-10-29. Recreate it as README "Develop"
says (`docker run … --name pdms-dev-db …`, the role passwords, then
`cargo run -p pdms -- migrate`). For tests, export `PDMS_TEST_DATABASE_URL`,
`PDMS_TEST_OWNER_PASSWORD` and `PDMS_TEST_APP_PASSWORD` as README "Tests"
says. Then run `cargo test --workspace` once on `main` to get the baseline
(on 2026-10-26: 544 passed, 5 ignored).

**Command shorthand:**

- `CHECKS` means `cargo fmt --all`, then
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`, then
  `cargo test --workspace`;
- add `cargo sqlx prepare --workspace -- --all-targets` whenever queries or
  migrations changed.

---

### Task 1: Graph paging and groups in the mock Graph

**Files:**
- Modify: `crates/pdms-core/src/graph/client.rs` (next to `get_delta`)
- Modify: `crates/pdms-core/tests/mock_graph/mod.rs`
- Test: `crates/pdms-core/tests/graph.rs`

**Interfaces:**
- Produces:
  - `GraphClient::get_all<T: DeserializeOwned>(&self, start: &str) -> Result<Vec<T>, GraphError>`.
    It follows `@odata.nextLink` until a page has none. It stops with
    `GraphError::Http { code: "tooManyPages", .. }` after `MAX_PAGES`, as
    `get_delta` does, and reuses `RawPage<T>`.
  - Mock Graph helpers, all `&self`:
    - `add_group(id: &str, name: &str)`;
    - `add_to_group(group: &str, user: &str)`;
    - `remove_from_group(group: &str, user: &str)`;
    - `nest_group(parent: &str, child: &str)`;
    - `forbid_groups(on: bool)`.
  - Mock state: `pub groups: BTreeMap<String, MockGroup>` and
    `pub groups_forbidden: bool`, with
    `pub struct MockGroup { pub name: String, pub users: Vec<String>, pub groups: Vec<String> }`.
  - Mock routes. None of them changes `pending`, so the users delta doesn't
    see group changes, as in real Entra.
    - `GET /groups/{id}` gives `{"id","displayName"}`, or a 404
      `Request_ResourceNotFound`.
    - `GET /groups/{id}/transitiveMembers/microsoft.graph.user` gives
      `{"value":[{"id":…}]}`. It lists every user reachable through nested
      groups once, only users still in `users`, `page_size` per page, with
      `@odata.nextLink` using `$skiptoken=<offset>`.
    - `POST /users/{id}/checkMemberGroups` with body `{"groupIds":[…]}`
      gives `{"value":[the ids the user is transitively in]}`.
    - While `groups_forbidden` is on, every one of these gives a 403
      `Authorization_RequestDenied`.

- [ ] **Step 1: Write the failing tests** in `tests/graph.rs`:

```rust
#[tokio::test]
async fn get_all_follows_next_links_until_the_last_page() {
    // group G with users u1..u5, page_size 2 → 3 pages
    let ids: Vec<serde_json::Value> = graph.get_all(
        "/groups/G/transitiveMembers/microsoft.graph.user?$select=id&$top=999").await.unwrap();
    assert_eq!(ids.len(), 5);
    assert_eq!(m.requests().iter().filter(|r| r.contains("transitiveMembers")).count(), 3);
}

#[tokio::test]
async fn transitive_members_count_nested_groups_once() {
    // G has u1 and nested group N; N has u1 and u2 → u1, u2 once each
}

#[tokio::test]
async fn check_member_groups_is_transitive_and_groups_can_be_forbidden() {
    // u2 only in N (nested in G): POST checkMemberGroups {"groupIds":["G"]} → value == ["G"]
    // u3 in nothing → value == []
    // forbid_groups(true): get_all(...) is Err(GraphError::Auth(_))
}
```

- [ ] **Step 2: Run them to see them fail.**
  Run `cargo test -p pdms-core --test graph -- get_all transitive check_member`.
  Expected: compile errors (`get_all`, `add_group` not found).
- [ ] **Step 3: Implement `get_all`** and the mock routes and helpers above.
- [ ] **Step 4: Run the tests to see them pass.** Same command. Expected:
  PASS.
- [ ] **Step 5: Commit.** Run `CHECKS`, then
  `git commit -m "GS: Graph paging helper; groups in the mock Graph"` and
  push.

---

### Task 2: The group setting, Test group, and the admin page field

**Files:**
- Modify:
  - `crates/pdms-core/src/config.rs`: `EntraSyncConfig`, `validate()`, the
    unit test `graph_and_entra_sync_defaults_and_validation`;
  - `crates/pdms-core/src/settings.rs`: `SETTINGS`;
  - `crates/pdms-core/src/directory/entra_sync.rs`: `cursor_key`,
    `sync_all`, `test_group`;
  - `crates/pdms-web/src/routes/admin.rs`: `EntraSettings`,
    `EntraSettingsForm`, `save_entra_settings`, `entra_page`, and a new
    `entra_test_group`;
  - `crates/pdms-web/src/lib.rs`: the route;
  - `crates/pdms-web/templates/admin/entra.html`;
  - `config/pdms.example.toml`: `[entra_sync]`.
- Test:
  - `crates/pdms-core/tests/entra_sync.rs`;
  - `crates/pdms-web/tests/entra.rs`.

**Interfaces:**
- Consumes: Task 1's `get_all`, `add_group`, `nest_group` and
  `forbid_groups`.
- Produces:
  - `EntraSyncConfig.group_id: String`, default `""`.
  - `EntraSyncConfig::group(&self) -> Option<String>`: `None` when blank,
    otherwise the GUID as lowercase hyphenated text (`Uuid::parse_str`
    accepts braces and capitals).
  - `pub const MISSING_PERMISSION: &str` in `entra_sync`, with the exact
    spec text.
  - `entra_sync::test_group(core: &Core, ctx: &Ctx, group_id: &str) -> CoreResult<settings::Check>`.
    It requires `ManageSettings` and records
    `settings::record_test(core, ctx, "entra_sync", ok)`.
  - `pub(crate) async fn group_members(graph: &GraphClient, group: &str) -> CoreResult<BTreeSet<String>>`
    in `entra_sync`: lowercase user IDs. `GraphError::Auth` becomes
    `CoreError::validation(MISSING_PERMISSION)`.
  - `cursor_key(select: &str, include_guests: bool, group: Option<&str>) -> String`.
    It appends `;group=<id>` after the existing `;guests` suffix.
  - The route `POST /admin/entra/test-group`, handled by
    `admin::entra_test_group`.
  - `entra_page(state, user, flash, preview, group_check: Option<Check>)`.

**Behaviour:**
- **Validation** in `validate()`: a non-blank `group_id` that doesn't parse
  as a GUID fails with
  `entra_sync.group_id {:?} must be the group's object ID (a GUID)`.
- **Test group** checks the form's value without saving it. Its lines, in
  order:
  - a value that isn't a GUID: failed, "Enter the group's object ID: Entra
    admin center → Groups → the group → Object ID.";
  - no Graph configured: failed, "Set up the Microsoft 365 connection
    first.";
  - Graph gives `NotFound`: failed, "Entra has no group with the object ID
    {id}.";
  - Graph gives `Auth`: failed, with `MISSING_PERMISSION`;
  - otherwise passed, with the lines `Group "{displayName}" found.` and
    `It has {n} people, counting members of groups inside it.`
- **The field** goes in the Settings panel, after "Department field":
  - label "Only members of this group", `id="es-group"`, `name="group_id"`;
  - help text: "The object ID of an Entra group (Entra admin center →
    Groups → the group → Object ID). When it is set, only its members, and
    members of groups inside it, have PDMS accounts. Leave it empty to sync
    everyone in the tenant. Needs the GroupMember.Read.All permission.";
  - a locked value shows `disabled`, with the usual "Set in the configuration
    file" help;
  - a **Test group** button in the form's toolbar uses
    `formaction="/admin/entra/test-group"`;
  - the result renders like Administration → Text extraction (OCR): an
    `<h3>Test result</h3>` and a `role="status"` checklist.
- **Example config:** `group_id = ""  # an Entra group's object ID: only its members get PDMS accounts (needs GroupMember.Read.All)`.

- [ ] **Step 1: Write the failing tests.**
  - Unit, `config.rs`:
    `entra_sync_group_id_must_be_a_guid`. `""` gives `group() == None`.
    `"{6F1B2C3D-0000-4000-8000-00000000ABCD}"` gives
    `Some("6f1b2c3d-0000-4000-8000-00000000abcd")`. `"PDMS"` makes
    `validate()` an `Err` containing "must be the group's object ID".
  - Unit, `entra_sync.rs`: extend `the_cursor_key_covers_guests` with
    `cursor_key("id,department", false, Some("g")) == "id,department;group=g"`
    and `cursor_key("id,department", true, Some("g")) == "id,department;guests;group=g"`.
  - Core, `tests/entra_sync.rs`. First add the helper
    `async fn with_group(core: &Core, group: &str) -> Core`. It clones
    `core.config()`, sets `entra_sync.group_id = group`, and returns
    `Core::new(core.db.clone(), cfg).unwrap()`, as `synced` does. Tasks 4
    to 7 use it.
    - `test_group_names_the_group_and_counts_nested_members`. Group "PDMS"
      has A and the nested group N, and N has A and B. The check is `ok`, and
      its lines contain `Group "PDMS" found.` and `It has 2 people`.
      `audit_actions` contains `settings.tested`.
    - `test_group_explains_a_wrong_id_an_unknown_group_and_a_missing_permission`:
      - `"not-a-guid"` gives `!ok`, with a line containing "Object ID";
      - an unknown GUID gives "has no group with the object ID";
      - after `forbid_groups(true)`, a line contains "GroupMember.Read.All";
      - a clerk's `Ctx` gets `CoreError::Forbidden(_)`.
    - `changing_the_group_starts_a_full_round` (Review Focus 3), modelled on
      `changing_include_guests_starts_a_full_round`:
      - run once;
      - set `group_id` to G's lowercase ID, add A to G, and run: `full_rounds`
        goes up by one;
      - set the same ID in capitals with braces and run: `full_rounds` is
        unchanged.
  - Web, `tests/entra.rs`:
    - `the_group_field_saves_and_shows_read_only_from_the_configuration_file`:
      - posting `/admin/entra/settings` with a GUID in `group_id` puts it in
        the page's `value`;
      - posting `"PDMS"` gives a flash containing "object ID";
      - with `c.locked.insert("entra_sync.group_id")` set,
        `name="group_id"` renders `disabled`.
    - `test_group_without_the_connection_says_so`: posting
      `/admin/entra/test-group` gives a body containing "Set up the
      Microsoft 365 connection first."
- [ ] **Step 2: Run them to see them fail.**
  Run `cargo test -p pdms-core entra_sync_group_id cursor_key`, then
  `cargo test -p pdms-core --test entra_sync -- test_group changing_the_group`,
  then `cargo test -p pdms-web --test entra -- group`.
  Expected: compile errors or FAIL.
- [ ] **Step 3: Implement it.**
  - Add the config field, `group()`, validation, the registry entry, and
    `cursor_key`. `sync_all` passes `config.entra_sync.group().as_deref()`.
  - Add `group_members`, `test_group`, the page field, the handler, the
    route and the example config.
  - `save_entra_settings` adds `("entra_sync.group_id", String(f.group_id.trim()))`.
- [ ] **Step 4: Run the tests to see them pass.** The same commands, plus
  `cargo test -p pdms-core --test entra_sync`, which must stay green
  (behaviour without a group is unchanged).
- [ ] **Step 5: Commit.** Run `CHECKS` and `sqlx prepare`, then
  `git commit -m "GS: the group setting and Test group"` and push.

---

### Task 3: The "departed for leaving the group" mark

**Files:**
- Create: `migrations/0016_entra_group_scope.sql`
- Modify: `crates/pdms-core/src/directory/admin.rs`, next to
  `set_user_status`
- Test: `crates/pdms-core/tests/entra_sync.rs`

**Interfaces:**
- Produces:
  - **The column:** `users.departed_from_group boolean NOT NULL DEFAULT false`.
    The migration's comment says: "set when the Entra sync departs someone for
    leaving the PDMS group (GS); the sync reactivates only these. Any other
    status change clears it."
  - `pub async fn depart_from_group(db: &PgPool, ctx: &Ctx, id: Uuid) -> CoreResult<bool>`.
    It does what `set_user_status(…, Departed)` does: requires
    `ManageDirectory`, refuses the last administrator with
    `CoreError::Conflict`, revokes sessions, and queues the SharePoint
    reconcile. It also sets the flag, and the audit event
    `user.status_changed` gets `.context("reason", "is no longer in the PDMS group")`.
    `Ok(false)` means the person was not active, and nothing changed.
  - `pub async fn reactivate_back_in_group(db: &PgPool, ctx: &Ctx, id: Uuid) -> CoreResult<bool>`.
    Under `FOR UPDATE`, and only when `status = 'departed'` and the flag is
    set, it makes the person active, clears the flag, and audits
    `user.status_changed` with `.context("reason", "is back in the PDMS group")`.
    It returns whether anything changed. Roles, grants and memberships are
    left as they are.
  - `set_user_status` clears `departed_from_group` in the same `UPDATE`.
  - All three share one private helper that takes the open transaction. The
    status `UPDATE`, the session revoke, the reconcile and the audit are
    written once. Both new functions are re-exported from `directory` (as
    `pub use admin::*` already does).

- [ ] **Step 1: Write the failing tests:**
  - `departing_for_the_group_is_marked_and_audited`: the person is
    `departed` with `departed_from_group = true`, their sessions are gone,
    and the newest `user.status_changed` has context reason "is no longer in
    the PDMS group".
  - `only_a_group_departure_is_undone_by_reactivate_back_in_group`:
    - after `set_user_status(…, Departed)`, `reactivate_back_in_group`
      returns `false` and the person stays departed;
    - after `depart_from_group`, it returns `true`, the person is active with
      the flag cleared, the audit reason is "is back in the PDMS group", and
      a role assigned before the departure is still assigned.
  - `any_other_status_change_clears_the_group_mark`: `depart_from_group`,
    then `set_user_status(Active)`, then `set_user_status(Departed)`. Now
    `reactivate_back_in_group` returns `false`.
  - `the_last_administrator_is_not_departed_for_the_group`:
    `CoreError::Conflict(_)`, and the person stays active.
- [ ] **Step 2: Run them to see them fail.**
  Run `cargo test -p pdms-core --test entra_sync -- group_departure group_mark departing_for last_administrator_is_not`.
  Expected: compile errors.
- [ ] **Step 3: Write the migration and the functions.** Then migrate the dev
  database (`cargo run -p pdms -- migrate`) so the query macros check.
- [ ] **Step 4: Run the tests to see them pass.** The same command. Expected:
  PASS.
- [ ] **Step 5: Commit.** Run `CHECKS` and `sqlx prepare`, then
  `git commit -m "GS: migration 0016; departures for leaving the group are marked and reversible"`
  and push.

---

### Task 4: The sync with a group set

**Files:**
- Modify: `crates/pdms-core/src/directory/entra_sync.rs` (`SyncReport`,
  `sync_all`, `apply`, and the new `group_sweep` and `add_missing_members`)
- Test: `crates/pdms-core/tests/entra_sync.rs`

**Interfaces:**
- Consumes:
  - Task 1's mock groups;
  - Task 2's `group()`, `group_members` and `MISSING_PERMISSION`;
  - Task 3's `depart_from_group`, `reactivate_back_in_group` and
    `departed_from_group`.
- Produces:
  - `SyncReport.reactivated: Vec<Uuid>` and `SyncReport.held_back: Vec<Uuid>`.
    The struct already has `#[serde(default)]`, so older summaries still read.
  - The `directory.synced` audit event gains `.context("reactivated", n)` and
    `.context("held_back", n)`.
  - `apply` gains a `membership: Membership` argument, with a private
    `enum Membership { Everyone, Member, Outside }`.

**Behaviour.** Each run with a group set does this, in order:

1. **Read the member list first.** Call `group_members` before any delta
   page is applied, so a failure changes nothing (Review Focus 1). An error
   fails the run through the existing `record_failure`.
2. **Apply the delta** as today, with these changes:
   - **Removed, disabled, or a guest while guests are off:** handled exactly
     as today. These reasons come first, so they are never "left the group".
   - **`Outside` and active:** skipped. `apply` neither creates nor updates
     them; the group sweep departs them under its guard.
   - **`Member`, existing, departed, with `departed_from_group`:**
     `reactivate_back_in_group`, push the ID onto `report.reactivated`, then
     update the account as an active one.
3. **Add missing members.** Take each member in the list that is not in this
   round's `seen` and has no known account. Known means this issuer, and
   `status = 'active' OR NOT departed_from_group`. Read it with
   `/users/{id}?$select=…` and apply it as `Member`. Skip `NotFound`. This
   creates people added to the group with no other change in Entra, and
   reactivates people who came back.
4. **Run the group sweep.** Take the active accounts linked to this issuer
   (as `sweep` selects them, `created_at < started`) that are not in the
   member list.
   - The last administrator is spared and reported, using the same rule as
     `sweep`. Extract that rule into one helper both use.
   - If the member list is empty, depart nobody. Push "The PDMS group lists
     no members, so the {n} linked account(s) outside it were not departed.
     Check the group's object ID and its members." onto `errors`, and leave
     `held_back` empty (Review Focus 2).
   - If more would depart than `max(5, linked / 5)`, depart nobody, set
     `report.held_back` to their IDs, and add no `errors` line. The page
     shows them (Task 7).
   - Otherwise, call `depart_from_group` for each one. Increment `departed`.
     For an account with a local password, also add the same report line as
     `depart`, with "is no longer in the PDMS group".
5. **On a complete round,** the existing `sweep` runs afterwards, unchanged.

With no group set, `membership` is `Everyone` everywhere, and nothing
above runs.

- [ ] **Step 1: Write the failing tests.** Use `synced(&m)`, then
  `with_group` (Task 2).
  - `with_a_group_only_members_get_accounts_and_others_are_departed`:
    - set up: the tenant has A, B and C, and all three are synced first; the
      group has A and B; D is in the tenant, created after the group is set;
    - after a run, A and B are active, and C is `departed` with
      `departed_from_group`;
    - `r.departed == 1`, and D has no account;
    - the audit for C carries "is no longer in the PDMS group".
  - `a_member_added_to_the_group_is_created_at_the_next_run`:
    - E is in the tenant but not in the group, and isn't created;
    - `add_to_group(G, E)`, then run: E is created, and `requests()`
      contains `GET /v1.0/users/{E}`.
  - `members_of_a_nested_group_count`: F is only in N, which is nested in G.
    F gets an account.
  - `narrowing_the_group_holds_back_a_mass_departure`:
    - nine synced people, then a group with two of them;
    - `r.departed == 0`, and `r.held_back` holds exactly the seven others'
      IDs;
    - all nine stay active, and `r.errors` is empty.
  - `an_empty_group_departs_nobody_and_says_so` (Review Focus 2): the group
    has no members. `departed == 0`, `held_back` is empty, and one error
    contains "lists no members".
  - `a_missing_permission_fails_the_run_and_changes_nothing` (spec case 7,
    Review Focus 1):
    - set up: `forbid_groups(true)`, plus a new tenant member H in the delta;
    - the run is an `Err` whose text contains "GroupMember.Read.All", and
      `entra_sync::status` shows it as `last_error`;
    - H has no account, nobody was departed, and `saved_link` is unchanged.
  - `the_last_administrator_and_the_break_glass_account_are_kept` (spec
    case 8):
    - the only administrator is linked and outside the group: still active,
      and an error line contains "is no longer in the PDMS group but was not
      deactivated";
    - a local break-glass admin with no link is untouched;
    - a linked account with a local password outside the group is departed,
      and reported with "the account also had a local password".
  - `someone_back_in_the_group_is_reactivated_with_their_roles` (spec case
    10):
    - A has a role, leaves the group (departed), and is added back; the next
      run gives `r.reactivated == vec![a]`; A is active, with the role still
      assigned;
    - B left the group, was then disabled in Entra (departed by the delta),
      re-enabled, and added back: B stays departed;
    - C is in the group but was departed by hand with `set_user_status`: C
      stays departed.
  - `clearing_the_group_reactivates_nobody` (Review Focus 4):
    - A is departed for leaving the group, and then `group_id` is cleared; a
      full run leaves A departed;
    - set the group again with A in it, and run: A is reactivated.
  - `disabled_and_guest_members_are_skipped_quietly` (Review Focus 5): the
    group has a disabled member and a guest (guests off). Over two runs,
    neither gets an account, and `r.errors` stays empty.
- [ ] **Step 2: Run them to see them fail.**
  Run `cargo test -p pdms-core --test entra_sync -- group`.
  Expected: FAIL, because the sync ignores the group.
- [ ] **Step 3: Implement the five steps above** in `sync_all`, `apply`,
  `add_missing_members` and `group_sweep`. Update the module doc comment:
  "nobody is ever reactivated" now has the group exception.
- [ ] **Step 4: Run the tests to see them pass.** Run
  `cargo test -p pdms-core --test entra_sync`. Expected: every test passes,
  old and new (spec case 1: the existing tests are unchanged).
- [ ] **Step 5: Commit.** Run `CHECKS` and `sqlx prepare`, then
  `git commit -m "GS: the sync follows the group: departures held back, new members, reactivation"`
  and push.

---

### Task 5: The sign-in check and its message

**Files:**
- Modify:
  - `crates/pdms-core/src/directory/entra_sync.rs`: `sync_one`;
  - `crates/pdms-core/src/auth/oidc.rs`: `resolve_identity`, `complete`,
    and a new `refusal`;
  - `crates/pdms-web/src/routes/auth.rs`: `oidc_callback`, and a new
    `sso_failure_message`.
- Test:
  - `crates/pdms-core/tests/entra_sync.rs`;
  - unit tests in `oidc.rs` and `routes/auth.rs`.

**Interfaces:**
- Consumes: Task 4's `apply(…, Membership)`.
- Produces:
  - `pub enum SignInLookup { Account(Uuid), NotInGroup, NoAccount }` in
    `entra_sync`.
  - `sync_one(core: &Core, oid: &str) -> CoreResult<SignInLookup>`. When a
    group is set, it first posts `checkMemberGroups`. A non-member gets
    `NotInGroup`, without reading the user. A member is applied with
    `Membership::Member`.
  - `resolve_identity`: `NotInGroup` maps to `Err("not_in_group")`, and
    `NoAccount` follows today's `Ok(None)` path. Graph errors still give
    `Err("directory_unavailable")`.
  - `pub const NOT_IN_GROUP_MESSAGE: &str = "You aren't in the PDMS group in Microsoft Entra ID, so you can't sign in to PDMS. Ask IT to add you.";`
    in `auth::oidc`.
  - `fn refusal(reason: &str) -> CoreError` in `oidc.rs`. `"not_in_group"`
    maps to `CoreError::Validation(NOT_IN_GROUP_MESSAGE.into())`, and
    anything else to `CoreError::Forbidden(None)`. `complete` uses it where
    it returns `Forbidden(None)` today. The `auth.login_failed` audit with the
    reason is written as now.
  - `fn sso_failure_message(e: &CoreError) -> &'static str` in
    `routes/auth.rs`. `Validation(m)` with `m == NOT_IN_GROUP_MESSAGE` maps
    to that message, and anything else to today's "Single sign-on failed.
    Try again, or contact your administrator." `oidc_callback` renders the
    sign-in page with it, for `Unauthenticated`, `Forbidden(_)` and that one
    `Validation`. Any other error is unchanged.

- [ ] **Step 1: Write the failing tests.**
  - Core: `first_sign_in_checks_the_group`. With a group set:
    - a member A gets `Ok((id, _))`;
    - a non-member N gets `Err("not_in_group")`, N has no account, and
      `requests()` contains `POST /v1.0/users/{N}/checkMemberGroups`;
    - after `forbid_groups(true)`, a new member gets
      `Err("directory_unavailable")`.
  - Core: update `sync_one_reads_a_single_user` to the new return type:
    `SignInLookup::Account(_)` and `SignInLookup::NoAccount` where it had
    `Some` and `None`.
  - Unit, `oidc.rs`: `refusal_names_the_group_only_for_not_in_group`.
    `refusal("not_in_group")` is a `Validation` equal to
    `NOT_IN_GROUP_MESSAGE`; `refusal("inactive")` is `Forbidden(None)`.
  - Unit, `routes/auth.rs`: `sso_failure_message_shows_the_group_case`. It
    gives `NOT_IN_GROUP_MESSAGE` for that error, and the generic text for
    `Unauthenticated` and for `Validation("other")`.
- [ ] **Step 2: Run them to see them fail.**
  Run `cargo test -p pdms-core --test entra_sync -- sign_in sync_one`,
  `cargo test -p pdms-core --lib refusal` and
  `cargo test -p pdms-web --lib sso_failure`. Expected: compile errors.
- [ ] **Step 3: Implement it** as the Interfaces block says.
- [ ] **Step 4: Run the tests to see them pass.** The same commands, plus
  `cargo test -p pdms-web --test oidc`, which must stay green.
- [ ] **Step 5: Commit.** Run `CHECKS`, then
  `git commit -m "GS: sign-in checks the group and says why it refuses"` and
  push.

---

### Task 6: Mapping changes and the sync take one lock

**Files:**
- Modify:
  - `crates/pdms-core/src/directory/entra_values.rs`: `upsert`, `remove`,
    `lookup`;
  - `crates/pdms-core/src/directory/entra_sync.rs`: `apply`;
  - `docs/ADMIN_GUIDE.md`: in "Entra ID user sync", the paragraph that
    begins "…not need a full resync. The one exception: if you change
    mappings while a…".
- Test: `crates/pdms-core/tests/entra_sync.rs`

**Interfaces:**
- Produces:
  - `pub const MAPPING_LOCK: i64 = 0x5044_4d53_454d_4150;` ("PDMSEMAP") in
    `entra_values`.
  - `pub(super) async fn lookup_in(conn: &mut PgConnection, value: Option<&str>) -> CoreResult<Option<Uuid>>`.
    `lookup` keeps its signature and delegates to it.
  - `upsert` and `remove` take `pg_advisory_xact_lock(MAPPING_LOCK)` as the
    first statement of their transaction.
  - `apply` takes `pg_advisory_xact_lock_shared(MAPPING_LOCK)` as the first
    statement of its transaction, and looks the mapping up with `lookup_in`
    inside that transaction, not before it.

- [ ] **Step 1: Write the failing tests:**
  - `a_mapping_change_waits_for_a_person_being_applied`:
    - hold `pg_advisory_xact_lock_shared(MAPPING_LOCK)` in an open
      transaction on another connection, and spawn `entra_values::upsert`;
    - after 300 ms the task is not finished;
    - commit, and it finishes `Ok`.
  - `a_person_is_applied_with_the_mapping_committed_before_them` (spec case
    11):
    - hold `pg_advisory_xact_lock(MAPPING_LOCK)` in transaction T, and
      inside T insert a `department_entra_values` row mapping A's value to
      department RB;
    - spawn `entra_sync::run`; after 300 ms it is not finished;
    - commit T; the run finishes, and `idp_departments(A) == [RB]`.
- [ ] **Step 2: Run them to see them fail.**
  Run `cargo test -p pdms-core --test entra_sync -- waits_for committed_before`.
  Expected: FAIL (nothing waits).
- [ ] **Step 3: Add the lock and `lookup_in`, and move the lookup into
  `apply`'s transaction.** In ADMIN_GUIDE, delete the "one exception"
  advice to press **Full resync** after changing mappings during a sync,
  and keep the rest of the paragraph.
- [ ] **Step 4: Run the tests to see them pass.** Run
  `cargo test -p pdms-core --test entra_sync`. Expected: PASS, all of it.
- [ ] **Step 5: Commit.** Run `CHECKS` and `sqlx prepare`, then
  `git commit -m "GS: mapping changes and the sync no longer race"` and
  push.

---

### Task 7: Depart these N people, the page's report, and the docs

**Files:**
- Modify:
  - `crates/pdms-core/src/directory/entra_sync.rs`: `Person`, `people`,
    `depart_held_back`;
  - `crates/pdms-web/src/routes/admin.rs`: `EntraPage`, `entra_page`, and a
    new `entra_depart_held_back`;
  - `crates/pdms-web/src/lib.rs`: the route;
  - `crates/pdms-web/templates/admin/entra.html`;
  - `docs/ADMIN_GUIDE.md`, `docs/SHAREPOINT_SETUP.md` (B2),
    `docs/DECISIONS.md`, `docs/ROADMAP.md`, and the README if it describes
    the Entra sync.
- Test:
  - `crates/pdms-core/tests/entra_sync.rs`;
  - `crates/pdms-web/tests/entra.rs`.

**Interfaces:**
- Consumes:
  - Task 4's `SyncReport.held_back`, `SyncReport.reactivated` and
    `group_members`;
  - Task 3's `depart_from_group`;
  - `LOCK_KEY`.
- Produces:
  - `pub struct Person { pub id: Uuid, pub name: String, pub email: String, pub active: bool, pub roles: Vec<String> }`.
    `roles` holds the distinct role names, sorted.
  - `pub async fn people(db: &PgPool, ids: &[Uuid]) -> CoreResult<Vec<Person>>`,
    in name order.
  - `pub async fn depart_held_back(core: &Core, ctx: &Ctx) -> CoreResult<(u32, Vec<String>)>`:
    - it requires `ManageDirectory`, the sync turned on, and a group set;
    - it takes `pg_try_advisory_xact_lock(LOCK_KEY)`, and if that fails it
      returns `Conflict("a directory sync is already running")`;
    - it reads the member list fresh, then calls `depart_from_group` for each
      `held_back` ID that is still active and whose `oidc_subject` is still
      outside the group;
    - a `Conflict`, such as the last administrator, goes into the returned
      problems;
    - it then sets `held_back` to `[]` in `sync_cursors.last_summary`.
  - The route `POST /admin/entra/depart-held-back`, handled by
    `admin::entra_depart_held_back`. Its flash is "Departed {n} people.",
    plus the problems.
  - `EntraPage` gains:
    - `group: Option<String>`;
    - `held_back: Vec<Person>` (only the active ones);
    - `reactivated: Vec<Person>`;
    - `group_check: Option<Check>`, already added in Task 2.

**Page.** These changes are in `admin/entra.html`, inside the "Sync status"
panel:

- **Intro paragraph:** replace "Nobody is reactivated automatically." with
  "Nobody is reactivated automatically, except people the sync departed for
  leaving the PDMS group, once they are back in it."
- **Facts:** add `<dt>Who is synced</dt>`, with "Everyone in the tenant" or
  "Members of the group {id}".
- **Result line:** append ", {n} reactivated".
- **When `held_back` is not empty:**
  - `<h3>Waiting for your confirmation</h3>`;
  - "These {n} people are outside the PDMS group. That is more than the sync
    departs at once, so none of them was departed.";
  - a list of "Name <email>";
  - a form posting to `/admin/entra/depart-held-back` with the CSRF token
    and `<button type="submit" class="button danger">Depart these {n} people</button>`.
- **When `reactivated` is not empty:**
  - `<h3>Reactivated in the last run</h3>`;
  - a list of "Name <email>". When `roles` is not empty, add
    `<span class="status status-warning">Has roles: {roles joined ", "}</span>`.

**Docs:**
- **ADMIN_GUIDE, "Entra ID user sync"**, all of spec §7:
  - the group field and `GroupMember.Read.All`;
  - syncing an on-premises AD group to Entra, or making the group in Entra;
  - the first run and **Depart these N people**;
  - after removing someone, and after adding them back: automatic
    reactivation with their roles, listed on the page;
  - clearing the group reactivates nobody;
  - why **Assignment required** is still worth turning on.
- **SHAREPOINT_SETUP.md B2:** add `GroupMember.Read.All` (application, admin
  consent), "only when the Entra sync is limited to a group".
- **DECISIONS:** add **D-117 "Entra sync limited to one group" — Accepted
  (built 2026-MM-DD)**, with the spec §9 answers. Add "(amended by D-117)"
  to the headings of D-086 and D-096, as D-003 does.
- **ROADMAP:**
  - §0.3 GS row set to **Done** with D-117 and migration `0016`;
  - an entry in §0.1, with the test count;
  - remove item 2 from §0.5, and the GS column from §0.6;
  - remove the GS row from §0.1a. This last one goes in its own commit,
    `docs: GS done`, after this task's push.

- [ ] **Step 1: Write the failing tests.**
  - Core: `depart_these_people_departs_only_those_still_outside`:
    - set up as in `narrowing_the_group_holds_back_a_mass_departure` (seven
      held back); then add P1 back to the group, and depart P2 by hand;
    - `depart_held_back` returns `(5, vec![])`; P1 is active;
    - `status().last_summary.held_back` is empty;
    - each of the five has an audit with reason "is no longer in the PDMS
      group";
    - a clerk's `Ctx` gets `Forbidden(_)`.
  - Core: `depart_these_people_waits_for_a_running_sync`: while a
    transaction holds `pg_advisory_xact_lock(LOCK_KEY)`, `depart_held_back`
    is `Err(CoreError::Conflict(_))`.
  - Web: `the_page_lists_held_back_and_reactivated_people`:
    - set up: write `sync_cursors` (`name = 'entra.users'`) directly with a
      `last_summary` holding six active users' IDs in `held_back`, and one
      user with the Administrator role in `reactivated`;
    - the page contains "Depart these 6 people", each held-back name,
      "Reactivated in the last run" and "Has roles: Administrator";
    - posting the button without Graph redirects with an error flash.
- [ ] **Step 2: Run them to see them fail.**
  Run `cargo test -p pdms-core --test entra_sync -- depart_these` and
  `cargo test -p pdms-web --test entra -- held_back`.
  Expected: compile errors.
- [ ] **Step 3: Implement it**: `people`, `depart_held_back`, the handler,
  the route and the page. Then write the docs.
- [ ] **Step 4: Run the full checks.** Run `CHECKS` and `sqlx prepare`, then
  `cargo test -p pdms-web --test csp_guard` (part of `CHECKS`, named here
  because the template changed). Record the test count for ROADMAP §0.1.
- [ ] **Step 5: Take the screenshots.** Use
  [tools/ui-screenshots](../../../tools/ui-screenshots/README.md) on
  `/admin/entra` with a held-back list and a reactivated list in the demo
  database. Check that there is no sideways scroll and exactly one `<h1>`,
  and do a keyboard pass over the new button.
- [ ] **Step 6: Commit.**
  `git commit -m "GS: Depart these N people, the reactivated list, docs and D-117"`,
  push, then the separate `docs: GS done` (§0.1a) commit.
