# Entra ID sync limited to one group: design

- **Date:** 2026-10-27
- **Status:** draft, waiting for the owner's approval (ROADMAP §0.7).
- **Item:** "Group-scoped sync" in ROADMAP §0.3. Commit messages start with
  `GS:`.
- **Why:** today the Entra ID user sync (D-086, D-096) creates a PDMS account
  for every enabled member account in the tenant. An administrator can already
  stop people outside a group from *signing in*, with **Assignment required**
  on the sign-in app in Entra (ADMIN_GUIDE "Sign-in", step 5), but their PDMS
  accounts still exist: they show in the people list and can be picked as
  owners, reviewers or approvers. IT wants a single place to say who uses
  PDMS: put people in a group (for example `PDMS`), and PDMS has exactly
  those people.

## 1. What changes for an administrator

1. On **Administration → Entra ID sync**, a new field **Only members of this
   group** takes the group's object ID (Entra admin center → Groups → the
   group → Object ID). Left empty, the sync works as it does today: the whole
   tenant.
2. **Test group** shows the group's name and how many people are in it,
   without saving or changing anything.
3. From the next run on (daily, or **Sync now**), PDMS creates and updates
   accounts only for the group's members, and departs (D-096) the people it
   had synced who are no longer in it.
4. Someone outside the group who signs in with Microsoft is refused, and no
   account is created. They see the same page as any other refused sign-in.
   The audit log records the attempt with the reason `not_in_group`, so IT
   can tell this case from the others.

The group can be:

- a cloud security group or Microsoft 365 group made in Entra;
- an on-premises AD group, **once Entra Connect (or Cloud Sync) syncs it to
  Entra**. D-086 records that the county's AD groups are not synced today,
  so IT either adds this one group to the sync scope or makes the group in
  Entra;
- a dynamic group (for example, everyone whose Department is set).

**Nested groups count:** a member of a group inside the PDMS group is a
member. This lets IT add whole department groups to it.

## 2. Microsoft Graph

- **New application permission:** `GroupMember.Read.All`, with admin consent,
  on the PDMS Service app. It is needed only when a group is set. It lets
  PDMS read group memberships, not change them. Without it, **Test group**
  and every run fail with "PDMS can't read the group's members: add the
  `GroupMember.Read.All` permission (ADMIN_GUIDE, Entra ID sync)".
- **Each run reads the whole member list:**
  `GET /groups/{id}/transitiveMembers/microsoft.graph.user?$select=id`,
  paged (999 per page). For a county, that is a few requests.
  - *Why not a group delta:* `/groups/delta` reports direct members only,
    so nested groups would be missed, and it doesn't say when someone leaves
    a nested group.
- **`/users/delta` stays** the source of names, email addresses, the
  Department value, enabled or disabled, and guests, as today.
- **Sign-in check:** at first sign-in, `sync_one` asks
  `POST /users/{id}/checkMemberGroups` with the group ID. This is
  transitive, and it is one request.

## 3. The sync with a group set

Each run, after reading the member list:

1. **Accounts in `/users/delta`:** a member is applied as today. A non-member
   is treated like a disabled account: departed if PDMS has them as active,
   otherwise skipped.
2. **Members PDMS doesn't have yet:** a member with no active PDMS account
   who wasn't in this round's delta (for example, added to the group without
   any other change in Entra) is read with `/users/{id}` and applied. This is
   how adding someone to the group creates their account at the next run.
3. **Active synced accounts not in the group** are departed, with the reason
   "is no longer in the PDMS group". This happens every run, not only on full
   rounds, because the member list is always complete.
4. **The existing safety rules still apply** (D-096):
   - departures are held back and reported if the group lists no members,
     or if more than max(5, 20%) of synced accounts would leave at once;
   - the last administrator is never departed automatically;
   - an account that also has a local password is departed and reported;
   - the break-glass account has no directory link and is never touched.

**First run after setting or changing the group.** Narrowing a 400-person
tenant to an 80-person group would depart 320 accounts. That is more than
the limit, so the run holds them back, as it should. To finish the job on
purpose, the held-back report lists those people with a button **Depart
these N people**:

- it requires `ManageDirectory`;
- it departs only the accounts listed in the last run's report, and only
  those still active and still outside the group;
- each departure is audited as usual.

This replaces departing them one by one on their pages.

**Re-added to the group.** Following D-096, the sync never reactivates
anyone. Someone removed from the group and later added back stays departed
until an administrator reactivates them. (Open question 2.)

## 4. Data and settings

- **New setting:** `entra_sync.group_id`, text, in the registry
  (`pdms-core::settings::SETTINGS`). It can be set on the page, or in
  `config/pdms.toml` as `[entra_sync] group_id`, where it wins and shows
  read-only (D-111). The value is checked as a GUID.
- **Changing the group** changes the cursor key (the same mechanism as
  `include_guests`), so the next round reads every account again.
- **`SyncReport` gets `held_back: Vec<Uuid>`**, with a serde default so
  older summaries still read. The button uses it.
- **No migration:** the report is stored in `sync_cursors.last_summary`
  (JSON) as today.

## 5. Permissions and audit

- The setting is saved through `settings::save`, so it needs
  `ManageSettings` and is audited `settings.changed`.
- **Test group** is audited `settings.tested`, with area `entra_sync`.
- Departures are audited as today (`user.status_changed`), with the new
  reason.
- **Depart these N people** writes one audit event per person, in the same
  transaction as that person's change. Nothing widens access.

## 6. Tests

Add to the mock Graph:

- a group with members, including a nested group;
- `transitiveMembers`;
- `checkMemberGroups`;
- a 403 for a missing permission.

Cases:

1. Without a group set, behaviour is unchanged (the existing sync tests
   pass untouched).
2. With a group set, only members are created; a non-member in the delta is
   skipped; an active non-member is departed.
3. A member added to the group with no other change in Entra is created at
   the next run.
4. Members of a nested group count.
5. Narrowing the group holds back the mass departure. **Depart these N
   people** departs exactly the listed people still outside the group, and
   refuses for others.
6. First sign-in: a member gets an account; a non-member is refused, the
   audit reason is `not_in_group`, and no account is created.
7. A missing `GroupMember.Read.All` gives the plain message, and nothing is
   departed.
8. The last administrator and the break-glass account are kept.
9. A group set in the configuration file shows read-only.

## 7. Documentation

- **ADMIN_GUIDE, "Entra ID user sync":**
  - the group field and the new permission;
  - how to sync an on-premises AD group to Entra, or make the group in
    Entra;
  - what happens on the first run and after removing someone;
  - why **Assignment required** is still worth turning on: it blocks
    sign-in at Microsoft at once, before the next sync.
- **SHAREPOINT_SETUP.md B2:** `GroupMember.Read.All` is added only when a
  group is used.
- **DECISIONS:** a new decision amending D-086. Membership of one group,
  when set, decides who has a PDMS account.
- **ROADMAP:** §0.3 and §0.1.

## 8. Phases (the plan will detail them)

1. The setting, **Test group**, and the mock Graph group endpoints.
2. The sync with a group set: member list, non-member departures, newly
   added members, and the sign-in check.
3. **Depart these N people**, the docs, and the decision.

Size: small to medium. It touches `crates/pdms-core/src/directory/entra_sync.rs`,
`auth/oidc.rs` (the refusal reason), `settings.rs`, and the Entra ID sync
admin page and its handler in `routes/admin.rs`.

## 9. Open questions for the owner

1. **Approve the change to D-086:** one group, when set, decides who has a
   PDMS account.
2. **Re-added to the group:** keep D-096 (an administrator reactivates by
   hand), or reactivate automatically when someone is back in the group?
   This spec keeps D-096.
3. **First narrowing:** is a **Depart these N people** button, after the
   held-back run, the right way to confirm a large departure? The other
   choice is a preview with a confirmation when the group is saved.
4. **Nested groups count** as members. Agreed?
5. **Refused sign-in:** keep the generic page that every refused sign-in
   shows today, or tell the person they aren't in the PDMS group and to ask
   IT? The second is friendlier, but it says why. This spec keeps the
   generic page.
