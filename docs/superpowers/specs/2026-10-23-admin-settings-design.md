# Settings in Administration (S1): design

- **Date:** 2026-10-23
- **Status:** approved by the owner, 2026-10-23.
- **Item:** S1 in ROADMAP §0.3. Commit messages start with `S1:`.
- **Why:** today an administrator has to log on to the server and run
  `./install.sh --configure …` (or edit `config/pdms.toml`) to set up
  SharePoint, Microsoft sign-in, the Entra ID user sync, email, OCR or virus
  scanning. The owner wants the installer to be for the first installation
  of the server only. Everything else is set up, changed, tested and checked
  in **Administration**, in the browser. This also closes the ROADMAP §4 gap
  "Most settings live in `config/pdms.toml`" and §5 "Next" item 7 ("Admin
  settings screen").

## 1. Decisions already made (owner, 2026-10-23)

1. **The configuration file wins.** A value set in `config/pdms.toml` or in a
   `PDMS_*` environment variable is used, and the admin page shows it
   read-only ("set in the configuration file"). Everything else is set on the
   page. So nothing an administrator wrote by hand changes behind their back.
2. **Changes apply without a manual restart.** The web server applies them
   live. The worker restarts itself (§5), which nobody notices.
3. **Secrets are stored in the database, encrypted** with the master key: the
   sign-in client secret, the SMTP password, and the Microsoft Graph
   certificate's private key. A secret is never shown again once saved. The
   trade-off is that a database backup now holds them, encrypted, so the
   master key's backup matters even more (BACKUP.md says so).
4. **Permission:** the existing `ManageSettings` capability (organisation-wide
   administrators). Every change is audited, without secret values.
5. **The installer** keeps the first installation, `--configure network`
   (because a broken address or port can lock everyone out of the admin page),
   `--configure database-port`, `--configure source` and `--configure admin`.
   The steps for sign-in, email, SharePoint, OCR and virus scanning leave it.
   A `pdms settings` command (§10) is the fallback for a server whose admin
   page can't be reached.

## 2. Scope

**In scope**, each on an admin page with a test button:

| Area | Settings on the page | Test |
|---|---|---|
| Microsoft 365 connection (the "PDMS Service" app, used by the Entra sync and SharePoint) | tenant ID, client ID; the certificate: **generate it in PDMS**, download its public `.crt`, see both thumbprints (SHA-1 and SHA-256) and the expiry date, renew it | sign in to Microsoft Graph and read one user |
| SharePoint storage | site address, library, check interval; turn on, turn off (§7) | connect to the site and library; check the columns and the three folders; check that PDMS may manage permissions |
| Entra ID user sync | on or off, the nightly time, the Entra attribute that holds the department, include guests | the existing **Sync now**, plus a read-only preview of the first page of users |
| Microsoft sign-in (SSO) | on or off, tenant (the issuer is derived from it), client ID, client secret, button label, create accounts on first sign-in; the exact redirect URI to copy | fetch the sign-in configuration (OIDC discovery) and check the client ID; a "Try sign-in" link that opens Microsoft's sign-in in a new tab |
| Email | on or off, SMTP host, port, security, username, password, from and reply-to addresses | send a test email to an address the administrator types |
| Text extraction (OCR) | read text in files on or off; OCR for scans on or off; suggest dates on or off | reach the OCR service |
| Virus scanning | on or off; refuse uploads when the scanner is down (on or off) | reach ClamAV and scan the EICAR test string |

**Out of scope** (stays in the installer or the file):
- the database and its passwords, the master key, ports, Caddy and HTTPS, the
  public address (`--configure network`), and creating the first
  administrator. PDMS can't start without these, or they live outside the
  app's container;
- **which optional containers exist.** OCR and ClamAV run in their own
  containers, and the app can't start them. The installer asks once
  "Include the OCR service / the virus scanner?" (Compose profiles); the admin
  page turns their use on and off and says when the container is missing;
- the time zone, the upload size limit, the API and rate-limit settings,
  S3 storage, and audit forwarding: rarely changed, and they can follow in a
  later round with the same pattern.

## 3. Where a value comes from

For each in-scope key (for example `email.smtp_host`) the effective value is
the first of:

1. **the configuration file or environment**, when the key is set there. A
   secret file (`PDMS_*_FILE`) counts only when the file is not empty, so the
   always-mounted, empty `secrets/*` files don't lock anything;
2. **the database** (set on the admin page);
3. the built-in default.

Config loading records which in-scope keys came from 1, so the page can show
"set in the configuration file" and disable that field. The values in the
database are merged into the same `Config` structure the app uses today, and
`Config::validate` runs on the merged result: an invalid combination is
refused when an administrator saves it, never discovered at the next start.

**Storage:**
- non-secret values: the existing `app_settings` table (key, JSON value,
  who, when), as Branding already does;
- secrets: a new table `app_secrets` (key, ciphertext, nonce, who, when),
  encrypted with a key derived from the master key for this purpose only.
  The app may read and write it; it never leaves the server except in
  backups.

## 4. The admin pages

All pages follow R2's admin page pattern (R2 spec §5.2). The menu gains a
group **Microsoft 365** (Connection, Sign-in, Entra ID sync, SharePoint), and
*System* gains **Email** and **Virus scanning** next to **Text extraction
(OCR)**. Existing pages (Entra ID sync, SharePoint, OCR) get a **Settings**
panel above their status panels instead of moving.

Each settings panel:
- a form with the fields above. A field set in the configuration file is shown
  disabled with "set in the configuration file";
- secrets are write-only: "Set (changed 2026-10-23 by Avery Admin)" and a
  **Replace** field; leaving it blank keeps the stored value;
- **Test** runs with the values in the form, **without saving them**, and shows
  the result on the page (what worked, and the error in plain words when not);
- **Save** checks the values (format and `Config::validate`) and applies them
  (§5). Turning a service *on* also runs the test first, and saving is refused
  if it fails, so a half-configured service never goes live;
- a short "what this does" line, and a link to the guide section.

**Microsoft 365 connection page, certificate:**
- **Generate certificate** creates the key pair in PDMS (P2's
  `graph::cert::generate`, RSA-2048, two years), stores the private key in
  `app_secrets`, and offers **Download the public certificate (.crt)** to
  upload to the app registration. The page then shows the SHA-1 thumbprint
  (what the Entra admin center lists) and the SHA-256 thumbprint;
- **Renew** makes a new certificate that waits as "pending" until **Use the
  new certificate** is chosen. That allows a renewal without an outage: upload
  the new `.crt` first, then switch. Both certificates stay listed until the
  old one is removed;
- the page warns 30 days before expiry, and administrators get a notice then;
- an existing `secrets/graph_certificate` file is read-only on the page (rule
  §3.1) and can be imported (§9).

## 5. How changes apply

- **Web server: live.** The services built from these settings (the Graph
  client, SharePoint, the mailer, the OCR client, the virus scanner, the sign-in
  provider) sit behind one swappable snapshot. A save builds the new services
  first, and only swaps them in if building them worked; requests already
  running finish with the old ones. Call sites read the current snapshot
  (`core.sharepoint()` instead of the field `core.sharepoint`), a mechanical
  change in about 80 places.
- **Worker: restarts itself.** Its jobs and schedules are registered at start
  (some only when a service is on), so it re-reads the settings by exiting
  cleanly. Docker restarts it at once (`restart: unless-stopped`); jobs are
  leased, so a job that was running is picked up again. With
  `serve --with-worker` the worker restarts inside the process.
- **Telling the other process:** every save writes the audit event and the
  values in one transaction, then sends a PostgreSQL `NOTIFY pdms_settings`.
  Both processes listen, and also check a settings version every 60 seconds
  in case a notification was missed.
- **Sign-in:** the provider is rebuilt (discovery is fetched again). If that
  fails, Microsoft sign-in is shown as unavailable and the error is on the
  page; local break-glass sign-in keeps working (as today when Entra is down
  at start, D-021).

## 6. Permissions and audit

- `ManageSettings` for every settings page and test. The status parts of
  the SharePoint page keep `ManageSharePoint` (both are held by the same
  role today).
- Every save writes one `settings.changed` event (category admin) in the same
  transaction: the keys that changed, with old and new values, except secrets,
  which are recorded only as "replaced". Tests write `settings.tested` with the
  result, without the values. Certificate generation, renewal and switching
  are audited (`graph.certificate_created`, `graph.certificate_activated`),
  with thumbprints.
- Nothing here widens who can see documents (D-085, D-090).

## 7. Safety rules

- **SharePoint storage can be turned on only while no document files exist**
  (D-102 point 4): files stored on the server can't be read once SharePoint
  is on. The page says how many files there are and why it is refused. A test
  server is reset first, as SHAREPOINT_SETUP.md describes.
- **Turning SharePoint off** is allowed only after typing the site address to
  confirm, and the page says that files stored in SharePoint can no longer be
  opened by PDMS.
- **The site address and library can't change while SharePoint holds
  files**, because PDMS tracks them by ID in that library. The tenant and app
  can change (a new app registration for the same site), after a successful
  test.
- **Turning Microsoft sign-in off, or changing it,** can't lock everyone out:
  saving is refused unless local break-glass sign-in is on, and the page
  says so.
- **Email "off"** keeps in-app notices working; emails are not queued while
  it is off.
- **A test never changes anything** in SharePoint, Entra or the mailbox, except
  sending the one test email the administrator asked for.

## 8. Data model

Migration `0015_settings.sql` (the next free number on `main` today):
- `app_secrets (key text primary key, ciphertext bytea, nonce bytea,
  updated_by uuid, updated_at timestamptz)`, readable and writable by the app
  role, never by anyone else;
- `graph_certificates (id, thumbprint_sha256, thumbprint_sha1, not_after,
  state: pending | active | retired, created_by, created_at)`, with the
  private key of each in `app_secrets` (`graph.certificate.<id>`);
- `settings_version` (one row, a counter bumped by every save), for the
  60-second check.

## 9. Upgrading existing installs

Values set by today's installer live in `config/pdms.toml` blocks and
`secrets/*` files, so under rule §3.1 they would lock the new pages. On
upgrade, `./install.sh` (which an upgrade runs anyway) offers once:
"Move the sign-in, email, SharePoint, OCR and virus-scanning settings into
Administration, so they can be changed there?" On yes it runs
`pdms settings import`, which copies them into the database (secrets
encrypted), then removes those managed blocks from `config/pdms.toml` and
empties the secret files. Values written by hand outside the installer's
blocks are left in the file and stay read-only on the page, with a note on how
to move them. The Graph certificate file is imported as the active certificate.

## 10. The fallback command

`pdms settings` (through `docker compose run --rm app settings …`):
- `show`: every in-scope key, its effective value (secrets as "set"), and where
  it came from (file, database, default);
- `set KEY VALUE` and `set-secret KEY` (reads the value from standard input):
  the same checks and audit as the page, as the system actor;
- `import`: §9;
- `test AREA`: the same tests as the page.

This is for a server whose admin page can't be reached, for example when
sign-in is broken and no break-glass account is left.

## 11. The installer after S1

The first installation asks: the network and address, the name and time
zone, the optional database port, the source URL, the optional containers
(OCR, virus scanner), and the first administrator. At the end it prints:
"Set up Microsoft sign-in, email, SharePoint and the rest under
Administration once you are signed in." The removed `--configure` options
print where the setting now lives. ADMIN_GUIDE §1 to §3 and
SHAREPOINT_SETUP.md B3 change to describe the admin pages instead.

## 12. Tests

- **Core:** precedence (file beats database beats default; an empty secret
  file doesn't lock); `Config::validate` on the merged config refuses bad
  combinations; secrets are encrypted at rest and never returned by `show`;
  every save is audited in its transaction; the storage switch and lockout
  rules (§7); the certificate generate, renew and activate cycle; the
  snapshot swap (a failed build leaves the old services in place).
- **Against the mock Graph and a mock SMTP server** (both exist): every test
  button's success and failure.
- **Web:** each page follows the admin pattern; fields set in the file are
  disabled; secrets are never in the HTML; CSRF on every form; only
  `ManageSettings` holders can see the pages; screenshots in dark and light.
- **Installer:** the harness from P4 checks the upgrade offer, the import, and
  that a re-run changes nothing.

## 13. Phases (the plan will detail them)

1. **Foundation:** migration `0015`, settings storage with precedence and
   encryption, the snapshot swap in the web server, worker restart on change,
   `NOTIFY` plus the version check, `pdms settings show`.
2. **Microsoft 365 connection:** the page, the certificate in the app, the
   Graph test.
3. **SharePoint and the Entra ID sync settings:** the settings panels, the
   tests, and the §7 rules.
4. **Microsoft sign-in:** the page, live rebuild of the provider, the lockout
   rule.
5. **Email, OCR and virus scanning.**
6. **Upgrade and installer:** `pdms settings import`, the installer's upgrade
   offer, the reduced installer, and the rest of the CLI.
7. **Docs:** ADMIN_GUIDE, SHAREPOINT_SETUP.md, BACKUP.md (the master key and
   secrets in the database), ARCHITECTURE, DECISIONS, ROADMAP.

## 14. Decisions to record

From D-111 (the next free number): the owner's choices in §1, the precedence
rule (§3), how changes apply (§5), and the safety rules (§7).
