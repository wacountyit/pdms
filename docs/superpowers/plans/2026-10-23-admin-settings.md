# S1: Settings in Administration. Implementation plan

- **Date:** 2026-10-23
- **Spec:** [2026-10-23-admin-settings-design.md](../specs/2026-10-23-admin-settings-design.md), approved by the owner on 2026-10-23.
- **Status:** in progress. Phase 1 done (2026-10-23).
- **How it is built:** directly on `main`, one phase at a time. Each phase ends with the checks below, its ROADMAP §0.1a state, and commit and push commands for the owner. Commit messages start with `S1:`.
- **Numbers:** migration `0015_settings.sql`; decisions from D-111 (both the next free on `main` on 2026-10-23).
- **Checks after every phase:** `cargo fmt --all`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo test --workspace`; `cargo sqlx prepare --workspace --check -- --all-targets` when queries changed; `bash -n install.sh` when the installer changed; the screenshot tool for every page a phase adds or changes (dark and light).
- **Rules that always apply** (CLAUDE.md): SPDX headers (test files: SPDX first, then `#![allow(`); no `unwrap`/`expect`/`panic` outside tests; the CSP and one `<h1>` per page; R2's admin page pattern; every change of state audited in its own transaction; never edit a pushed migration.

## Terms

- **In-scope key:** a setting the admin pages manage, named like the config path: `email.smtp_host`, `auth.oidc.client_secret`, `storage.backend`. The full list is the registry in Phase 1.
- **Locked key:** set in `config/pdms.toml` or a `PDMS_*` variable (a `PDMS_*_FILE` counts only when the file is not empty). Shown read-only.
- **File config:** what `Config::load` reads today. **Effective config:** the file config with the database values applied to the keys that aren't locked.
- **Live snapshot:** the effective config plus the services built from it (Graph client, SharePoint, mailer, OCR client, virus scanner), swapped as one.

---

## Phase 1: Foundation

> **As built (2026-10-23):**
> - Task 1.1 was the planned pure refactor (485 tests unchanged). Tests that replaced `core.mailer` or `core.ocr` now call `Core::set_mailer` / `Core::set_ocr`; `Live` is `Clone`; `status::parts` returns owned `(GraphClient, Resolved)`. It is in the same commit as the rest of the phase.
> - `app_secrets` has no `nonce` column: PDMS's `encrypt` stores `nonce || ciphertext` in one value, bound to the setting's key (associated data). The key check allows `-` too.
> - `settings::get` / `set` work on the configuration's JSON form (one path per key), so the configuration's own types check every value; no per-key match.
> - Locked keys are found with figment's `find_value` on the TOML file (no new `toml` dependency) and by the `PDMS_*` / non-empty `PDMS_*_FILE` variables.
> - **The worker is rebuilt in the same process** (`maintenance::run_worker`), not by exiting it: the same in Docker, under systemd and with `serve --with-worker` (D-112).
> - `Core::reload` runs once at start (in `pdms`'s `connect`, after the migration check) and from `settings::watch`; a failure keeps the running settings.
> - Tests: `tests/settings.rs` (8), unit tests in `settings` and `config` (lock rule).

**Goal:** settings can be stored, merged, validated, audited and applied live; the worker restarts on a change; `pdms settings show` lists them. No admin page yet, and behaviour is unchanged while the database holds no settings.

### ✓ Task 1.1: The live snapshot (pure refactor, its own commit)

- **Files:** `crates/pdms-core/src/lib.rs`; every crate that reads `core.config`, `core.sharepoint`, `core.graph`, `core.mailer`, `core.ocr` or `core.scanner` (about 220 uses).
- **Change:**
  - `Core` keeps `db`, `keys`, `policy`, `storage` and `signatures`, gains `file_config: Arc<Config>` and `live: Arc<RwLock<Arc<Live>>>`, and drops the public `config`, `sharepoint`, `graph`, `mailer`, `ocr` and `scanner` fields.
  - `pub struct Live { pub config: Arc<Config>, pub graph: Option<GraphClient>, pub sharepoint: Option<SharePoint>, pub mailer: Arc<dyn Mailer>, pub ocr: Option<Arc<dyn OcrProvider>>, pub scanner: Option<Arc<dyn VirusScanner>>, pub version: i64 }`, built by `Live::build(&Config, version) -> CoreResult<Live>` (today's code from `Core::new`).
  - Accessors: `config() -> Arc<Config>`, `graph()`, `sharepoint()`, `mailer()`, `ocr()`, `scanner()` (each clones out of the current snapshot under a short read lock), and `live() -> Arc<Live>`, so one request can read a consistent set.
  - Mechanical rewrite of the call sites (`core.config.` → `core.config().`, `core.sharepoint` → `core.sharepoint()`, and so on). In tests, `(*core.config).clone()` becomes `(*core.config()).clone()`.
- **Tests:** none new. All existing tests must pass unchanged, which is the point of this commit.

### ✓ Task 1.2: Migration 0015

`migrations/0015_settings.sql`:
- `app_secrets (key text PRIMARY KEY CHECK (key ~ '^[a-z][a-z0-9_.]{1,127}$'), ciphertext bytea NOT NULL, nonce bytea NOT NULL, updated_by uuid REFERENCES users (id), updated_at timestamptz NOT NULL DEFAULT now())`; app role: SELECT, INSERT, UPDATE, DELETE.
- `graph_certificates (id uuid PRIMARY KEY, thumbprint_sha256 text NOT NULL, thumbprint_sha1 text NOT NULL, not_after timestamptz NOT NULL, state text NOT NULL CHECK (state IN ('pending','active','retired')), created_by uuid REFERENCES users (id), created_at timestamptz NOT NULL DEFAULT now())`, and a partial unique index so at most one row is `active`; app role: SELECT, INSERT, UPDATE.
- `settings_version (id boolean PRIMARY KEY DEFAULT true CHECK (id), version bigint NOT NULL)` with one row `(true, 0)`; app role: SELECT, UPDATE.

### ✓ Task 1.3: The registry, precedence and encryption

- **New module `pdms-core::settings`:**
  - `pub struct SettingDef { key: &'static str, kind: Kind, area: Area }` with `Kind::{Bool, Text, Port, Enum(&[&str]), Time, Secret}` and `Area::{Microsoft365, SignIn, EntraSync, SharePoint, Email, Ocr, VirusScanning}`. `SETTINGS: &[SettingDef]` lists every in-scope key from spec §2: `graph.tenant_id`, `graph.client_id`, `sharepoint.site_url`, `sharepoint.library`, `sharepoint.delta_interval_minutes`, `storage.backend` (local or sharepoint only), `entra_sync.enabled`, `entra_sync.run_at`, `entra_sync.attribute`, `entra_sync.include_guests`, `auth.oidc.enabled`, `auth.oidc.issuer_url`, `auth.oidc.client_id`, `auth.oidc.client_secret`, `auth.oidc.display_name`, `auth.oidc.auto_provision`, `email.transport`, `email.smtp_host`, `email.smtp_port`, `email.smtp_security`, `email.smtp_username`, `email.smtp_password`, `email.from`, `email.reply_to`, `ocr.text_extraction`, `ocr.sidecar_url`, `ocr.suggest_dates`, `storage.clamav_addr`, `storage.clamav_required`. The Graph certificate is not a key: it comes from `graph_certificates` (Phase 2).
  - `fn get(config: &Config, key) -> Value` and `fn set(config: &mut Config, key, Value) -> CoreResult<()>`, one match per key, with a unit test that every registry key round-trips.
- **Locked keys:** `Config::load_from` also records `locked: BTreeSet<String>` (a `#[serde(skip)]` field): a key is locked when the TOML file has it (parsed as a `toml::Table`, path lookup), or when `PDMS_<SECTION>__<KEY>` is set, or `…_FILE` names a non-empty file. Unit tests cover each case, including the empty secret file.
- **Encryption:** `Keys::derive` gains a key for settings secrets (its own HKDF label, `settings`), with `encrypt_setting(key, plaintext)` and `decrypt_setting(key, ciphertext, nonce)`. The setting's key is the associated data, so a ciphertext can't be moved to another key.
- **Loading:** `settings::load_overrides(db, keys) -> CoreResult<BTreeMap<String, Value>>` reads `app_settings` (the in-scope keys only; branding keys stay Branding's) and decrypts `app_secrets`. `settings::effective(file: &Config, overrides) -> CoreResult<Config>` applies the overrides to the keys that aren't locked, then runs `Config::validate`.

### ✓ Task 1.4: Saving and applying

- `pub async fn save(core, ctx, changes: Vec<Change>) -> CoreResult<()>` where `Change { key, value: NewValue::{Set(Value), Clear, KeepSecret} }`:
  - needs `ManageSettings`; refuses locked keys and unknown keys;
  - builds the would-be effective config and validates it, then builds a `Live` from it before saving, so a save that can't take effect is refused with the reason;
  - in one transaction: upsert `app_settings` and `app_secrets`, bump `settings_version`, append `settings.changed` (category admin; context: each key with old and new value, secrets as `"replaced"` or `"cleared"`), `NOTIFY pdms_settings`;
  - then `core.reload()`.
- `pub async fn reload(&self) -> CoreResult<bool>` on `Core`: reads `settings_version`; when it differs from the snapshot's, loads the overrides, builds a new `Live`, and swaps it in. If building fails it keeps the old snapshot, logs the error, and returns it. `Core::connect` calls it once after connecting.
- `pub fn watch(core: Core) -> tokio::sync::watch::Receiver<i64>`: spawns a task that listens on `pdms_settings` (`sqlx::postgres::PgListener`) and also checks every 60 seconds, calls `reload`, and publishes the new version.
- **Worker restart:** `maintenance::worker(&core).run(rx)` already stops on its shutdown channel. `pdms worker` also stops when the settings version changes, and exits 0, so Docker restarts it with the new settings. `serve --with-worker` restarts its in-process worker instead.
- **Tests:** precedence (a file value beats the database, which beats the default); a save of a locked key is refused; an invalid combination is refused and nothing is stored; a secret is stored encrypted, decrypts, and is never in the audit event; `settings.changed` is in the save's transaction; a second `Core` on the same database sees the change through `reload`; a failed build keeps the old snapshot.

### ✓ Task 1.5: `pdms settings show`

Prints every registry key: the effective value (secrets as `set` or `not set`) and where it came from (`file`, `database` or `default`). `--json` too.

**Docs in this phase:** ARCHITECTURE §10 (configuration: the settings layer and the precedence rule). DECISIONS D-111 (the owner's choices, spec §1) and D-112 (precedence and live apply, spec §3 and §5).

---

## Phase 2: Microsoft 365 connection

**Goal:** the "PDMS Service" app is set up on a new page, with the certificate made in PDMS.

- **Menu:** a new group **Microsoft 365** in `Layout::admin_groups()`: Connection (`/admin/microsoft-365`), Sign-in (Phase 4), Entra ID sync and SharePoint (moved here from *People and access* and *Integrations*). Update `tests/shell.rs` and the screenshot list.
- **Certificates (`pdms-core::graph::certs`):**
  - `generate(core, ctx)` creates a `pending` certificate (`graph::cert::generate`, two years), with its private key in `app_secrets` (`graph.certificate.<id>`), audited `graph.certificate_created` (thumbprints);
  - `activate(core, ctx, id)` makes it `active` and the old one `retired`, in one transaction, audited `graph.certificate_activated`, then bumps the settings version (the Graph client is rebuilt);
  - `public_crt(id)` gives the PEM of the public certificate for download;
  - the effective `graph.certificate` is the file secret when it isn't empty (locked), otherwise the active certificate's key.
- **Page `/admin/microsoft-365`** (`ManageSettings`), in R2's pattern:
  - settings panel with the tenant ID and client ID (GUID check);
  - certificate panel: active and pending certificates with both thumbprints, the expiry date, a download link for each `.crt`, and **Generate certificate**, **Use the new certificate** and **Retire**;
  - **Test connection** runs with the form's values and the newest certificate: token request, then `GET /users?$top=1&$select=id`. It shows "Signed in to Microsoft Graph as the PDMS Service app", or the Graph error in plain words (consent, certificate not uploaded, wrong IDs);
  - a link to SHAREPOINT_SETUP.md B1 and B2.
- **Expiry reminder:** a daily job notifies administrators 30 days before the active certificate expires (dedupe key per certificate).
- **Tests:** generate, activate and retire with audit; the download is the public half only; the test against the mock Graph for success, a refused sign-in, and wrong IDs; the page's fields are disabled when locked; no secret in the HTML; the expiry notice.

**Docs:** SHAREPOINT_SETUP.md B3 and B4 describe the page instead of the installer; ADMIN_GUIDE "Entra ID user sync" setup uses the page; D-113 (the certificate in PDMS, with pending and active certificates).

---

## Phase 3: SharePoint and the Entra ID user sync

- **SharePoint page:** a **Settings** panel above the status panels: site address, library, check interval, and the storage switch.
  - **Test** with the form's values: resolve the site and library (P3's `SharePoint::resolved`), list the columns that would be created, check the three folders, and read the library's permissions (proves full control). It changes nothing.
  - **Turn on** (`storage.backend = "sharepoint"`): refused while any document file exists (`blobs` count, shown), or when the test fails.
  - **Turn off:** asks for the site address to be typed, and warns that PDMS can no longer open files stored in SharePoint.
  - **Site and library** can't change while `sharepoint_files` has rows.
  - The worker restarts on the change, which registers or unregisters the SharePoint jobs.
- **Entra ID sync page:** a **Settings** panel: on or off, nightly time, attribute, include guests. Turning it on needs a working Microsoft 365 connection and Microsoft sign-in (today's `Config::validate` rule). **Preview** reads the first page of users without changing anything.
- **Tests:** every rule above; the test makes no change in the mock (no folder or column created); switching on and off rebuilds the snapshot and the worker's jobs.

**Docs:** ADMIN_GUIDE "SharePoint storage" and "Entra ID user sync"; SHAREPOINT_SETUP.md B3 and C3; D-114 (the switch rules, spec §7).

---

## Phase 4: Microsoft sign-in

- **Page `/admin/sign-in`** (`ManageSettings`): on or off, tenant ID (the issuer `https://login.microsoftonline.com/<tenant>/v2.0` is built from it; a full issuer URL is shown read-only when the file sets one), client ID, client secret (write-only), button label, create accounts on first sign-in. The redirect URI is shown with a copy-friendly field and a note about the port.
- **Test:** fetch the discovery document for the form's values and check that the client ID is accepted (an authorization request URL is built and checked for the expected parameters; no sign-in happens). **Try sign-in** opens Microsoft's sign-in in a new tab, using the saved settings.
- **Live:** `AppState.oidc` becomes swappable (`RwLock<Option<Arc<OidcProvider>>>`). The web server rebuilds it when the settings version changes. If discovery fails, Microsoft sign-in shows as unavailable, with the error on the page, and local sign-in keeps working.
- **Lockout rule:** saving is refused when it would leave no way to sign in (`auth.local.enabled` off and sign-in off or failing its test).
- **Tests:** against the existing mock OIDC provider: test success and failure; live rebuild after a save, without restarting the app; the lockout rule; the secret is never in the HTML.

**Docs:** ADMIN_GUIDE §2 "Microsoft Entra ID" describes the page (the app registration steps stay); the installer's redirect-URI note points to the page.

---

## Phase 5: Email, OCR and virus scanning

- **Email page `/admin/email`:** on or off (`email.transport` smtp or disabled), host, port, security, username, password (write-only), from, reply-to. **Send a test email** to an address typed on the page (audited, without the address list being stored). The mailer is rebuilt live.
- **OCR page (existing `/admin/ocr`):** a **Settings** panel: read text in files, OCR for scans (the sidecar address stays `http://ocr:8080` unless locked), suggest dates. **Test** reaches the OCR service. If its container isn't running, the page says to re-run the installer and include it.
- **Virus scanning page `/admin/virus-scanning`:** on or off (the address stays `clamav:3310` unless locked), refuse uploads when the scanner is down. **Test** sends the EICAR test string and expects it to be found. If the container is missing, the page says so.
- **Tests:** each test button against Mailpit, the OCR fake, and a fake clamd; the live rebuild; fields locked by the file.

**Docs:** ADMIN_GUIDE email, OCR and virus scanning sections.

---

## Phase 6: Upgrade and the installer

- **`pdms settings`** gains `set KEY VALUE`, `set-secret KEY` (value from standard input), `test AREA`, and `import`. They use the same checks and audit as the pages, as the system actor.
- **`pdms settings import`:** for each in-scope key that the installer's managed blocks set (`sso`, `smtp`, `sharepoint`, `ocr`, `storage`/`clamav`) and that the database doesn't have yet, copy it into the database; import a non-empty `secrets/graph_certificate` as the active certificate; print what was imported.
- **Installer:**
  - removes the sign-in, email, SharePoint, OCR and virus-scanning questions, and their `--configure` options. Each removed option prints where the setting now lives;
  - adds "Include the OCR service? / Include the virus scanner?" (Compose profiles, recorded in `.env`), asked once;
  - **on an upgrade** where those managed blocks exist, offers once: "Move these settings into Administration?". On yes: build, migrate, `docker compose run --rm app settings import`, then remove the managed blocks from `config/pdms.toml`, empty `secrets/oidc_client_secret`, `secrets/smtp_password` and `secrets/graph_certificate`, and restart. Values written by hand stay in the file and are listed as locked;
  - the closing summary says to finish setup under Administration.
- **Tests:** the P4 installer harness for a fresh install, an upgrade with each block, the move, and a re-run that changes nothing; `import` copies what the blocks set and nothing else, and secrets arrive encrypted.

**Docs:** README "Install", ADMIN_GUIDE §1 (the installer table) and §3, SHAREPOINT_SETUP.md B3, BACKUP.md (secrets now in the database: the master key backup matters even more; `graph_certificate` may be empty), D-115 (the installer after S1, and the move).

---

## Phase 7: Records and the full check

- ROADMAP: §0.3 S1 to **Done**, §0.1 entry with the test count, §0.1a row removed, §0.5 and §0.6, §4 (the "Settings" gap closed for these areas; the out-of-scope settings listed), §5 "Next" item 7 marked as done by S1.
- ARCHITECTURE §10 and §12 (migration `0015`); README; CLAUDE.md "Start here".
- DECISIONS: status lines; the "Open questions" defaults that now live on pages.
- The plan's "As built" notes and checkboxes; the full check; screenshots of every page in both themes.
