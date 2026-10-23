# Setting up SharePoint for PDMS

PDMS can keep every document file in one dedicated SharePoint site, where
each department's staff can browse their own files read-only. PDMS stays the
system people use: details, permissions, workflow, reminders, search and the
audit log stay in PDMS. This guide is for the Microsoft 365 administrator.

- **Part A** can be done at any time.
- **Part B** is done when PDMS is ready to connect.
- **Part C** covers looking after the connection.

Do everything for the **test** site and test server first.

| | Test | Production |
|---|---|---|
| Site | `PDMS Records – Test` | `PDMS Records` |
| App registration | `PDMS Service – Test` | `PDMS Service` |
| PDMS server | the test server | the production server |

Use a separate app registration for each server, so the test server's
certificate can never reach production files.

## Part A: the site

### A1. Create the site

In the SharePoint admin center, choose **Sites → Active sites → Create →
Communication site**:

- name `PDMS Records – Test` (production: `PDMS Records`);
- design **Blank**;
- language English.

Note the site address, for example
`https://contoso.sharepoint.com/sites/PDMSRecordsTest`. The PDMS installer
asks for it.

### A2. Permissions

On the site, open **Settings (gear) → Site permissions → Advanced permissions
settings**:

- **Owners:** one or two named administrators. These are the only people who
  can change files directly; PDMS watches for that (step B9).
- **Members** and **Visitors:** remove everyone. Leave the groups empty.

Do not add people or groups anywhere else on the site. PDMS gives each
department's staff read access to their own department folder itself.

### A3. Sharing

- **Site permissions → Change how members can share:** choose "Only site
  owners can share files, folders, and the site", and turn **Allow access
  requests** off.
- **SharePoint admin center → Active sites → the site → Settings → External
  file sharing:** "Only people in your organization".

### A4. The document library

In **Documents → Settings (gear) → Library settings → Versioning settings**:

- **Create major versions:** yes (keep the default limit or higher);
- **Require content approval:** no;
- **Require check out:** no.

Leave the library named `Documents`.

### A5. The three top-level folders

In `Documents`, create these folders, spelled exactly:

- `Departments`
- `County-wide`
- `Restricted`

On `County-wide` only: choose **… → Manage access → Advanced settings →
Stop inheriting permissions**, then **Grant permissions** to **Everyone
except external users** with **Read**.

Leave `Departments` and `Restricted` inheriting (owners only). PDMS creates
the department and document folders itself. If a top-level folder is missing,
PDMS creates it and logs a warning; `County-wide` then still needs the
permission change above.

### A6. Retention (recommended)

In Microsoft Purview, **Data lifecycle management → Retention policies → New
retention policy**: static scope, location **SharePoint classic and
communication sites**, include only this site, and retain items for the
county's records period (or forever). Files deleted from the site can then
always be recovered.

### A7. The Department field

PDMS puts each person in a department from the **Department** field of their
Entra account. Make sure every staff account has one consistent value per
department, and make the field part of account creation. To list the values
in use (Microsoft Graph PowerShell):

```powershell
Connect-MgGraph -Scopes "User.Read.All"
Get-MgUser -All -Filter "accountEnabled eq true and userType eq 'Member'" -Property Department |
  Group-Object Department | Sort-Object Count -Descending | Select-Object Count, Name
```

## Part B: connecting PDMS

### B1. Register the app

In the Entra admin center, choose **Applications → App registrations → New
registration**:

- name `PDMS Service – Test` (production: `PDMS Service`);
- **Accounts in this organizational directory only**;
- no redirect URI.

Note the **Application (client) ID** and the **Directory (tenant) ID**.

### B2. API permissions

Under **API permissions → Add a permission → Microsoft Graph → Application
permissions**, add:

- `Sites.Selected`
- `User.Read.All`

Then choose **Grant admin consent**. Do not add `Sites.ReadWrite.All` or
`Files.ReadWrite.All`: `Sites.Selected` limits PDMS to the one site you grant
in B5.

### B3. Connect PDMS to the app

Sign in to PDMS as an administrator and open **Administration → Microsoft 365
→ Connection**:

1. Choose **Make a new certificate**. PDMS makes the certificate it signs in
   with; its private key never leaves PDMS (it is stored encrypted in the
   database). The page shows its expiry date and two thumbprints: SHA-1 (what
   the Entra admin center lists) and SHA-256.
2. Choose **Download** to save the public certificate (`pdms-service.crt`).
3. Enter the **Directory (tenant) ID** and **Application (client) ID** from
   B1, and choose **Save** once the certificate is uploaded (B4). **Test
   these IDs** tries them first without saving.

### B4. Upload the certificate

In the app registration, choose **Certificates & secrets → Certificates →
Upload certificate** and pick `pdms-service.crt`. Check that the thumbprint
Entra lists matches the SHA-1 thumbprint on the Connection page. Then choose
**Test connection** there: it should say "Signed in to Microsoft Graph as the
PDMS Service app".

### B4b. Enter the site

Open **Administration → SharePoint**. Under **Settings**, enter the site
address (for example `https://contoso.sharepoint.com/sites/PDMSRecordsTest`)
and the library (`Documents` unless you named it differently in A4), then
choose **Save**. Leave storage off for now: turn it on in B6, after the site
grant.

**Test** checks what you typed without saving it and without changing
anything in SharePoint. It lists each step: the site, the library, the PDMS
columns, the three folders, and whether PDMS can read the library's sharing.
Before B5 it reports that the site wasn't found, because PDMS has no access
to it yet.

### B5. Give the app full control of this one site

PDMS needs full control so it can create the library columns and folders and
grant and remove read access on department folders. The grant is made in two
steps, because Microsoft Graph creates the permission first and then raises
it. Sign in as a Global Administrator or SharePoint Administrator. Use one of
the two methods.

**Microsoft Graph PowerShell** (module `Microsoft.Graph` 2.x):

```powershell
Connect-MgGraph -Scopes "Sites.FullControl.All" -TenantId "<tenant-id>"
$site = Get-MgSite -SiteId "contoso.sharepoint.com:/sites/PDMSRecordsTest"
$perm = New-MgSitePermission -SiteId $site.Id -BodyParameter @{
  roles = @("write")
  grantedToIdentities = @(@{ application = @{ id = "<client-id>"; displayName = "PDMS Service – Test" } })
}
Update-MgSitePermission -SiteId $site.Id -PermissionId $perm.Id -BodyParameter @{ roles = @("fullcontrol") }
Get-MgSitePermission -SiteId $site.Id | Format-List Id, Roles, GrantedToIdentitiesV2
```

The same calls in raw Microsoft Graph (Graph Explorer, signed in with
`Sites.FullControl.All`):

```http
GET https://graph.microsoft.com/v1.0/sites/contoso.sharepoint.com:/sites/PDMSRecordsTest?$select=id

POST https://graph.microsoft.com/v1.0/sites/{site-id}/permissions
{"roles":["write"],"grantedToIdentities":[{"application":{"id":"<client-id>","displayName":"PDMS Service – Test"}}]}

PATCH https://graph.microsoft.com/v1.0/sites/{site-id}/permissions/{permission-id}
{"roles":["fullcontrol"]}
```

**PnP PowerShell** (needs its own app registration for sign-in, see the PnP
documentation):

```powershell
Connect-PnPOnline -Url "https://contoso-admin.sharepoint.com" -Interactive -ClientId "<pnp-app-id>"
$p = Grant-PnPAzureADAppSitePermission -AppId "<client-id>" -DisplayName "PDMS Service – Test" `
       -Permissions Write -Site "https://contoso.sharepoint.com/sites/PDMSRecordsTest"
Set-PnPAzureADAppSitePermission -Site "https://contoso.sharepoint.com/sites/PDMSRecordsTest" `
  -PermissionId $p.Id -Permissions FullControl
```

Recent PnP releases name these `Grant-PnPEntraIDAppSitePermission` and
`Set-PnPEntraIDAppSitePermission`.

To disconnect PDMS later, remove this permission
(`Remove-MgSitePermission -SiteId $site.Id -PermissionId <id>`).

### B6. Turn storage on and check it

In PDMS, open **Administration → SharePoint** and choose **Test** again:
every line should now say OK. Then choose **Turn SharePoint storage on**.
PDMS refuses while any document file is stored on this server, because it
can't read those files once SharePoint storage is on: turn it on before real
documents are added (on the test server, reset it first: rollout, at the end
of this guide). It also tests the site again and stays off if the test
fails.

Then choose **Check for changes now**. On
its first connection PDMS creates the library columns (Document ID, Document
Type, Tags, Status, Visibility, Department, Owner, the four dates, Version,
Current, Open in PDMS) and checks the three folders. The page should show the
run as successful within a minute.

If it shows "PDMS cannot sign in to Microsoft Graph", check, in order:

1. the certificate upload (B4);
2. admin consent (B2);
3. the site grant (B5).

### B7. Library views (by hand)

Microsoft Graph cannot create views, so add this one by hand once the columns
exist. In `Documents`, open the view menu (**All Documents**) and choose
**Create new view**:

- name **Current documents**, type **List**, **Make this the default view**;
- then **Edit current view**:
  - **Columns:** Name, Document ID, Title, Document Type, Department, Status,
    Review Date, Expiration Date, Open in PDMS;
  - **Sort:** Review Date, ascending;
  - **Filter:** show items only when **Current** is equal to **Yes**;
  - **Folders:** **Show all items without folders**.

The existing **All Documents** view shows every version; you may rename it
**All versions**. "Open in PDMS" is plain text, because Microsoft Graph cannot
write link columns: copy it into the browser.

### B8. Departments

If Entra user sync is on, open **Administration → Entra ID sync**, map each
Department value from A7 to a PDMS department, and choose **Sync now**.
Within about a minute PDMS gives each department's staff read access to
`Departments/<department>`. **Administration → SharePoint** shows the run and
anyone SharePoint could not resolve (for example, an account without a
mailbox).

### B9. Prove the change check (test site only)

1. Upload a document in PDMS.
2. In SharePoint, as a site owner, open its file and change it.
3. Within about five minutes (`[sharepoint] delta_interval_minutes`), PDMS
   flags the document, and administrators get "SharePoint: 1 file(s) changed
   outside PDMS".
4. On **Administration → SharePoint**, choose **Restore original**. The flag
   clears.
5. On the server, run:
   `docker compose run --rm app verify-integrity --rehash-sharepoint`.
   It should report no problems.

## Part C: looking after it

### C1. Renew the certificate (every two years)

Administrators get a reminder 30 days before the certificate expires. To
renew without an outage, on **Administration → Microsoft 365 → Connection**:

1. Choose **Make a new certificate**. It waits; PDMS keeps signing in with
   the current one.
2. **Download** the new certificate and upload it to the app registration
   (B4). Keep the old one there for now.
3. Choose **Test connection** (it tries the newest certificate), then **Use
   this certificate**. PDMS switches at once; the old certificate's private
   key is deleted.
4. Delete the old certificate from the app registration.

If the certificate comes from `secrets/graph_certificate` instead (set up
with the installer or `pdms gen-graph-cert`), replace that file with a new
certificate and key and run `docker compose up -d --force-recreate app worker`,
after uploading the new public certificate.

### C2. When something looks wrong

| You see | Likely cause | What to do |
|---|---|---|
| "PDMS cannot sign in to Microsoft Graph" (at most hourly) | certificate expired or not uploaded, consent or site grant removed | B4, B2, B5; then **Check for changes now** |
| "File storage (SharePoint) is unavailable right now" for users | Microsoft 365 outage or throttling | wait; nothing was saved or changed |
| Someone cannot open their department folder in SharePoint | blank or unmapped Department, a local (non-Entra) account, or no mailbox | A7, B8; the error list on **Administration → SharePoint** |
| "Other access on department folders" | someone shared a folder directly in SharePoint | remove it in SharePoint if it was not intended; PDMS never removes it |
| "Files PDMS does not track" | someone added a file in SharePoint, or an upload failed half-way ("PDMS (a save that did not finish)") | delete it in SharePoint if it does not belong, then **Dismiss** |
| A document page says "A file of this document was changed outside PDMS" | the change check (or a download) found the file changed or deleted in SharePoint | an administrator resolves it on **Administration → SharePoint**; downloads of it are paused until then |

### C3. Turning SharePoint storage off

On **Administration → SharePoint**, under **Settings → Storage**, type the
site address and choose **Turn SharePoint storage off**. New document files
are stored on the server again. Files already in SharePoint stay there, but
PDMS can no longer open them until storage is turned back on, which is why
it asks for the address. The Microsoft 365 connection stays, for the Entra
ID user sync.

While SharePoint holds PDMS files, the site and library can't be changed:
PDMS tracks each file by its ID in that library.

If `config/pdms.toml` sets `[storage] backend` or the `[sharepoint]` values,
the page shows them read-only; change them in the file instead. An install
set up by an earlier installer moves them into Administration when it
upgrades (ADMIN_GUIDE section 1).

## Rollout on the test server

> **Step 1 erases the test server's database and files.** Back up first if
> anything on it matters.

1. On the test server: `cd <install directory> && git pull && docker compose down -v`.
2. `./install.sh`, then do B3 to B6 on the admin pages.
3. `docker compose run --rm app seed-demo`. The demo files go to the test site.
4. Optional, from a workstation with the repository: run the three opt-in
   live tests against the test site (`live_sharepoint_round_trip`,
   `live_pdms_writes_are_recognised`, `live_department_staff_get_read_access`
   in `crates/pdms-core/tests/sharepoint.rs`; each lists the variables it
   needs). They sign in with a certificate file
   (`PDMS_LIVE_CERTIFICATE_FILE`), so they need one made for the purpose
   and uploaded to the app registration as well, not the certificate made
   on the Connection page. Delete the file afterwards.
5. Do B7 to B9.
