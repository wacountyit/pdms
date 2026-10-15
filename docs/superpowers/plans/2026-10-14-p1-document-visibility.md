# P1 — Document visibility (Department / County-wide / Restricted) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every document one of three visibility levels: Department (default), County-wide (every active account may view), or Restricted (explicit grants only). This replaces the `is_confidential` and `inherit_acl` flags in the engine, SQL filter, database, services, web pages, and API.

**Architecture:**

- `pdms-authz` gains a `Visibility` enum, which `DocumentAttrs` carries instead of two booleans.
- `decide()` gives *view* on County-wide documents to every active principal. Restricted behaves like the old "confidential".
- The SQL predicate mirrors both rules, and the Postgres property test proves they agree.
- A new migration converts the data.
- `set_visibility` (owner or `manage_permissions`) replaces `set_access_settings`.
- The web create form, document page, and API expose the setting.

**Tech Stack:** Rust 2024, Axum 0.8, SQLx 0.9 (`query!` macros with `.sqlx/` offline data), Askama 0.16, proptest, PostgreSQL 15/16.

**Spec:** [docs/superpowers/specs/2026-10-14-sharepoint-storage-entra-sync-design.md](../specs/2026-10-14-sharepoint-storage-entra-sync-design.md) §2 D7, §6 "Document visibility" and "Authorization rule changes"; decision D-085 in `docs/DECISIONS.md`. **Index:** [2026-10-14-padm-sharepoint-entra-index.md](2026-10-14-padm-sharepoint-entra-index.md) (contracts "Produced by P1").

## Global Constraints

- **Values.** The enum values are exactly `department`, `county_wide`, `restricted`. The labels are exactly "Department", "County-wide", "Restricted". Never call it "Public" anywhere in the interface.
- **County-wide** grants **view only**, to every **active** principal. It never grants create, edit, delete, approve, manage_permissions, or export.
- **Restricted** behaves exactly like today's confidential: only document-scope grants and the owner rule apply.
- **Department** behaves exactly like today's default (`inherit_acl = true`, `is_confidential = false`).
- **Migration mapping:** `restricted` where `is_confidential OR NOT inherit_acl`, otherwise `department` (spec §6).
- **Who may change visibility:** the document's owner, or a holder of `manage_permissions` on it (D-085).
- **Auditing:** every change is audited in the same transaction as `document.visibility_changed` with `change("visibility", from, to)`.
- **SQL agreement:** the SQL predicate and `decide()` must agree on every input (`crates/pdms-core/tests/authz_sql.rs`).
- **Migrations:** the new migration is `migrations/0008_document_visibility.sql`. Never edit migrations 0001–0007. Migrations carry no SPDX header (D-080).
- **New Rust files** start with `// SPDX-License-Identifier: AGPL-3.0-or-later` and `// Copyright (C) 2026 PDMS contributors`.
- **Lints:** `unwrap`, `expect`, `panic`, and `dbg` are denied outside tests (workspace lints); `clippy.toml` allows them in tests. `unsafe` is forbidden.
- **Name clash:** `crate::notifications::dispatch::Visibility` is an unrelated `pub(crate)` type (who may receive a notice). In files that import it (`comments.rs`, `notifications/planner.rs`, `signatures.rs`, `ocr/mod.rs`, `workflow.rs`, `notifications/actions.rs`), never import `pdms_authz::Visibility` unqualified; let the type be inferred or write `pdms_authz::Visibility`.
- **Offline query data:** after any `sqlx::query!` or migration change, run `cargo sqlx prepare --workspace -- --all-targets` and commit `.sqlx/`.
- **Dev environment:** use the "Shared development setup" in the index. `DATABASE_URL` must point at the dev database with migration 0008 applied before building.

## Review Focus

These five inputs are implied by the spec and most likely to bite a user if they break. Each has a test in its owning task.

1. **Deactivated account and County-wide.** A deactivated (departed) account must **not** see a County-wide document, even though "everyone" can (Task 2, `county_wide_lets_every_active_account_view_only`).
2. **An editor who is not the owner tries to change visibility.** They must get "You do not have permission" (403), with a denial in the audit log, and the setting stays unchanged (Task 3, `editor_who_is_not_owner_cannot_change_visibility`).
3. **Existing data after upgrade.** Documents marked confidential, or with inheritance turned off, must come out Restricted, never Department (Task 2, Step 2 dev-database check).
4. **An unknown value from a form or API call** (`public`, `County-wide`, empty string in the API) must be rejected with a validation message, never silently treated as Department (Task 4 `unknown_visibility_value_is_rejected`, Task 5 `api_rejects_unknown_visibility`).
5. **Saving the same visibility again** is a no-op: no audit entry and no `updated_at` change (Task 3, `setting_same_visibility_is_a_no_op`).

---

### Task 1: The `Visibility` type

**Files:**
- Modify: `crates/pdms-authz/src/model.rs` (add after `PermissionSet`'s `Debug` impl, before `pub enum Subject`)
- Modify: `crates/pdms-authz/src/lib.rs:23-26` (re-export)
- Test: `crates/pdms-authz/src/tests.rs` (append)

**Interfaces:**
- Consumes: nothing.
- Produces: `pdms_authz::Visibility { Department (default), CountyWide, Restricted }` with `ALL`, `as_str()`, `label()`, `is_tighter_than(self, other) -> bool`, `inherits_broad_grants(self) -> bool`, `Display`, `FromStr<Err = UnknownVisibility>`, `TryFrom<String>`. Also `pdms_authz::UnknownVisibility(pub String)`.

- [ ] **Step 1: Write the failing test.** Append to `crates/pdms-authz/src/tests.rs`:

```rust
#[test]
fn visibility_round_trips_labels_and_orders() {
    for v in Visibility::ALL {
        assert_eq!(v.as_str().parse::<Visibility>().unwrap(), v);
        assert_eq!(Visibility::try_from(v.as_str().to_owned()).unwrap(), v);
        assert_eq!(v.to_string(), v.as_str());
    }
    assert_eq!(Visibility::default(), Visibility::Department);
    assert_eq!(
        Visibility::ALL.map(Visibility::label),
        ["Department", "County-wide", "Restricted"]
    );
    for bad in ["public", "County-wide", "", "RESTRICTED"] {
        assert!(bad.parse::<Visibility>().is_err(), "{bad:?}");
    }
    // Restrictiveness: County-wide < Department < Restricted.
    assert!(Visibility::Restricted.is_tighter_than(Visibility::Department));
    assert!(Visibility::Department.is_tighter_than(Visibility::CountyWide));
    assert!(Visibility::Restricted.is_tighter_than(Visibility::CountyWide));
    assert!(!Visibility::CountyWide.is_tighter_than(Visibility::Department));
    assert!(!Visibility::Department.is_tighter_than(Visibility::Department));
    assert!(Visibility::Department.inherits_broad_grants());
    assert!(Visibility::CountyWide.inherits_broad_grants());
    assert!(!Visibility::Restricted.inherits_broad_grants());
}
```

- [ ] **Step 2: Run the test and confirm it fails.**

Run: `cargo test -p pdms-authz visibility_round_trips_labels_and_orders`
Expected: compile error `cannot find type 'Visibility' in this scope`.

- [ ] **Step 3: Implement.** Insert into `crates/pdms-authz/src/model.rs`, after the `impl fmt::Debug for PermissionSet { … }` block:

```rust
/// Who can see a document without an explicit share (D-085).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Department and organization-wide grants apply. The default.
    #[default]
    Department,
    /// As `Department`, and every active account may also view it.
    CountyWide,
    /// Only document-scope grants and the owner rule apply.
    Restricted,
}

impl Visibility {
    pub const ALL: [Visibility; 3] = [
        Visibility::Department,
        Visibility::CountyWide,
        Visibility::Restricted,
    ];

    /// Value of the `document_visibility` Postgres enum.
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Department => "department",
            Visibility::CountyWide => "county_wide",
            Visibility::Restricted => "restricted",
        }
    }

    /// What people see in the interface.
    pub fn label(self) -> &'static str {
        match self {
            Visibility::Department => "Department",
            Visibility::CountyWide => "County-wide",
            Visibility::Restricted => "Restricted",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Visibility::CountyWide => 0,
            Visibility::Department => 1,
            Visibility::Restricted => 2,
        }
    }

    /// Whether `self` lets fewer people see the document than `other`.
    pub fn is_tighter_than(self, other: Visibility) -> bool {
        self.rank() > other.rank()
    }

    /// Whether department- and global-scope grants reach the document.
    pub fn inherits_broad_grants(self) -> bool {
        self != Visibility::Restricted
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown visibility: {0}")]
pub struct UnknownVisibility(pub String);

impl FromStr for Visibility {
    type Err = UnknownVisibility;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Visibility::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .ok_or_else(|| UnknownVisibility(s.to_owned()))
    }
}

impl TryFrom<String> for Visibility {
    type Error = UnknownVisibility;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}
```

Then, in `crates/pdms-authz/src/lib.rs`, replace the `pub use model::{ … };` block with:

```rust
pub use model::{
    Assignee, DocumentAttrs, Grant, Permission, PermissionSet, Policy, PositionAttrs, Principal,
    RoleHolding, Scope, Subject, UnknownPermission, UnknownVisibility, Visibility,
};
```

- [ ] **Step 4: Run the test and confirm it passes.**

Run: `cargo test -p pdms-authz visibility_round_trips_labels_and_orders`
Expected: `test tests::visibility_round_trips_labels_and_orders ... ok`

- [ ] **Step 5: Commit.**

```bash
git add crates/pdms-authz/src/model.rs crates/pdms-authz/src/lib.rs crates/pdms-authz/src/tests.rs
git commit -m "authz: add Visibility type (department, county_wide, restricted)"
```

---

### Task 2: Switch the engine, SQL filter, database, and callers to `visibility`

Removing two fields from `DocumentAttrs` breaks every crate at once, so this task is one atomic switch. Behaviour is unchanged, except that County-wide now exists. All existing tests must still pass at the end, with only the mechanical edits listed here.

**Files:**
- Create: `migrations/0008_document_visibility.sql`
- Modify: `crates/pdms-authz/src/model.rs` (`DocumentAttrs`)
- Modify: `crates/pdms-authz/src/engine.rs` (`AllowReason`, `scope_covers`, `decide`)
- Modify: `crates/pdms-authz/src/sql.rs` (predicate and its tests)
- Modify: `crates/pdms-authz/src/strategies.rs` (`document()`, new `visibility()`)
- Modify: `crates/pdms-authz/src/tests.rs`
- Modify: `crates/pdms-core/src/documents/access.rs:44-64` (`load`)
- Modify: `crates/pdms-core/src/documents/service.rs` (`Document`, `fetch`, snapshot JSON, `NewDocument`, `create`, `set_access_settings` → `set_visibility`)
- Modify: `crates/pdms-core/src/documents/search.rs` (`DocumentSummary`, select list, CSV column)
- Modify: `crates/pdms-core/src/calendar.rs:432` and `:607`
- Modify: `crates/pdms-core/src/notifications/planner.rs:80-105` and `:460-466`
- Modify: `crates/pdms-core/src/ocr/mod.rs:251-256` and `:422-428`
- Modify: `crates/pdms-core/src/graph_calendar.rs:353`
- Modify: `crates/pdms-core/src/demo.rs:479`
- Modify: `crates/pdms-web/src/routes/documents.rs` (`Row.confidential` (the list-row struct), `AccessForm` / `access_settings` → `VisibilityForm` / `set_visibility`)
- Modify: `crates/pdms-web/src/lib.rs:206` (route)
- Modify: `crates/pdms-web/src/routes/exports.rs:91`
- Modify: `crates/pdms-web/src/routes/api.rs:681`
- Modify: `crates/pdms-web/templates/documents/show.html:7-8` and `:195-202`
- Modify (mechanical, for the `NewDocument.visibility` field): every `NewDocument { … }` literal (23 sites)
- Modify (mechanical, renamed function): tests calling `set_access_settings`
- Test: `crates/pdms-core/tests/authz_sql.rs`, `crates/pdms-core/tests/directory.rs`, `crates/pdms-core/tests/documents.rs:706`
- Regenerate: `.sqlx/`

**Interfaces:**
- Consumes: `pdms_authz::Visibility` (Task 1).
- Produces:
  - `DocumentAttrs { id, department_id, owner_id, visibility: Visibility }`;
  - `AllowReason::CountyWide`;
  - `pdms_core::documents::Document.visibility: Visibility`;
  - `pdms_core::documents::NewDocument.visibility: Visibility`;
  - `pdms_core::documents::search::DocumentSummary.visibility: Visibility`;
  - `pub async fn set_visibility(core: &Core, ctx: &Ctx, id: Uuid, to: Visibility) -> CoreResult<()>` (requires `manage_permissions` for now; Task 3 adds owners);
  - route `POST /documents/{id}/visibility`.

- [ ] **Step 1: Write the failing engine tests.** In `crates/pdms-authz/src/tests.rs`:

(a) Change the `doc` helper body to:

```rust
fn doc(id: Uuid, department_id: Uuid, owner_id: Uuid) -> DocumentAttrs {
    DocumentAttrs {
        id,
        department_id,
        owner_id,
        visibility: Visibility::Department,
    }
}
```

(b) Replace the whole `broken_inheritance_ignores_department_and_global_grants` test with:

```rust
#[test]
fn restricted_ignores_department_and_global_grants() {
    let alice = user(ALICE, &[OPS]);
    let mut d = doc(DOCUMENTS[0], OPS, BOB);
    d.visibility = Visibility::Restricted;
    let g = [
        department_default(OPS),
        grant(Subject::User(ALICE), Scope::Global, Permission::View),
    ];
    assert!(!allowed(&alice, Permission::View, &d, &g));
    let g2 = [grant(
        Subject::User(ALICE),
        Scope::Document(d.id),
        Permission::View,
    )];
    assert!(allowed(&alice, Permission::View, &d, &g2));
}
```

(c) In `confidential_requires_document_grant_even_for_admin`, rename it to `restricted_requires_document_grant_even_for_admin`, and replace `d.is_confidential = true;` with `d.visibility = Visibility::Restricted;`. Also change its last comment to `// Owner still sees their own restricted document.`

(d) Add this test after it:

```rust
#[test]
fn county_wide_lets_every_active_account_view_only() {
    let outsider = user(ALICE, &[HR]);
    let mut d = doc(DOCUMENTS[0], OPS, BOB);
    d.visibility = Visibility::CountyWide;
    let g = [department_default(OPS)];
    assert_eq!(
        decide(&Policy::default(), &outsider, Permission::View, &d, &g, now()),
        Decision::Allow(AllowReason::CountyWide)
    );
    for perm in [
        Permission::Create,
        Permission::Edit,
        Permission::Delete,
        Permission::Approve,
        Permission::ManagePermissions,
        Permission::Export,
    ] {
        assert!(!allowed(&outsider, perm, &d, &g), "{perm}");
    }
    // Review Focus 1: a deactivated account sees nothing, County-wide included.
    let mut departed = outsider.clone();
    departed.active = false;
    assert!(!allowed(&departed, Permission::View, &d, &g));
}
```

(e) In the `proptest!` block, replace `confidential_documents_ignore_broad_grants` with the two properties below:

```rust
    #[test]
    fn restricted_documents_ignore_broad_grants(
        p in strategies::principal(), perm in strategies::permission(), mut d in strategies::document(),
        gs in strategies::grants(),
    ) {
        d.visibility = Visibility::Restricted;
        let doc_only: Vec<Grant> = gs.iter().filter(|g| matches!(g.scope, Scope::Document(_))).cloned().collect();
        prop_assert_eq!(allowed(&p, perm, &d, &gs), allowed(&p, perm, &d, &doc_only));
    }

    #[test]
    fn county_wide_adds_view_and_nothing_else(
        p in strategies::principal(), perm in strategies::permission(), mut d in strategies::document(),
        gs in strategies::grants(),
    ) {
        d.visibility = Visibility::Department;
        let as_department = allowed(&p, perm, &d, &gs);
        d.visibility = Visibility::CountyWide;
        let as_county_wide = allowed(&p, perm, &d, &gs);
        if perm == Permission::View {
            prop_assert_eq!(as_county_wide, p.active);
        } else {
            prop_assert_eq!(as_county_wide, as_department);
        }
    }
```

(f0) Two existing properties assume "no grants means no access", which no longer holds for viewing a County-wide document. In the same `proptest!` block, replace `deny_by_default` and `create_grants_never_grant_document_access` with:

```rust
    #[test]
    fn deny_by_default(p in strategies::principal(), perm in strategies::permission(), mut d in strategies::document()) {
        d.owner_id = Uuid::from_u128(0xdead);
        // Without grants, the only access is viewing a County-wide document.
        let county_wide_view = perm == Permission::View && d.visibility == Visibility::CountyWide && p.active;
        prop_assert_eq!(allowed(&p, perm, &d, &[]), county_wide_view);
    }

    #[test]
    fn create_grants_never_grant_document_access(
        p in strategies::principal(), perm in strategies::permission(), mut d in strategies::document(),
        gs in strategies::grants(),
    ) {
        d.owner_id = Uuid::from_u128(0xdead);
        let creates: Vec<Grant> = gs.into_iter().filter(|g| g.permission == Permission::Create).collect();
        // Create grants change nothing compared with having no grants at all.
        prop_assert_eq!(allowed(&p, perm, &d, &creates), allowed(&p, perm, &d, &[]));
    }
```

(f) In `crates/pdms-authz/src/sql.rs`, add this to `mod tests`:

```rust
    #[test]
    fn county_wide_clause_only_for_view() {
        let view = visible_documents_predicate(
            &Policy::default(),
            &principal(true),
            Permission::View,
            Utc::now(),
            "d",
        )
        .unwrap();
        assert!(
            view.to_numbered(1)
                .0
                .starts_with("(d.visibility = 'county_wide' OR ")
        );
        let edit = visible_documents_predicate(
            &Policy::default(),
            &principal(true),
            Permission::Edit,
            Utc::now(),
            "d",
        )
        .unwrap();
        let sql = edit.to_numbered(1).0;
        assert!(!sql.contains("county_wide"));
        assert!(sql.contains("d.visibility <> 'restricted'"));
        assert!(!sql.contains("inherit_acl") && !sql.contains("is_confidential"));
    }
```

- [ ] **Step 2: Write the migration, apply it to the dev database, and check the data mapping (Review Focus 3).**

Create `migrations/0008_document_visibility.sql`:

```sql
-- D-085: one visibility level replaces is_confidential and inherit_acl.
--   department  : department and organization-wide grants apply (the default)
--   county_wide : as department, and every active account may also view
--   restricted  : only document-scope grants and the owner rule apply
CREATE TYPE document_visibility AS ENUM ('department', 'county_wide', 'restricted');

ALTER TABLE documents ADD COLUMN visibility document_visibility NOT NULL DEFAULT 'department';

-- Both old flags had the same effect in the engine (department and global grants
-- stopped applying), which is exactly 'restricted'.
UPDATE documents SET visibility = 'restricted' WHERE is_confidential OR NOT inherit_acl;

ALTER TABLE documents DROP COLUMN inherit_acl, DROP COLUMN is_confidential;

COMMENT ON COLUMN documents.visibility IS
  'department | county_wide (all active accounts may view) | restricted (document grants and owner only); D-085';
```

Before applying it, seed three documents in the old shape in the dev database:

```bash
docker exec -i pdms-dev-db psql -v ON_ERROR_STOP=1 "postgres://pdms_owner:devowner@127.0.0.1:5432/pdms" <<'SQL'
INSERT INTO departments (id, code, name) VALUES ('00000000-0000-0000-0000-00000000d001', 'P1T', 'P1 test') ON CONFLICT DO NOTHING;
INSERT INTO users (id, display_name, email) VALUES ('00000000-0000-0000-0000-00000000a001', 'P1 test', 'p1@example.org') ON CONFLICT DO NOTHING;
INSERT INTO documents (id, external_id, document_type_id, department_id, owner_id, title, inherit_acl, is_confidential)
SELECT v.id::uuid, v.ext, (SELECT id FROM document_types WHERE key = 'other'),
       '00000000-0000-0000-0000-00000000d001', '00000000-0000-0000-0000-00000000a001', v.ext, v.inh, v.conf
  FROM (VALUES ('00000000-0000-0000-0000-0000000000f1', 'P1-DEFAULT', true,  false),
               ('00000000-0000-0000-0000-0000000000f2', 'P1-CONF',    true,  true),
               ('00000000-0000-0000-0000-0000000000f3', 'P1-NOINH',   false, false)) AS v(id, ext, inh, conf)
ON CONFLICT DO NOTHING;
SQL
sqlx migrate run --source migrations
docker exec pdms-dev-db psql "postgres://pdms_owner:devowner@127.0.0.1:5432/pdms" -Atc \
  "SELECT external_id, visibility FROM documents WHERE external_id LIKE 'P1-%' ORDER BY 1"
```

Expected output:

```
P1-CONF|restricted
P1-DEFAULT|department
P1-NOINH|restricted
```

Then remove the fixtures:

```bash
docker exec pdms-dev-db psql "postgres://pdms_owner:devowner@127.0.0.1:5432/pdms" -c \
  "DELETE FROM documents WHERE external_id LIKE 'P1-%'; DELETE FROM users WHERE email = 'p1@example.org'; DELETE FROM departments WHERE code = 'P1T';"
```

If `DELETE FROM documents` fails because an audit or version trigger references the rows, leave them. The dev database is disposable (recreate it with the README "Develop" commands).

- [ ] **Step 3: Confirm the authz tests fail.**

Run: `cargo test -p pdms-authz`
Expected: compile errors `no field 'visibility' on type 'DocumentAttrs'` and `no variant 'CountyWide' in enum 'AllowReason'`.

- [ ] **Step 4: Implement in `pdms-authz`.**

`crates/pdms-authz/src/model.rs`: replace the `DocumentAttrs` struct with:

```rust
/// The attributes of a document that authorization depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentAttrs {
    pub id: Uuid,
    pub department_id: Uuid,
    pub owner_id: Uuid,
    pub visibility: Visibility,
}
```

`crates/pdms-authz/src/engine.rs`:

- Change the model import to `use crate::model::{Assignee, DocumentAttrs, Grant, Permission, Policy, PositionAttrs, Principal, Scope, Visibility};`.
- Replace `AllowReason` with:

```rust
/// Why access was allowed. Useful for explanations in the UI and access reviews.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AllowReason {
    Owner,
    Grant(Uuid),
    /// Any active account may view a County-wide document.
    CountyWide,
}
```

- Replace `scope_covers` with:

```rust
/// Whether a grant's scope covers the document.
///
/// Department and global grants reach a document unless it is Restricted;
/// document grants always apply.
pub fn scope_covers(scope: Scope, doc: &DocumentAttrs) -> bool {
    let inherits = doc.visibility.inherits_broad_grants();
    match scope {
        Scope::Document(id) => id == doc.id,
        Scope::Department(id) => inherits && id == doc.department_id,
        Scope::Global => inherits,
    }
}
```

- In `decide`, directly after the owner `if` block (`return Decision::Allow(AllowReason::Owner);`), insert:

```rust
    if perm == Permission::View && doc.visibility == Visibility::CountyWide {
        return Decision::Allow(AllowReason::CountyWide);
    }
```

`crates/pdms-authz/src/sql.rs`:

- Update the doc comment line "The documents table must provide …" to:
  ``/// The documents table must provide `id`, `department_id`, `owner_id`, and `visibility`.``
- Replace the block from `f.sql("(");` through the end of the `.sql(&format!( … ))` call with:

```rust
    f.sql("(");
    if perm == Permission::View {
        f.sql(&format!("{d}.visibility = 'county_wide' OR "));
    }
    if policy.owner_permissions.satisfies(perm) {
        f.sql(&format!("{d}.owner_id = ")).param(user()).sql(" OR ");
    }
    f.sql("EXISTS (SELECT 1 FROM acl_entries acl WHERE acl.permission = ANY((")
        .param(SqlParam::PermissionArray(perm.satisfying()))
        .sql(")::text[]::permission[]) AND (acl.expires_at IS NULL OR acl.expires_at > ")
        .param(SqlParam::Timestamp(now))
        .sql(") AND (acl.subject_user_id = ")
        .param(user())
        .sql(" OR acl.subject_group_id = ANY(")
        .param(SqlParam::UuidArray(principal.groups.iter().copied().collect()))
        .sql(") OR acl.subject_department_id = ANY(")
        .param(SqlParam::UuidArray(principal.departments.iter().copied().collect()))
        .sql(") OR EXISTS (SELECT 1 FROM unnest((")
        .param(SqlParam::UuidArray(principal.roles.iter().map(|r| r.role_id).collect()))
        .sql(")::uuid[], (")
        .param(SqlParam::NullableUuidArray(
            principal.roles.iter().map(|r| r.department_id).collect(),
        ))
        .sql(&format!(
            ")::uuid[]) AS held(role_id, department_id) \
WHERE held.role_id = acl.subject_role_id \
AND (held.department_id IS NULL OR held.department_id = {d}.department_id))) \
AND ((acl.scope = 'document' AND acl.scope_document_id = {d}.id) \
OR ({d}.visibility <> 'restricted' \
AND (acl.scope = 'global' OR (acl.scope = 'department' AND acl.scope_department_id = {d}.department_id))))))"
        ));
    Ok(f)
```

The existing test `placeholders_are_numbered_from_offset` uses `Permission::Edit`, so it is unchanged: still 8 params, still starting `(doc.owner_id = $4 OR `.

`crates/pdms-authz/src/strategies.rs`:

- Import `Visibility`: `use crate::model::{DocumentAttrs, Grant, Permission, Principal, RoleHolding, Scope, Subject, Visibility};`.
- Replace `document()` with the two functions below:

```rust
pub fn visibility() -> impl Strategy<Value = Visibility> {
    prop_oneof![
        6 => Just(Visibility::Department),
        2 => Just(Visibility::CountyWide),
        2 => Just(Visibility::Restricted),
    ]
}

pub fn document() -> impl Strategy<Value = DocumentAttrs> {
    (pick(DOCUMENTS), pick(DEPARTMENTS), pick(USERS), visibility()).prop_map(
        |(id, department_id, owner_id, visibility)| DocumentAttrs {
            id,
            department_id,
            owner_id,
            visibility,
        },
    )
}
```

- [ ] **Step 5: Run the authz tests and confirm they pass.**

Run: `cargo test -p pdms-authz`
Expected: all tests pass, including `county_wide_lets_every_active_account_view_only`, `restricted_ignores_department_and_global_grants`, `county_wide_adds_view_and_nothing_else`, and `county_wide_clause_only_for_view`. Summary: `test result: ok.`

- [ ] **Step 6: Update `pdms-core`.** Make these edits exactly.

`crates/pdms-core/src/documents/access.rs`, in `load`:

```rust
pub(crate) async fn load(core: &Core, id: Uuid) -> CoreResult<Option<DocRef>> {
    let row = sqlx::query!(
        r#"SELECT id, department_id, owner_id, visibility::text AS "visibility!", external_id, title
             FROM documents WHERE id = $1 AND deleted_at IS NULL"#,
        id
    )
    .fetch_optional(&core.db)
    .await?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(DocRef {
        attrs: DocumentAttrs {
            id: r.id,
            department_id: r.department_id,
            owner_id: r.owner_id,
            visibility: r.visibility.parse().map_err(CoreError::internal)?,
        },
        external_id: r.external_id,
        title: r.title,
    }))
}
```

`crates/pdms-core/src/documents/service.rs`:

- Imports: `use pdms_authz::{Capability, Permission, PermissionSet, Scope, Subject, Visibility};`.
- `Document` struct: replace the two lines `pub inherit_acl: bool,` and `pub is_confidential: bool,` with `pub visibility: Visibility,`.
- In `fetch`'s SQL, replace `d.inherit_acl, d.is_confidential,` with `d.visibility::text AS "visibility!",`. In the `Document { … }` literal, replace the two field lines with `visibility: r.visibility.parse().map_err(CoreError::internal)?,`.
- In the version-snapshot `json!({ … })` (the one ending with `"inherit_acl": d.inherit_acl,`), replace the two lines `"is_confidential": …` and `"inherit_acl": …` with `"visibility": d.visibility.as_str(),`.
- `NewDocument`: add this field after `pub status: DocumentStatus,`:

```rust
    /// Who can see it (D-085); chosen at upload.
    pub visibility: Visibility,
```

- In `create`'s `INSERT INTO documents`:
  - add `visibility` to the column list after `created_by`, giving `…, search_extra, created_by, visibility, archived_at)`;
  - change `VALUES (…, $18, NULL)` to `VALUES ($1, …, $18, $19::text::document_visibility, NULL)`;
  - add the argument `new.visibility.as_str(),` after `ctx.user_id(),`.
- In `create`'s `DocRef { attrs: pdms_authz::DocumentAttrs { … } }`, replace the two flag lines with `visibility: new.visibility,`. In the `document.created` event chain, after `.change("status", AuditValue::Null, new.status.as_str())`, add:

```rust
        .change("visibility", AuditValue::Null, new.visibility.as_str())
```

- Replace the whole `set_access_settings` function with:

```rust
/// Who can see the document without an explicit share (D-085).
/// Requires `manage_permissions`.
pub async fn set_visibility(core: &Core, ctx: &Ctx, id: Uuid, to: Visibility) -> CoreResult<()> {
    let doc_ref = access::authorize(core, ctx, id, Permission::ManagePermissions).await?;
    let from = doc_ref.attrs.visibility;
    if from == to {
        return Ok(());
    }
    let mut tx = core.db.begin().await?;
    sqlx::query!(
        "UPDATE documents SET visibility = $2::text::document_visibility, updated_at = now() WHERE id = $1",
        id,
        to.as_str()
    )
    .execute(&mut *tx)
    .await?;
    refresh_search(&mut tx, id).await?;
    pdms_audit::append(
        &mut tx,
        vec![
            doc_ref
                .event(ctx, Category::Permission, "document.visibility_changed")
                .change("visibility", from.as_str(), to.as_str()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
```

`crates/pdms-core/src/documents/search.rs`:

- Imports: `use pdms_authz::{Permission, Visibility, visible_documents_predicate};`.
- In `DocumentSummary`, replace `pub is_confidential: bool,` with:

```rust
    #[sqlx(try_from = "String")]
    pub visibility: Visibility,
```

- In the `QueryBuilder::new("SELECT …")` text, replace `d.is_confidential,` with `d.visibility::text AS visibility,`.
- In the CSV header array, replace `"Confidential",` with `"Visibility",`. In the row cells, replace `if it.is_confidential { "yes" } else { "no" }.to_owned(),` with `it.visibility.label().to_owned(),`.

`crates/pdms-core/src/calendar.rs`:

- Imports: `use pdms_authz::{Permission, Visibility};`.
- Line 432: `.filter(|d| d.visibility != Visibility::Restricted)`.
- Line 607 (test fixture): `visibility: Visibility::Department,` in place of `is_confidential: false,`.

`crates/pdms-core/src/notifications/planner.rs`:

- In `struct DocDates`, replace the two bool fields with `visibility: String,`.
- In `load_docs`'s SQL, replace `inherit_acl, is_confidential,` with `visibility::text AS "visibility!",`.
- Imports: `use crate::error::{CoreError, CoreResult};`.
- In `Target::document`, replace the two attr lines with `visibility: d.visibility.parse().map_err(CoreError::internal)?,`.

`crates/pdms-core/src/ocr/mod.rs`:

- In the query at ~251, replace `d.owner_id, d.inherit_acl, d.is_confidential` with `d.owner_id, d.visibility::text AS "visibility!"`.
- In the `pdms_authz::DocumentAttrs { … }` at ~422, replace the two flag lines with `visibility: v.visibility.parse().map_err(CoreError::internal)?,`.

`crates/pdms-core/src/graph_calendar.rs:353`: replace `NOT d.is_confidential` with `d.visibility <> 'restricted'`. The module is removed in sub-project 3; this only keeps it compiling.

`crates/pdms-core/src/demo.rs:479`: replace `documents::set_access_settings(core, ctx, ids[3], true, true).await?;` with:

```rust
    documents::set_visibility(core, ctx, ids[3], pdms_authz::Visibility::Restricted).await?;
```

Also update the comment above it to `// Restrict the investigation procedures, and share the grant with Legal for review.`

- [ ] **Step 7: Mechanical edits for the new field and the renamed function.** Run from the repository root:

```bash
perl -0pi -e 's/(?<!struct )(NewDocument \{\n)([ \t]*)/$1$2visibility: Default::default(),\n$2/g' \
  crates/pdms-web/tests/phase4.rs crates/pdms-web/tests/schedule.rs crates/pdms-web/src/routes/api.rs \
  crates/pdms-web/tests/phase3.rs crates/pdms-core/tests/documents.rs crates/pdms-core/tests/phase4.rs \
  crates/pdms-core/src/demo.rs crates/pdms-web/src/routes/documents.rs crates/pdms-core/tests/phase5.rs \
  crates/pdms-core/tests/workflow.rs crates/pdms-core/tests/notifications.rs
grep -rn "visibility: Default::default()," crates | wc -l
```

Expected: `23`.

```bash
perl -pi -e 's/documents::set_access_settings\(([^,]+), ([^,]+), ([^,]+), true, true\)/documents::set_visibility($1, $2, $3, pdms_authz::Visibility::Restricted)/g' \
  crates/pdms-core/tests/documents.rs crates/pdms-core/tests/phase4.rs \
  crates/pdms-core/tests/notifications.rs crates/pdms-core/tests/phase5.rs
grep -rn "set_access_settings" crates
```

Expected: no output.

`crates/pdms-core/tests/documents.rs`: in the list `for a in ["document.access_settings_changed", …]` (around line 706), replace that string with `"document.visibility_changed"`.

`crates/pdms-core/tests/directory.rs`:

- Import: `use pdms_authz::{DocumentAttrs, Permission, Policy, Scope, Subject, Visibility, decide};`.
- Then run:

```bash
perl -0pi -e 's/inherit_acl: true,\n(\s*)is_confidential: false,/visibility: Visibility::Department,/g' crates/pdms-core/tests/directory.rs
```

`crates/pdms-core/tests/authz_sql.rs`, in `load_case`: replace the documents `INSERT … ON CONFLICT` statement and its binds with:

```rust
        sqlx::query(
            "INSERT INTO documents (id, department_id, owner_id, visibility,
                                    external_id, document_type_id, title)
             VALUES ($1, $2, $3, $4::text::document_visibility, $1::text,
                     (SELECT id FROM document_types WHERE key = 'other'), 'Doc')
             ON CONFLICT (id) DO UPDATE SET department_id = EXCLUDED.department_id,
                owner_id = EXCLUDED.owner_id, visibility = EXCLUDED.visibility",
        )
        .bind(d.id)
        .bind(d.department_id)
        .bind(d.owner_id)
        .bind(d.visibility.as_str())
        .execute(owner)
        .await
        .unwrap();
```

- [ ] **Step 8: Update `pdms-web` so it compiles, keeping today's interface.**

`crates/pdms-web/src/routes/documents.rs`:

- Imports: `use pdms_authz::{Capability, Permission, Subject, Visibility};`.
- In the list mapping (around line 414): `confidential: d.visibility == Visibility::Restricted,`.
- Replace `AccessForm` and `access_settings` with:

```rust
#[derive(Deserialize)]
pub struct VisibilityForm {
    #[serde(default)]
    visibility: String,
}

pub async fn set_visibility(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Form(f): Form<VisibilityForm>,
) -> AppResult<Response> {
    let r = match f.visibility.parse::<Visibility>() {
        Ok(v) => documents::set_visibility(&state.core, &user.ctx, id, v).await,
        Err(_) => Err(CoreError::validation("choose who can see the document")),
    };
    done(&state, &back(id), r, "Visibility saved.")
}
```

- `crates/pdms-web/src/lib.rs:206`: replace `.route("/{id}/access", post(documents::access_settings))` with `.route("/{id}/visibility", post(documents::set_visibility))`.
- `crates/pdms-web/src/routes/exports.rs:91`: `confidential: d.visibility == pdms_authz::Visibility::Restricted,`.
- `crates/pdms-web/src/routes/api.rs:681`: `is_confidential: d.visibility == pdms_authz::Visibility::Restricted,`. Task 5 adds the full `visibility` field.

`crates/pdms-web/templates/documents/show.html`:

- Replace lines 7–8 (the Confidential and "Inheritance off" badges) with:

```html
    {% if d.visibility.as_str() == "restricted" %}<span class="badge confidential">Restricted</span>{% endif %}
    {% if d.visibility.as_str() == "county_wide" %}<span class="badge">County-wide</span>{% endif %}
```

- Replace the `<form method="post" action="/documents/{{ d.id }}/access" …>…</form>` inside the `access-h` section with:

```html
  <form method="post" action="/documents/{{ d.id }}/visibility" class="stack">
    <input type="hidden" name="_csrf" value="{{ l.csrf }}">
    <label for="visibility">Who can see it</label>
    <select id="visibility" name="visibility">
      <option value="department" {% if d.visibility.as_str() == "department" %}selected{% endif %}>Department: its department and anyone it is shared with</option>
      <option value="county_wide" {% if d.visibility.as_str() == "county_wide" %}selected{% endif %}>County-wide: every county employee can view and download it</option>
      <option value="restricted" {% if d.visibility.as_str() == "restricted" %}selected{% endif %}>Restricted: only the owner and people it is shared with</option>
    </select>
    <div><button type="submit">Save visibility</button></div>
  </form>
```

- [ ] **Step 9: Build, refresh offline data, and run the whole suite.**

```bash
cargo build --workspace --all-targets
cargo sqlx prepare --workspace -- --all-targets
cargo test --workspace --locked
```

Expected:

- The build succeeds.
- `prepare` prints `query data written to .sqlx in the current directory`.
- All tests pass (about 231: the previous 228 plus the new authz tests).
- The equivalence test prints `equivalence: N allowed and M denied …` with both above 500.

If a `query!` macro fails to build with `column "inherit_acl" does not exist`, the dev database lacks migration 0008. Re-run `sqlx migrate run --source migrations` (Step 2).

- [ ] **Step 10: Lint and commit.**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
git add -A migrations/0008_document_visibility.sql crates .sqlx
git commit -m "authz: replace confidential/inherit flags with document visibility (D-085)"
```

---

### Task 3: Owners choose visibility, at upload and later

**Files:**
- Modify: `crates/pdms-core/src/documents/service.rs` (`set_visibility` authorization)
- Test: `crates/pdms-core/tests/documents.rs` (append)

**Interfaces:**
- Consumes: `set_visibility`, `NewDocument.visibility` (Task 2).
- Produces: `set_visibility` allowed for the document's owner **or** a `manage_permissions` holder; denials audited as `access.denied` with `required_permission = manage_permissions`.

- [ ] **Step 1: Write the failing tests.** Append to `crates/pdms-core/tests/documents.rs`, and add `Visibility` to its `use pdms_authz::{…}` line:

```rust
async fn new_doc_with(
    core: &Core,
    ctx: &Ctx,
    input: MetadataInput,
    owner: Uuid,
    visibility: Visibility,
) -> Uuid {
    documents::create(
        core,
        ctx,
        NewDocument {
            metadata: input,
            owner_id: owner,
            status: DocumentStatus::Active,
            visibility,
            tags: vec![],
            file: Some(upload(core, "policy.pdf", &pdf("leave policy")).await),
            note: String::new(),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn county_wide_documents_are_viewable_by_every_department() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let (outsider, _) = user_in_new_department(&core, "OPS", "o@x.org").await;
    let other = type_id(&core, "other").await;
    let ctx_alice = as_user(&core, alice).await;
    let shared = new_doc_with(&core, &ctx_alice, meta(other, dept, "Leave policy"), alice, Visibility::CountyWide).await;
    let internal = new_doc_with(&core, &ctx_alice, meta(other, dept, "HR roster"), alice, Visibility::Department).await;

    let ctx_out = as_user(&core, outsider).await;
    assert_eq!(visible_ids(&core, &ctx_out, DocumentFilter::default()).await, vec![shared]);
    assert!(documents::view(&core, &ctx_out, shared).await.is_ok());
    assert!(documents::open_file(&core, &ctx_out, shared, None).await.is_ok(), "download allowed");
    assert!(matches!(
        documents::view(&core, &ctx_out, internal).await,
        Err(CoreError::Forbidden(_))
    ));
    // View only: editing details is still refused.
    let doc = documents::view(&core, &ctx_out, shared).await.unwrap().document;
    let mut m = meta(other, dept, "Renamed by outsider");
    m.description = doc.description.clone();
    assert!(matches!(
        documents::update(&core, &ctx_out, shared, m, doc.row_version).await,
        Err(CoreError::Forbidden(_))
    ));
    let created = audit_events(&core, "document.created").await;
    assert!(created.iter().any(|c| c.contains("county_wide")), "visibility recorded at creation");
}

#[tokio::test]
async fn owner_can_change_visibility_and_it_is_audited() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let (outsider, _) = user_in_new_department(&core, "OPS", "o@x.org").await;
    let other = type_id(&core, "other").await;
    let ctx_alice = as_user(&core, alice).await;
    let id = new_doc_with(&core, &ctx_alice, meta(other, dept, "Leave policy"), alice, Visibility::Department).await;

    documents::set_visibility(&core, &ctx_alice, id, Visibility::CountyWide).await.unwrap();
    assert!(documents::view(&core, &as_user(&core, outsider).await, id).await.is_ok());
    documents::set_visibility(&core, &ctx_alice, id, Visibility::Restricted).await.unwrap();
    assert!(matches!(
        documents::view(&core, &as_user(&core, outsider).await, id).await,
        Err(CoreError::Forbidden(_))
    ));
    let changes = audit_events(&core, "document.visibility_changed").await;
    assert_eq!(changes.len(), 2);
    assert!(changes[0].contains("department") && changes[0].contains("county_wide"));
    assert!(changes[1].contains("county_wide") && changes[1].contains("restricted"));
}

#[tokio::test]
async fn editor_who_is_not_owner_cannot_change_visibility() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let bob = directory::create_user(&core.db, &system(), "bob", "bob@x.org", Some(dept))
        .await
        .unwrap();
    let other = type_id(&core, "other").await;
    let id = new_doc_with(&core, &as_user(&core, alice).await, meta(other, dept, "Roster"), alice, Visibility::Department).await;
    let ctx_bob = as_user(&core, bob).await; // department editor, not the owner
    assert!(matches!(
        documents::set_visibility(&core, &ctx_bob, id, Visibility::CountyWide).await,
        Err(CoreError::Forbidden(_))
    ));
    let doc = documents::view(&core, &ctx_bob, id).await.unwrap().document;
    assert_eq!(doc.visibility, Visibility::Department);
    assert!(audit_actions(&core).await.contains(&"access.denied".to_owned()));
}

#[tokio::test]
async fn setting_same_visibility_is_a_no_op() {
    let (core, _db) = core().await;
    let (dept, alice) = editing_department(&core, "HR", "alice@x.org").await;
    let other = type_id(&core, "other").await;
    let ctx = as_user(&core, alice).await;
    let id = new_doc_with(&core, &ctx, meta(other, dept, "Roster"), alice, Visibility::Department).await;
    let before = documents::view(&core, &ctx, id).await.unwrap().document.updated_at;
    documents::set_visibility(&core, &ctx, id, Visibility::Department).await.unwrap();
    assert_eq!(documents::view(&core, &ctx, id).await.unwrap().document.updated_at, before);
    assert!(audit_events(&core, "document.visibility_changed").await.is_empty());
}

/// `changes` of every audit entry with this action, as JSON text, oldest first.
async fn audit_events(core: &Core, action: &str) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT coalesce(changes::text, '') FROM audit_log WHERE action = $1 ORDER BY seq",
    )
    .bind(action)
    .fetch_all(&core.db)
    .await
    .unwrap()
}
```

`documents::update` is `update(core, ctx, id, input: MetadataInput, row_version: i32)` (`service.rs:881`). The helpers `pdf`, `upload`, `as_user`, `audit_actions`, and `user_in_new_department` come from `tests/common.rs`, which this file already uses; `editing_department`, `meta`, `type_id`, and `visible_ids` are defined at the top of `tests/documents.rs`.

- [ ] **Step 2: Run the tests and confirm the owner tests fail.**

Run: `cargo test -p pdms-core --test documents visibility`
Expected:

- `owner_can_change_visibility_and_it_is_audited` FAILS with `called Result::unwrap() on an Err value: Forbidden(...)`, because owners hold only view and edit.
- `editor_who_is_not_owner_cannot_change_visibility` and `setting_same_visibility_is_a_no_op` PASS.
- `county_wide_documents_are_viewable_by_every_department` PASSES, because Task 2 already did the engine and create work.

- [ ] **Step 3: Implement the owner rule.** In `crates/pdms-core/src/documents/service.rs`, replace the first line of `set_visibility`'s body (`let doc_ref = access::authorize(core, ctx, id, Permission::ManagePermissions).await?;`) with:

```rust
    // The owner chooses visibility (D-085); otherwise `manage_permissions` is needed.
    let is_owner = match (access::load(core, id).await?, ctx.user_id()) {
        (Some(d), Some(user)) => d.attrs.owner_id == user,
        (None, _) => return Err(CoreError::NotFound),
        _ => false,
    };
    let doc_ref = if is_owner {
        access::authorize(core, ctx, id, Permission::View).await?
    } else {
        access::authorize(core, ctx, id, Permission::ManagePermissions).await?
    };
```

and update the doc comment to:

```rust
/// Who can see the document without an explicit share (D-085). Allowed for
/// the document's owner or anyone with `manage_permissions` on it.
```

Inactive owners are still refused, because `authorize` denies inactive principals.

- [ ] **Step 4: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-core --test documents`
Expected: `test result: ok.`, with all four new tests passing and the existing ones still passing.

- [ ] **Step 5: Commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-core
git commit -m "documents: owners choose visibility; county-wide end-to-end tests"
```

---

### Task 4: Web pages: choose at upload, change later, see badges

**Files:**
- Modify: `crates/pdms-web/src/routes/documents.rs` (`NewPage`, `render_new`, `create`, `ShowPage`, `show`, `Row`)
- Modify: `crates/pdms-web/src/routes/exports.rs:41,91` (`Candidate`)
- Modify: `crates/pdms-web/templates/documents/new.html`, `documents/show.html`, `documents/list.html:60`, `exports.html:21`, `admin/grant_form.html:9`, `admin/grants.html:3`, `feeds.html:3`
- Test: `crates/pdms-web/tests/documents.rs` (append)

**Interfaces:**
- Consumes: `Visibility` (`ALL`, `as_str`, `label`), `documents::set_visibility` (owner or manage).
- Produces: the create form field `visibility`; a document page form visible to the owner or a manage holder; Department/County-wide/Restricted badges in lists.

- [ ] **Step 1: Write the failing web tests.** Append to `crates/pdms-web/tests/documents.rs`:

```rust
#[tokio::test]
async fn owner_publishes_county_wide_then_restricts_through_the_pages() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "HR", "alice@x.org").await;
    let mut a = Browser::new(&app);
    a.login("alice@x.org", &secret).await;
    let csrf = a.get("/documents/new").await.csrf();
    let t = type_id(&app, "policy").await;
    let (d, o) = (dept.to_string(), uid.to_string());
    let doc = a
        .multipart(
            "/documents",
            &[
                ("_csrf", csrf.as_str()),
                ("title", "Leave policy"),
                ("document_type_id", t.as_str()),
                ("department_id", d.as_str()),
                ("owner_id", o.as_str()),
                ("status", "active"),
                ("visibility", "county_wide"),
            ],
            Some(("leave.pdf", &pdf("leave"))),
        )
        .await
        .location()
        .to_owned();

    let ops = directory::create_department(&app.core.db, &system(), "OPS", "OPS").await.unwrap();
    let (_, bob_secret) = user(&app.core, "bob@x.org", Some(ops), None).await;
    let mut b = Browser::new(&app);
    b.login("bob@x.org", &bob_secret).await;
    let list = b.get("/documents").await;
    assert!(list.body.contains("Leave policy") && list.body.contains("County-wide"));
    assert_eq!(b.get(&format!("{doc}/download")).await.status, StatusCode::OK);

    // The owner sees the form on the document page and restricts it.
    let page = a.get(&doc).await;
    assert!(page.body.contains(&format!("action=\"{doc}/visibility\"")));
    let csrf = page.csrf();
    a.post(&format!("{doc}/visibility"), &[("_csrf", csrf.as_str()), ("visibility", "restricted")]).await;
    assert_eq!(b.get(&doc).await.status, StatusCode::FORBIDDEN);
    assert!(actions(&app.core).await.contains(&"document.visibility_changed".to_owned()));
}

#[tokio::test]
async fn unknown_visibility_value_is_rejected() {
    let app = app().await;
    let (dept, uid, secret) = editor(&app, "HR", "alice@x.org").await;
    let mut a = Browser::new(&app);
    a.login("alice@x.org", &secret).await;
    let csrf = a.get("/documents/new").await.csrf();
    let t = type_id(&app, "policy").await;
    let (d, o) = (dept.to_string(), uid.to_string());
    let r = a
        .multipart(
            "/documents",
            &[
                ("_csrf", csrf.as_str()),
                ("title", "Leave policy"),
                ("document_type_id", t.as_str()),
                ("department_id", d.as_str()),
                ("owner_id", o.as_str()),
                ("visibility", "public"),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body.contains("choose who can see the document"));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM documents").fetch_one(&app.core.db).await.unwrap();
    assert_eq!(n, 0, "nothing created");
}
```

`Resp` exposes `status`, `headers`, `body: String`, `csrf()`, and `location()` (`tests/support.rs:85-107`).

- [ ] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-web --test documents visibility`
Expected:

- `owner_publishes_county_wide_then_restricts_through_the_pages` FAILS. The create handler ignores `visibility`, so the document is Department and Bob's list lacks it.
- `unknown_visibility_value_is_rejected` FAILS, because the value is ignored and the document is created.

- [ ] **Step 3: Implement the create form.** In `crates/pdms-web/src/routes/documents.rs`, add these helpers below `struct NewPage`:

```rust
/// (value, label, explanation) for the visibility choices, in display order.
fn visibility_options() -> Vec<(&'static str, &'static str, &'static str)> {
    Visibility::ALL
        .into_iter()
        .map(|v| {
            let help = match v {
                Visibility::Department => "its department and anyone you share it with",
                Visibility::CountyWide => "every county employee can view and download it",
                Visibility::Restricted => "only the owner and people it is shared with",
            };
            (v.as_str(), v.label(), help)
        })
        .collect()
}

fn parse_visibility(s: &str) -> CoreResult<Visibility> {
    if s.is_empty() {
        return Ok(Visibility::Department);
    }
    s.parse()
        .map_err(|_| CoreError::validation("choose who can see the document"))
}
```

- Add the field `visibility_options: Vec<(&'static str, &'static str, &'static str)>,` to `NewPage`, and set `visibility_options: visibility_options(),` in `render_new`'s `NewPage { … }`.
- In `create`, after the `let status = match …;` statement, add `let visibility = parse_visibility(form.v("visibility"))?;`.
- In the `NewDocument { … }` literal, replace `visibility: Default::default(),` with `visibility,`.

In `crates/pdms-web/templates/documents/new.html`, insert directly after the closing `</div>` of the first `field-row` (after the "Start as" field):

```html
  <fieldset>
    <legend>Who can see it</legend>
    {% for v in visibility_options %}
    <label class="check"><input type="radio" name="visibility" value="{{ v.0 }}"
      {% if form.v("visibility") == v.0 || (form.v("visibility") == "" && v.0 == "department") %}checked{% endif %}>
      <strong>{{ v.1 }}</strong>: {{ v.2 }}</label>
    {% endfor %}
  </fieldset>
```

- [ ] **Step 4: Implement the document page and the badges.**

`ShowPage`:

- Add the field `can_set_visibility: bool,`.
- In `show`, set it with `can_set_visibility: is_owner || can_manage,`. `is_owner` is computed at ~line 810, so move that `let` above the `ShowPage { … }` literal if it is not already.

`documents/show.html`: wrap the visibility `<form>` from Task 2 Step 8 in its own section, placed **before** `{% if can_manage %}<section aria-labelledby="access-h">`. Then delete the form from inside the access section:

```html
{% if can_set_visibility %}
<section aria-labelledby="visibility-h">
  <h2 id="visibility-h">Who can see it</h2>
  <form method="post" action="/documents/{{ d.id }}/visibility" class="stack">
    <input type="hidden" name="_csrf" value="{{ l.csrf }}">
    <label for="visibility" class="visually-hidden">Visibility</label>
    <select id="visibility" name="visibility">
      <option value="department" {% if d.visibility.as_str() == "department" %}selected{% endif %}>Department: its department and anyone it is shared with</option>
      <option value="county_wide" {% if d.visibility.as_str() == "county_wide" %}selected{% endif %}>County-wide: every county employee can view and download it</option>
      <option value="restricted" {% if d.visibility.as_str() == "restricted" %}selected{% endif %}>Restricted: only the owner and people it is shared with</option>
    </select>
    <div><button type="submit">Save visibility</button></div>
  </form>
</section>
{% endif %}
```

List badges:

- `Row` (the list-row struct, line ~351): replace `pub confidential: bool,` with `pub visibility: &'static str,` (the label). In the list mapping, use `visibility: d.visibility.label(),`.
- `documents/list.html:60`: replace `{% if r.confidential %} <span class="badge confidential">Confidential</span>{% endif %}` with:

```html
{% if r.visibility != "Department" %} <span class="badge{% if r.visibility == "Restricted" %} confidential{% endif %}">{{ r.visibility }}</span>{% endif %}
```

- `exports.rs` `Candidate`: replace `pub confidential: bool,` with `pub visibility: &'static str,`, and at line 91 use `visibility: d.visibility.label(),`. In `exports.html:21`, use the same replacement as list.html, with `c.` instead of `r.`.

Wording:

- `admin/grant_form.html:9`: `<option value="global">All documents except Restricted ones</option>`.
- `admin/grants.html:3`: replace the sentence about confidential with: `Grants never reach Restricted documents.`
- `feeds.html:3`: replace `never includes confidential documents` with `never includes Restricted documents`.

- [ ] **Step 5: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-web`
Expected: `test result: ok.` for every web test binary, including the two new tests.

- [ ] **Step 6: Commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-web
git commit -m "web: choose visibility at upload, change it on the document page, show badges"
```

---

### Task 5: API exposes and accepts visibility

**Files:**
- Modify: `crates/pdms-web/src/routes/api.rs` (`DocumentOut`, `DocumentSummaryOut`, `CreateDocumentIn`, `create_document`, the mapping at ~681)
- Test: `crates/pdms-web/tests/phase4.rs` (append)

**Interfaces:**
- Consumes: `Visibility`, `NewDocument.visibility`.
- Produces: JSON `visibility` (`"department"`, `"county_wide"`, `"restricted"`) on every document in list and get responses. `is_confidential` stays on `DocumentOut` as a compatibility field (`true` iff restricted). `CreateDocumentIn.visibility` is optional and defaults to `department`.

- [ ] **Step 1: Write the failing API tests.** Append to `crates/pdms-web/tests/phase4.rs`. It reuses the file's existing helpers: `call(b, method, path, token, json) -> Resp`, `json(&Resp) -> Value`, `token(app, user, scopes) -> String`, and `user(...)` from `support`.

```rust
/// An HR department whose members may create and edit; returns (dept, user).
async fn hr_editor(app: &App) -> (Uuid, Uuid) {
    let hr = directory::create_department(&app.core.db, &system(), "HR", "HR")
        .await
        .unwrap();
    for p in [Permission::Create, Permission::Edit] {
        acl::grant(&app.core.db, &system(), Subject::Department(hr), Scope::Department(hr), p, None, "t")
            .await
            .unwrap();
    }
    let (uid, _) = user(&app.core, "alice@x.org", Some(hr), None).await;
    (hr, uid)
}

#[tokio::test]
async fn api_reports_and_accepts_visibility() {
    let app = app().await;
    let (hr, alice) = hr_editor(&app).await;
    let t = token(&app, alice, &["documents:read", "documents:write"]).await;
    let mut b = Browser::new(&app);
    let body = format!(
        r#"{{"title":"Leave policy","document_type":"policy","department_id":"{hr}","status":"active","visibility":"county_wide"}}"#
    );
    let r = call(&mut b, "POST", "/api/v1/documents", Some(&t), Some(&body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let id = json(&r)["id"].as_str().unwrap().to_owned();
    let d = json(&call(&mut b, "GET", &format!("/api/v1/documents/{id}"), Some(&t), None).await);
    assert_eq!(d["visibility"], "county_wide");
    assert_eq!(d["is_confidential"], false);
    let list = json(&call(&mut b, "GET", "/api/v1/documents", Some(&t), None).await);
    assert_eq!(list["items"][0]["visibility"], "county_wide");

    // Someone in another department sees it through the API too.
    let ops = directory::create_department(&app.core.db, &system(), "OPS", "OPS")
        .await
        .unwrap();
    let (bob, _) = user(&app.core, "bob@x.org", Some(ops), None).await;
    let tb = token(&app, bob, &["documents:read"]).await;
    let theirs = json(&call(&mut b, "GET", "/api/v1/documents", Some(&tb), None).await);
    assert_eq!(theirs["total"], 1);
    assert_eq!(theirs["items"][0]["title"], "Leave policy");
}

#[tokio::test]
async fn api_rejects_unknown_visibility() {
    let app = app().await;
    let (hr, alice) = hr_editor(&app).await;
    let t = token(&app, alice, &["documents:write"]).await;
    let mut b = Browser::new(&app);
    for bad in ["public", "", "County-wide"] {
        let body = format!(
            r#"{{"title":"X","document_type":"policy","department_id":"{hr}","visibility":"{bad}"}}"#
        );
        let r = call(&mut b, "POST", "/api/v1/documents", Some(&t), Some(&body)).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}: {}", r.body);
        assert!(r.body.contains("visibility"), "{bad:?}: {}", r.body);
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM documents")
        .fetch_one(&app.core.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}
```

- [ ] **Step 2: Run the tests and confirm they fail.**

Run: `cargo test -p pdms-web --test phase4 visibility`
Expected: both tests FAIL. `doc["visibility"]` is `null`, and unknown values are accepted with 201.

- [ ] **Step 3: Implement.** In `crates/pdms-web/src/routes/api.rs`:

- `DocumentSummaryOut`: add the field below, and set it in every place a `DocumentSummaryOut` is built. Search `DocumentSummaryOut {`: use `visibility: d.visibility.as_str().to_owned(),`. The list mapping is from `DocumentSummary` and the get mapping is from `Document`; both have `.visibility`.

```rust
    /// `department`, `county_wide` (every county account may view), or `restricted`.
    pub visibility: String,
```

- `DocumentOut`: keep `is_confidential`, with the doc comment `/// Deprecated: true when visibility is restricted.`
- `CreateDocumentIn`: add:

```rust
    /// `department` (default), `county_wide`, or `restricted`.
    #[serde(default)]
    pub visibility: Option<String>,
```

- `create_document`: after the `status` match, add:

```rust
    let visibility = match b.visibility.as_deref() {
        None => pdms_authz::Visibility::Department,
        Some(v) => v.parse().map_err(|_| {
            CoreError::validation("visibility must be department, county_wide, or restricted")
        })?,
    };
```

  Then, in its `NewDocument { … }`, replace `visibility: Default::default(),` with `visibility,`.

- [ ] **Step 4: Run the tests and confirm they pass.**

Run: `cargo test -p pdms-web --test phase4`
Expected: `test result: ok.`

- [ ] **Step 5: Commit.**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets --all-features -- -D warnings
git add crates/pdms-web
git commit -m "api: expose and accept document visibility"
```

---

### Task 6: Demo data, documentation, and final verification

**Files:**
- Modify: `crates/pdms-core/src/demo.rs` (make "Personnel Policy Manual", `ids[2]`, County-wide)
- Modify: `docs/ARCHITECTURE.md` §6 (authorization model), §12 data-model table row for phase 1
- Modify: `docs/ADMIN_GUIDE.md:131,152-156,197,361,442,554` (confidential/inheritance wording)
- Modify: `docs/ROADMAP.md` §0.3 (P1 status)
- Regenerate: `.sqlx/`

**Interfaces:**
- Consumes: everything above.
- Produces: the demo shows all three levels; docs describe them; ROADMAP records P1 done.

- [ ] **Step 1: Write the failing demo test.** Append to `crates/pdms-core/tests/documents.rs`:

```rust
#[tokio::test]
async fn demo_seed_shows_all_three_visibilities() {
    let (core, _db) = core().await;
    pdms_core::demo::seed(&core).await.unwrap();
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT title, visibility::text FROM documents
          WHERE title IN ('Personnel Policy Manual', 'Workplace Investigation Procedures',
                          'Asphalt Supply Contract FY2026')
          ORDER BY title",
    )
    .fetch_all(&core.db)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("Asphalt Supply Contract FY2026".to_owned(), "department".to_owned()),
            ("Personnel Policy Manual".to_owned(), "county_wide".to_owned()),
            ("Workplace Investigation Procedures".to_owned(), "restricted".to_owned()),
        ]
    );
}
```

Run: `cargo test -p pdms-core --test documents demo_seed_shows_all_three_visibilities`
Expected: FAIL. "Personnel Policy Manual" is `department`.

- [ ] **Step 2: Demo data.** In `crates/pdms-core/src/demo.rs`, after the line added in Task 2 that restricts `ids[3]`, add:

```rust
    // Everyone in the organization should be able to read the personnel policy.
    documents::set_visibility(core, ctx, ids[2], pdms_authz::Visibility::CountyWide).await?;
```

Run: `cargo test -p pdms-core --test documents demo_seed_shows_all_three_visibilities`
Expected: PASS.

- [ ] **Step 3: Docs.**

`docs/ARCHITECTURE.md` §6, "Decision rule" and the bullets under "So:":

- Replace "the document inherits ACLs and is not confidential" (both occurrences) with "the document is not Restricted".
- Replace the "Breaking inheritance" and "Confidential" bullets with:

```markdown
- **Visibility (D-085):** every document is *Department* (default), *County-wide*
  (additionally, every active account may **view** it; nothing else), or
  *Restricted* (department and global grants never apply, so it needs explicit
  grants; admins included). The owner chooses at upload and can change it later;
  anyone with `manage_permissions` can too. Changes are audited
  (`document.visibility_changed`).
```

In §12's phase 1 row, replace ``incl. `external_id`, `inherit_acl`, `is_confidential`, …`` with ``incl. `external_id`, `visibility` (0008; replaced `inherit_acl`/`is_confidential`), …``.

`docs/ADMIN_GUIDE.md`: in each listed line, replace "confidential documents" with "Restricted documents" and "non-confidential" with "non-Restricted". Rewrite line 197's permission row as:

```markdown
| Share with a person, group, department, or role; transfer ownership | `manage_permissions` |
| Change who can see it (Department, County-wide, Restricted) | the owner, or `manage_permissions` |
```

`docs/ROADMAP.md` §0.3: add this row above sub-project 1, so the plan's progress is visible:

```markdown
| 1a | **Document visibility** (plan P1) | Department / County-wide / Restricted | **Done** | [P1 plan](superpowers/plans/2026-10-14-p1-document-visibility.md) |
```

Leave sub-projects 1 and 2 at "Planned"; they become "In progress" or "Done" as P2–P4 land.

- [ ] **Step 4: Full verification.**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo sqlx prepare --workspace --check -- --all-targets
SQLX_OFFLINE=true cargo test --workspace --locked
```

Expected:

- `fmt` prints nothing.
- `clippy` finishes with no warnings.
- `prepare --check` exits 0.
- `cargo test` reports every binary `test result: ok.`: 0 failed, and 2 ignored (opt-in OCR and ClamAV).

- [ ] **Step 5: Commit.**

```bash
git add crates/pdms-core/src/demo.rs crates/pdms-core/tests/documents.rs docs .sqlx
git commit -m "docs: document visibility levels; demo shows county-wide and restricted"
```
