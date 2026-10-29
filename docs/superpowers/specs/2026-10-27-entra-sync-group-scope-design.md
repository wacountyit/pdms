# Entra ID sync limited to one group: design

- **Date:** 2026-10-27
- **Status:** approved by the owner on 2026-10-28, with the answers in §9.
  Amended by the owner on 2026-10-29: reactivation also happens at sign-in
  (§3, §9 item 7). Plan:
  [2026-10-29-entra-sync-group-scope.md](../plans/2026-10-29-entra-sync-group-scope.md).
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
   account is created. The page tells them they aren't in the PDMS group and
   to ask IT for access; every other refused sign-in still shows the generic
   page. The audit log records the attempt with the reason `not_in_group`,
   so IT can tell this case from the others.
5. Someone the sync departed because they left the group is reactivated
   once they are back in it: at the next run, or when they sign in with
   Microsoft, whichever comes first (§3).

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

**Re-added to the group.** The sync reactivates them automatically (the
owner's answer, §9). This amends D-096, under which the sync never
reactivates anyone. The rule is narrow:

- It applies only to accounts **the sync departed because they left the
  group**. The group rule never undoes a departure an administrator made by
  hand, or one made because Entra disabled or removed the account.
- The person must be back in the group, with an enabled member account in
  Entra.
- Their roles, access grants and memberships come back as they were,
  because a departure leaves them on the account. Their department is
  refreshed from Entra as usual.
- Each run's report lists the people it reactivated and marks those who
  hold roles, so an administrator can review them. Each reactivation is
  audited `user.status_changed` with the reason "is back in the PDMS group".

**At sign-in** (the owner's answer, 2026-10-29): a departed account marked
as departed for leaving the group doesn't wait for the next run. When the
person signs in with Microsoft, PDMS checks the group
(`checkMemberGroups`). If they are back in it, with an enabled member
account, they are reactivated as above and signed in. If they are still
outside it, they see the "not in the PDMS group" page. Any other departed
account is refused as today, and Graph is not asked.

The sync has to know why it departed someone. The plan settles how it
records that, for example in a column on `users` (§4).

**A Department mapping change racing the sync.** This was left open in the
P2 build (D-096). If an administrator changes a mapping while a run is
applying the same person, that person can stay on the old mapping until Entra
changes them or a **Full resync** runs. GS closes the gap, because it changes
the same code: applying a person and changing a mapping
(`directory/entra_values.rs`, `upsert` and `remove`) take one advisory lock,
shared and exclusive respectively, and the mapping is looked up inside the
apply transaction. The admin guide's advice to press **Full resync** after
mapping values during a sync then goes. A mapping edit made while the sync is
off stays as it is (the owner's answer, §9).

## 4. Data and settings

- **New setting:** `entra_sync.group_id`, text, in the registry
  (`pdms-core::settings::SETTINGS`). It can be set on the page, or in
  `config/pdms.toml` as `[entra_sync] group_id`, where it wins and shows
  read-only (D-111). The value is checked as a GUID.
- **Changing the group** changes the cursor key (the same mechanism as
  `include_guests`), so the next round reads every account again.
- **`SyncReport` gets `held_back: Vec<Uuid>`** (the button uses it) and
  **`reactivated: Vec<Uuid>`** (the report lists them), each with a serde
  default so older summaries still read.
- **Migrations:** the report is stored in `sync_cursors.last_summary`
  (JSON) as today. Recording why the sync departed someone (§3) probably
  needs one migration, which takes the next free number.

## 5. Permissions and audit

- The setting is saved through `settings::save`, so it needs
  `ManageSettings` and is audited `settings.changed`.
- **Test group** is audited `settings.tested`, with area `entra_sync`.
- Departures are audited as today (`user.status_changed`), with the new
  reason.
- **Depart these N people** writes one audit event per person, in the same
  transaction as that person's change.
- **Automatic reactivation** gives back exactly the access the person had
  before they left the group, and nothing more. It is audited like a
  departure, and the run report lists it (§3). Nothing else widens access.

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
6. First sign-in: a member gets an account; a non-member is refused with
   the "not in the PDMS group" page, the audit reason is `not_in_group`, and
   no account is created. Other refusals still show the generic page.
7. A missing `GroupMember.Read.All` gives the plain message, and nothing is
   departed.
8. The last administrator and the break-glass account are kept.
9. A group set in the configuration file shows read-only.
10. Someone the sync departed for leaving the group is reactivated once back
    in it, with their roles and grants, and listed in the report. Someone
    departed by hand, or because Entra disabled them, is not reactivated.
    The same holds when they sign in before the next run.
11. A mapping change made while a run is applying the same person is not
    lost.

## 7. Documentation

- **ADMIN_GUIDE, "Entra ID user sync":**
  - the group field and the new permission;
  - how to sync an on-premises AD group to Entra, or make the group in
    Entra;
  - what happens on the first run, after removing someone, and after adding
    them back;
  - the **Full resync** advice for mapping changes during a sync is removed
    (§3);
  - why **Assignment required** is still worth turning on: it blocks
    sign-in at Microsoft at once, before the next sync.
- **SHAREPOINT_SETUP.md B2:** `GroupMember.Read.All` is added only when a
  group is used.
- **DECISIONS:** a new decision amending D-086 and D-096. Membership of one
  group, when set, decides who has a PDMS account. The sync reactivates the
  people it departed for leaving the group once they are back, and the
  mapping race is closed.
- **ROADMAP:** §0.3 and §0.1.

## 8. Phases (the plan will detail them)

1. The setting, **Test group**, and the mock Graph group endpoints.
2. The sync with a group set: member list, non-member departures, newly
   added members, reactivation of people back in the group, the sign-in
   check and its page, and the lock between mapping changes and the sync.
3. **Depart these N people**, the docs, and the decision.

Size: small to medium. It touches `crates/pdms-core/src/directory/entra_sync.rs`
and `entra_values.rs`, `auth/oidc.rs` (the refusal reason and its page),
`settings.rs`, and the Entra ID sync admin page and its handler in
`routes/admin.rs`.

## 9. The owner's answers (2026-10-28)

1. **The change to D-086:** approved. One group, when set, decides who has
   a PDMS account.
2. **Re-added to the group:** reactivate automatically, with roles and
   grants as they were, listed in the run report. This applies only to
   accounts the sync departed for leaving the group (§3).
3. **First narrowing:** yes, a **Depart these N people** button after the
   held-back run.
4. **Nested groups count** as members: yes.
5. **Refused sign-in:** tell the person they aren't in the PDMS group and to
   ask IT (§1).
6. **Also decided:** of the two timing cases left from the P2 build, the
   mapping change racing the sync is fixed in GS; the edit made while the
   sync is off stays as it is (§3).
7. **Reactivation at sign-in** (2026-10-29): someone back in the group who
   signs in before the next run is reactivated then, instead of being refused
   until the run (§3).
