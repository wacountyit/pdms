# Policy and Document Management System (PDMS)

A self-hosted system of record for policies, contracts, partnership agreements,
and any other official document with review, renewal, or expiration dates. It
notifies the right people before those dates, tracks board and committee
appointments, enforces per-document and per-department access control, and
keeps a tamper-evident audit trail of every action.

The system is not tied to one organization. You choose its name when you
install it (for example "Example Org PDMS").

Written in Rust (Axum, SQLx, PostgreSQL), server-rendered HTML with HTMX, and
packaged as a single Docker image.

## Status

| Phase | Scope | State |
|---|---|---|
| 0 | Foundation: auth (Entra ID OIDC + break-glass local with TOTP), users, departments, groups, roles, central authorization, hash-chained audit log, job worker, packaging | **Done** |
| 1 | Documents: types, versioned custom fields, upload/versioning (local or S3 storage, ClamAV), tags, search/filter/export, sharing, ownership, retention and legal hold | **Done** |
| 2 | Review schedules and tasks, date-driven statuses, reminder rules, email and in-app notifications, calendar, ICS feeds, dashboards | **Done** |
| 3 | Review/approval workflow (reviewers, legal counsel step, approvers, separation of duties), signature tracking, boards and appointments with requirement checklists and term reminders | **Done** |
| 4 | Records request packages, reports (CSV), REST API with OpenAPI and scoped tokens, webhooks and integration providers, audit forwarding, access review reports, backup/restore with `verify-integrity` | **Done** |
| 5 | Threaded comments with @mentions and preserved edits; text extraction (PDF, Word) and optional OCR sidecar with date suggestions. (The optional Outlook calendar sync built here was later removed, D-087.) | **Done** |

**What's next:** the PADM plan (SharePoint document storage, Entra user sync, a UI
redesign, and more), with what is done and what remains, is in
[docs/ROADMAP.md, section 0](docs/ROADMAP.md#0-where-things-stand).
Start there if you are picking up the work, and read [CLAUDE.md](CLAUDE.md)
for the rules and commands that apply to every change.

## Install

Requirements: Linux with Docker Engine, the Docker Compose plugin, and `openssl`.

```sh
./install.sh
```

The installer asks everything first, then builds and starts unattended:

- **How people reach PDMS:** the bundled Caddy proxy with automatic HTTPS, your
  own reverse proxy, or local testing. It finds a free host port (8080, or the
  next free one). Caddy gets ports 80 and 443, or the next free ones if
  something else on the machine already has them.
- **Name and time zone** (detected from the host).
- **Optionally:** PostgreSQL on `127.0.0.1` for host tools (on a free port),
  Microsoft Entra ID sign-in, SMTP email, OCR, and virus scanning.
- **The first administrator.** The password is generated and shown once,
  along with an authenticator secret.

Database passwords and the master key are strong random values in `secrets/`.
Deployment settings go in `.env` and application settings in
`config/pdms.toml`. Re-running is safe; `./install.sh --configure smtp` (or
`sso`, `ocr`, `clamav`, `network`, `database-port`, `admin`)
redoes one part later. `./install.sh --help` lists the variables for unattended
installs. Then see [docs/ADMIN_GUIDE.md](docs/ADMIN_GUIDE.md) and the testing
plan in [docs/ROADMAP.md](docs/ROADMAP.md).

## Develop

You need Rust 1.90 or later, Docker, and `sqlx-cli` for migrations and the
SQLx offline data (the install command is in [CLAUDE.md](CLAUDE.md),
"Commands").

```sh
# Postgres for development and SQLx compile-time query checks
docker run -d --name pdms-dev-db -p 127.0.0.1:55432:5432 -e POSTGRES_PASSWORD=devsuper postgres:15-alpine
docker exec -i pdms-dev-db psql -U postgres -v owner_password=devowner -v app_password=devapp < deploy/postgres/00_roles.sql

export DATABASE_URL=postgres://pdms_owner:devowner@127.0.0.1:55432/pdms     # SQLx macros
export PDMS_DATABASE__URL=postgres://pdms_app:devapp@127.0.0.1:55432/pdms   # the app
export PDMS_DATABASE__OWNER_URL=$DATABASE_URL                                # migrations
export SQLX_OFFLINE=true   # the database is still empty, so build from the committed .sqlx/ data
export PDMS_SECRET_KEY=$(cargo run -q -p pdms -- gen-secret)
export PDMS_SERVER__SECURE_COOKIES=false PDMS_LOG__FORMAT=pretty
export PDMS_STORAGE__LOCAL_PATH=data/blobs PDMS_STORAGE__SPOOL_PATH=data/spool
export PDMS_EMAIL__TRANSPORT=file PDMS_EMAIL__FILE_DIR=data/mail   # reminder emails as .eml files

cargo run -p pdms -- migrate
unset SQLX_OFFLINE         # from now on the SQLx macros check queries against the database
cargo run -p pdms -- seed-demo                     # optional demo data
PDMS_AUTH__LOCAL__REQUIRE_TOTP=false cargo run -p pdms -- serve --with-worker
```

The demo includes sample documents: an interlocal agreement with its 2013 original
and 2014 amendment (signed by the presiding officer, waiting on the partner), a
contract, policies (one Restricted, one County-wide), a grant agreement, and a equipment inspection
contract in review waiting for legal counsel. Three boards show an overdue
reappointment decision, a holdover, a vacancy, and appointment history.
Demo users (password `demo-password-change-me`): `admin@example.org`,
`alice@example.org` (department head), `bob@example.org` (read-only default),
`carol@example.org` (legal counsel), `dana@example.org` (records officer),
`erin@example.org` (auditor), `jordan@example.org` (presiding officer, approves
everywhere), `gus@example.org` (operations engineer, reviewer), `frank@example.org`
(appointments clerk).

### Tests

```sh
cargo test --workspace          # needs Docker: tests start Postgres and a mock OIDC IdP
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
```

By default each test binary starts one throwaway `postgres:15-alpine`
container (labeled `pdms.testkit=postgres`). A small watcher removes it a few
seconds after the test process exits, even if the run is interrupted, so
containers do not pile up.

For local runs you can skip containers entirely and use the dev database
server. Each test still gets its own fresh database, named after the test
process, and those of finished runs are dropped the next time tests start.
Your `pdms` database is never touched:

```sh
export PDMS_TEST_DATABASE_URL=postgres://postgres:devsuper@127.0.0.1:55432/postgres
export PDMS_TEST_OWNER_PASSWORD=devowner PDMS_TEST_APP_PASSWORD=devapp   # the dev roles' passwords
cargo test --workspace
```

To remove anything left by older versions of the test kit:
`docker ps -aq --filter label=org.testcontainers.managed-by=testcontainers --filter ancestor=postgres:15-alpine | xargs -r docker rm -f`.

Tests also start SeaweedFS (S3 API), Mailpit (SMTP), and a mock OIDC provider;
those containers are removed when each test finishes. A real OCR test is opt-in too: `docker build -t pdms-ocr:test deploy/ocr`, then
`cargo test -p pdms-core --test phase5 -- --ignored`. A real ClamAV test is opt-in:
`cargo test -p pdms-storage --test clamav -- --ignored`.

After changing any `sqlx::query!` or a migration, refresh the offline metadata
(CI checks it): `cargo sqlx prepare --workspace -- --all-targets`.

## Layout

```
crates/pdms-authz    authorization engine (pure) + SQL predicate
crates/pdms-audit    audit events, canonical hashing, chain verification
crates/pdms-core     config, auth, directory, grants, documents, notifications, calendar,
                     workflow, signatures, appointments, comments, ocr (text and date
                     suggestions), exports, reports, API tokens, activity (the
                     dashboard feed),
                     integrations (webhooks, providers, audit forwarding), integrity, jobs
crates/pdms-storage  blob storage (local, S3), upload spooling, type sniffing, ClamAV
crates/pdms-web      HTTP routes (including the REST API and OpenAPI), middleware, templates
crates/pdms          the `pdms` binary (serve, worker, migrate, create-admin, verify-audit,
                     verify-integrity, ...)
crates/pdms-testkit  disposable test databases
migrations/          SQL migrations
deploy/              database bootstrap, reverse-proxy samples, backup and restore scripts,
                     the OCR sidecar (deploy/ocr)
docs/                ARCHITECTURE, DECISIONS, ADMIN_GUIDE, BACKUP, ROADMAP; superpowers/specs (designs)
                     and superpowers/plans (implementation plans)
tools/               developer tools (ui-screenshots: page screenshots for UI work)
```

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): design, security model, data model by phase
- [docs/DECISIONS.md](docs/DECISIONS.md): decision log and open questions
- [docs/ADMIN_GUIDE.md](docs/ADMIN_GUIDE.md): installation, SSO, users and access, documents, reminders, workflow, appointments, records requests, reports, API and webhooks, audit, operations
- [docs/ROADMAP.md](docs/ROADMAP.md): testing plan, pilot, go-live checklist, known gaps, and future additions
- [docs/superpowers/specs/](docs/superpowers/specs/): design specs for planned work (one per sub-project), and [plans/](docs/superpowers/plans/) for their step-by-step implementation plans
- [CLAUDE.md](CLAUDE.md): rules and commands for contributors and Claude sessions (public-repo rules, numbering, checks to run)
- [tools/ui-screenshots/](tools/ui-screenshots/README.md): screenshots of every page with layout checks, for UI work
- [docs/BACKUP.md](docs/BACKUP.md): what to back up, consistent backups (including enterprise backup systems), restore, and `verify-integrity`
- API reference: `/api/docs` on a running server, and `/api/v1/openapi.json`

## License

Copyright (C) 2026 PDMS contributors.

PDMS is free software: you can redistribute it and/or modify it under the terms
of the **GNU Affero General Public License** as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. It is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
PARTICULAR PURPOSE. See [LICENSE](LICENSE) for the full text.

**If you run PDMS for others, offer them the source.** Section 13 of the AGPL
requires that everyone who uses the system over a network can get the source
code of the version you run, including your changes. Every page links to
**Source code** (`/source`) and **License** (`/license`); set `[org] source_url`
(the installer asks, defaulting to the git remote) to the repository you
publish. Each source file carries an `SPDX-License-Identifier:
AGPL-3.0-or-later` line.

Third-party components keep their own licenses: the Rust crates in `Cargo.lock`
(MIT, Apache-2.0, BSD, ISC, and similar permissive licenses), the vendored
`htmx.min.js` (0BSD), and the container images used alongside PDMS
(PostgreSQL, Caddy, ClamAV, OCRmyPDF/Tesseract/Ghostscript).

