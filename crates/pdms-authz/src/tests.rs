// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Unit and property tests for the decision engine.

use std::collections::BTreeSet;

use chrono::Duration;
use proptest::prelude::*;
use uuid::Uuid;

use crate::strategies::{self, DEPARTMENTS, DOCUMENTS, GROUPS, ROLES, USERS, now};
use crate::*;

fn user(id: Uuid, departments: &[Uuid]) -> Principal {
    Principal {
        user_id: id,
        active: true,
        departments: departments.iter().copied().collect(),
        groups: BTreeSet::new(),
        roles: Vec::new(),
    }
}

fn doc(id: Uuid, department_id: Uuid, owner_id: Uuid) -> DocumentAttrs {
    DocumentAttrs {
        id,
        department_id,
        owner_id,
        visibility: Visibility::Department,
    }
}

fn grant(subject: Subject, scope: Scope, permission: Permission) -> Grant {
    Grant {
        id: Uuid::new_v4(),
        subject,
        scope,
        permission,
        expires_at: None,
    }
}

fn allowed(p: &Principal, perm: Permission, d: &DocumentAttrs, g: &[Grant]) -> bool {
    decide(&Policy::default(), p, perm, d, g, now()).is_allowed()
}

/// The grant every department gets when created: members may view.
fn department_default(dept: Uuid) -> Grant {
    grant(
        Subject::Department(dept),
        Scope::Department(dept),
        Permission::View,
    )
}

const ALICE: Uuid = USERS[0];
const BOB: Uuid = USERS[1];
const CAROL: Uuid = USERS[2];
const OPS: Uuid = DEPARTMENTS[0];
const HR: Uuid = DEPARTMENTS[1];

#[test]
fn new_user_reads_own_department_only() {
    let alice = user(ALICE, &[OPS]);
    let grants = [department_default(OPS), department_default(HR)];
    let roads_doc = doc(DOCUMENTS[0], OPS, BOB);
    let hr_doc = doc(DOCUMENTS[1], HR, BOB);

    assert!(allowed(&alice, Permission::View, &roads_doc, &grants));
    for perm in Permission::ALL
        .into_iter()
        .filter(|p| *p != Permission::View)
    {
        assert!(
            !allowed(&alice, perm, &roads_doc, &grants),
            "{perm} on own department"
        );
    }
    for perm in Permission::ALL {
        assert!(
            !allowed(&alice, perm, &hr_doc, &grants),
            "{perm} on other department"
        );
    }
    assert!(!decide_create(&alice, OPS, &grants, now()).is_allowed());
}

#[test]
fn owner_gets_view_and_edit_only() {
    let bob = user(BOB, &[]);
    let d = doc(DOCUMENTS[0], OPS, BOB);
    assert_eq!(
        decide(&Policy::default(), &bob, Permission::Edit, &d, &[], now()),
        Decision::Allow(AllowReason::Owner)
    );
    assert!(allowed(&bob, Permission::View, &d, &[]));
    for perm in [
        Permission::Delete,
        Permission::Approve,
        Permission::ManagePermissions,
        Permission::Export,
    ] {
        assert!(!allowed(&bob, perm, &d, &[]), "owner should not get {perm}");
    }
}

#[test]
fn non_create_permissions_imply_view() {
    let alice = user(ALICE, &[]);
    let d = doc(DOCUMENTS[0], OPS, BOB);
    for perm in Permission::ALL {
        let g = [grant(Subject::User(ALICE), Scope::Document(d.id), perm)];
        if perm == Permission::Create {
            continue; // not storable at document scope
        }
        assert!(
            allowed(&alice, Permission::View, &d, &g),
            "{perm} should imply view"
        );
    }
    let g = [grant(
        Subject::User(ALICE),
        Scope::Department(OPS),
        Permission::Create,
    )];
    assert!(
        !allowed(&alice, Permission::View, &d, &g),
        "create must not imply view"
    );
    assert!(decide_create(&alice, OPS, &g, now()).is_allowed());
    assert!(!decide_create(&alice, HR, &g, now()).is_allowed());
}

#[test]
fn single_document_share_does_not_expose_department() {
    let alice = user(ALICE, &[HR]);
    let shared = doc(DOCUMENTS[0], OPS, BOB);
    let other = doc(DOCUMENTS[1], OPS, BOB);
    let g = [grant(
        Subject::User(ALICE),
        Scope::Document(shared.id),
        Permission::View,
    )];
    assert!(allowed(&alice, Permission::View, &shared, &g));
    assert!(!allowed(&alice, Permission::View, &other, &g));
}

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

#[test]
fn restricted_requires_document_grant_even_for_admin() {
    let admin_role = ROLES[0];
    let mut admin = user(ALICE, &[OPS]);
    admin.roles.push(RoleHolding {
        role_id: admin_role,
        role_key: "admin".into(),
        department_id: None,
    });
    let mut d = doc(DOCUMENTS[0], OPS, BOB);
    d.visibility = Visibility::Restricted;
    let g = [
        department_default(OPS),
        grant(Subject::Role(admin_role), Scope::Global, Permission::View),
        grant(
            Subject::Role(admin_role),
            Scope::Global,
            Permission::ManagePermissions,
        ),
    ];
    assert!(!allowed(&admin, Permission::View, &d, &g));
    assert!(!allowed(&admin, Permission::ManagePermissions, &d, &g));
    // Owner still sees their own restricted document.
    assert!(allowed(&user(BOB, &[]), Permission::View, &d, &g));
}

#[test]
fn county_wide_lets_every_active_account_view_only() {
    let outsider = user(ALICE, &[HR]);
    let mut d = doc(DOCUMENTS[0], OPS, BOB);
    d.visibility = Visibility::CountyWide;
    let g = [department_default(OPS)];
    assert_eq!(
        decide(
            &Policy::default(),
            &outsider,
            Permission::View,
            &d,
            &g,
            now()
        ),
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

#[test]
fn department_scoped_role_matches_only_that_department() {
    let head = ROLES[1];
    let mut alice = user(ALICE, &[]);
    alice.roles.push(RoleHolding {
        role_id: head,
        role_key: "department_head".into(),
        department_id: Some(OPS),
    });
    let g = [grant(
        Subject::Role(head),
        Scope::Global,
        Permission::Approve,
    )];
    assert!(allowed(
        &alice,
        Permission::Approve,
        &doc(DOCUMENTS[0], OPS, BOB),
        &g
    ));
    assert!(!allowed(
        &alice,
        Permission::Approve,
        &doc(DOCUMENTS[1], HR, BOB),
        &g
    ));
}

#[test]
fn group_grants_apply_to_members() {
    let mut alice = user(ALICE, &[]);
    alice.groups.insert(GROUPS[0]);
    let d = doc(DOCUMENTS[0], OPS, BOB);
    let g = [grant(
        Subject::Group(GROUPS[0]),
        Scope::Document(d.id),
        Permission::Export,
    )];
    assert!(allowed(&alice, Permission::Export, &d, &g));
    assert!(!allowed(&user(CAROL, &[]), Permission::Export, &d, &g));
}

#[test]
fn expired_grants_are_ignored() {
    let alice = user(ALICE, &[]);
    let d = doc(DOCUMENTS[0], OPS, BOB);
    let mut g = grant(
        Subject::User(ALICE),
        Scope::Document(d.id),
        Permission::View,
    );
    g.expires_at = Some(now());
    assert!(!allowed(
        &alice,
        Permission::View,
        &d,
        std::slice::from_ref(&g)
    ));
    g.expires_at = Some(now() + Duration::seconds(1));
    assert!(allowed(&alice, Permission::View, &d, &[g]));
}

#[test]
fn inactive_user_is_denied_everything() {
    let mut bob = user(BOB, &[OPS]);
    bob.active = false;
    let d = doc(DOCUMENTS[0], OPS, BOB);
    let g = [department_default(OPS)];
    assert_eq!(
        decide(&Policy::default(), &bob, Permission::View, &d, &g, now()),
        Decision::Deny(DenyReason::InactiveUser)
    );
}

#[test]
fn self_approval_is_configurable() {
    let strict = Policy::default();
    assert_eq!(
        check_self_approval(&strict, ALICE, ALICE),
        Err(DenyReason::SelfApproval)
    );
    assert_eq!(check_self_approval(&strict, ALICE, BOB), Ok(()));
    let lax = Policy {
        allow_self_approval: true,
        ..Policy::default()
    };
    assert_eq!(check_self_approval(&lax, ALICE, ALICE), Ok(()));
}

#[test]
fn capabilities_require_org_wide_roles() {
    let mut p = user(ALICE, &[]);
    assert!(!has_capability(&p, Capability::ViewAudit));
    p.roles.push(RoleHolding {
        role_id: ROLES[0],
        role_key: "admin".into(),
        department_id: Some(OPS),
    });
    assert!(
        !has_capability(&p, Capability::ManageDirectory),
        "department-scoped admin is not admin"
    );
    p.roles.push(RoleHolding {
        role_id: ROLES[0],
        role_key: "admin".into(),
        department_id: None,
    });
    assert!(has_capability(&p, Capability::ManageDirectory));
    assert!(has_capability(&p, Capability::ViewAudit));
    assert!(
        !has_capability(&p, Capability::ExportAudit),
        "audit export is auditor-only"
    );
    p.active = false;
    assert!(!has_capability(&p, Capability::ManageDirectory));
}

#[test]
fn permission_round_trips_through_str() {
    for p in Permission::ALL {
        assert_eq!(p.as_str().parse::<Permission>().unwrap(), p);
    }
    assert!("superuser".parse::<Permission>().is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn deny_by_default(p in strategies::principal(), perm in strategies::permission(), mut d in strategies::document()) {
        d.owner_id = Uuid::from_u128(0xdead);
        // Without grants, the only access is viewing a County-wide document.
        let county_wide_view = perm == Permission::View && d.visibility == Visibility::CountyWide && p.active;
        prop_assert_eq!(allowed(&p, perm, &d, &[]), county_wide_view);
    }

    #[test]
    fn adding_a_grant_never_removes_access(
        p in strategies::principal(), perm in strategies::permission(), d in strategies::document(),
        gs in strategies::grants(), extra in strategies::grant(),
    ) {
        let before = allowed(&p, perm, &d, &gs);
        let mut more = gs.clone();
        more.push(extra);
        prop_assert!(!before || allowed(&p, perm, &d, &more));
    }

    #[test]
    fn revoking_the_allowing_grant_denies_when_no_other_path(
        p in strategies::principal(), perm in strategies::permission(), d in strategies::document(),
        gs in strategies::grants(),
    ) {
        if let Decision::Allow(AllowReason::Grant(id)) = decide(&Policy::default(), &p, perm, &d, &gs, now()) {
            let remaining: Vec<Grant> = gs.iter().filter(|g| g.id != id).cloned().collect();
            let still = allowed(&p, perm, &d, &remaining);
            let other_path = remaining.iter().any(|g| {
                perm.satisfied_by(g.permission) && g.is_live(now())
                    && p.matches_subject(g.subject, d.department_id) && scope_covers(g.scope, &d)
            });
            let owner_path = d.owner_id == p.user_id && Policy::default().owner_permissions.satisfies(perm);
            prop_assert_eq!(still, other_path || owner_path);
        }
    }

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

    #[test]
    fn create_is_never_decided_on_existing_documents(
        p in strategies::principal(), d in strategies::document(), gs in strategies::grants(),
    ) {
        prop_assert!(!allowed(&p, Permission::Create, &d, &gs));
    }

    #[test]
    fn inactive_principals_are_always_denied(
        mut p in strategies::principal(), perm in strategies::permission(), d in strategies::document(),
        gs in strategies::grants(),
    ) {
        p.active = false;
        prop_assert!(!allowed(&p, perm, &d, &gs));
        prop_assert!(!decide_create(&p, d.department_id, &gs, now()).is_allowed());
    }

    #[test]
    fn unrelated_grants_do_not_matter(
        p in strategies::principal(), perm in strategies::permission(), d in strategies::document(),
        gs in strategies::grants(),
    ) {
        // Grants for other documents never change the outcome.
        let relevant: Vec<Grant> = gs.iter()
            .filter(|g| !matches!(g.scope, Scope::Document(id) if id != d.id))
            .cloned().collect();
        prop_assert_eq!(allowed(&p, perm, &d, &gs), allowed(&p, perm, &d, &relevant));
    }
}

fn with_org_role(mut p: Principal, key: &str) -> Principal {
    p.roles.push(RoleHolding {
        role_id: Uuid::new_v4(),
        role_key: key.to_owned(),
        department_id: None,
    });
    p
}

#[test]
fn positions_are_visible_to_their_department_manager_and_clerks() {
    let position = PositionAttrs {
        department_id: Some(OPS),
        manager_user_id: Some(CAROL),
    };
    assert!(can_view_position(&user(ALICE, &[OPS]), &position));
    assert!(!can_view_position(&user(BOB, &[HR]), &position));
    assert!(can_view_position(&user(CAROL, &[]), &position), "manager");
    let clerk = with_org_role(user(BOB, &[HR]), role_keys::APPOINTMENTS_CLERK);
    let auditor = with_org_role(user(BOB, &[HR]), role_keys::AUDITOR);
    assert!(can_view_position(&clerk, &position));
    assert!(can_view_position(&auditor, &position));

    // Only clerks/admins and the manager track requirements; auditors read only.
    assert!(can_track_position(&clerk, &position));
    assert!(can_track_position(&user(CAROL, &[]), &position));
    assert!(!can_track_position(&auditor, &position));
    assert!(!can_track_position(&user(ALICE, &[OPS]), &position));

    let mut inactive = user(CAROL, &[OPS]);
    inactive.active = false;
    assert!(!can_view_position(&inactive, &position));
    assert!(!can_track_position(&inactive, &position));
}

#[test]
fn step_assignees_match_users_and_department_scoped_roles() {
    let role = ROLES[0];
    let mut p = user(ALICE, &[OPS]);
    assert!(is_assignee(&p, Assignee::User(ALICE), OPS));
    assert!(!is_assignee(&p, Assignee::User(BOB), OPS));
    assert!(!is_assignee(&p, Assignee::Role(role), OPS));
    p.roles.push(RoleHolding {
        role_id: role,
        role_key: "legal_counsel".into(),
        department_id: Some(OPS),
    });
    assert!(is_assignee(&p, Assignee::Role(role), OPS));
    assert!(
        !is_assignee(&p, Assignee::Role(role), HR),
        "role held for another department"
    );
}

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

#[test]
fn sharepoint_management_is_for_org_admins_only() {
    let admin = with_org_role(user(ALICE, &[]), "admin");
    assert!(has_capability(&admin, Capability::ManageSharePoint));
    for key in [
        "auditor",
        "records_officer",
        "appointments_clerk",
        "legal_counsel",
    ] {
        let p = with_org_role(user(BOB, &[]), key);
        assert!(!has_capability(&p, Capability::ManageSharePoint), "{key}");
    }
    let mut scoped = user(CAROL, &[OPS]);
    scoped.roles.push(RoleHolding {
        role_id: ROLES[0],
        role_key: "admin".into(),
        department_id: Some(OPS),
    });
    assert!(!has_capability(&scoped, Capability::ManageSharePoint));
}
