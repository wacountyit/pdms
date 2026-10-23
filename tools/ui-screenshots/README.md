# UI screenshots

`shoot.py` signs in to a local PDMS instance and takes a full-page screenshot
of every page. It also flags common layout faults. It was used to check UI
redesign round 1 (R1) and is meant for round 2 (R2): see
[the R2 brief](../../docs/superpowers/specs/2026-10-21-ui-redesign-r2-brief.md).

For each page, `out/report.json` records:
- the HTTP status;
- whether the page scrolls sideways (`overflow`), and which elements stick out past the viewport (`wide`);
- the visible `<h1>` headings (a page should have exactly one);
- browser console errors and warnings, which include Content Security Policy violations.

The script also:
- shoots the sign-in page signed out, and the "Microsoft sign-in is not set up" notice;
- shoots the five R1 screens at phone width (390×844), and every page at 1440×900;
- does a keyboard pass (Tab ten times on the dashboard, with screenshots of the focus);
- shoots the document page with "Manage this document" open;
- shoots the narrow-screen menu open;
- runs an overflow sweep at 1024, 768 and 390 px.

The run needs Docker on a Linux host. Everything else runs inside
Microsoft's Playwright image. `run.sh` uses `--network host` to reach the
local instance at 127.0.0.1. Docker Desktop on macOS or Windows doesn't
support that by default. There, either turn on host networking in Docker
Desktop, or drop `--network host` from `run.sh` and set
`PDMS_SHOTS_BASE=http://host.docker.internal:18080`.

## 1. Start a local instance with demo data

Use the development database from the README ("Develop"), with the demo data
loaded (`pdms seed-demo`). Then serve on port 18080, from the branch you want
to check:

```sh
export PDMS_DATABASE__URL=postgres://pdms_app:devapp@127.0.0.1:55432/pdms
export PDMS_DATABASE__OWNER_URL=postgres://pdms_owner:devowner@127.0.0.1:55432/pdms
export PDMS_SECRET_KEY=$(cargo run -q -p pdms -- gen-secret)
export PDMS_SERVER__BIND=127.0.0.1:18080 PDMS_SERVER__PUBLIC_URL=http://127.0.0.1:18080
export PDMS_SERVER__SECURE_COOKIES=false
export PDMS_AUTH__LOCAL__REQUIRE_TOTP=false
export PDMS_AUTH__LOGIN_ATTEMPTS_PER_MINUTE=100   # the script signs in several times
export PDMS_STORAGE__LOCAL_PATH=data/blobs PDMS_STORAGE__SPOOL_PATH=data/spool
export PDMS_EMAIL__TRANSPORT=disabled

cargo run -p pdms -- migrate
cargo run -p pdms -- seed-demo        # once; skip if the demo data is already there
cargo run -p pdms -- serve --with-worker
```

The demo accounts and their password are listed in the README; the script
signs in as `admin@example.org`, and as `alice@example.org` for the pages an
administrator may not open.

## 2. Write `ids.json`

The script needs the ids of a few demo records (a document, a user, a
position, …):

```sh
docker exec -i pdms-dev-db psql -U postgres -d pdms -tA < tools/ui-screenshots/ids.sql > tools/ui-screenshots/ids.json
```

## 3. Take the screenshots

From the repository root, in another shell:

```sh
tools/ui-screenshots/run.sh                              # every page
tools/ui-screenshots/run.sh desktop-admin phone-         # only pages whose names match
tools/ui-screenshots/run.sh --theme=light                # every page in light mode
```

- **Light and dark:** dark is every account's default. `--theme=dark`,
  `--theme=light` or `--theme=system` first sets the color scheme of each
  account the script signs in as (on **Your account → Appearance**), and
  writes to `out/<theme>/`. Check every page in both modes before a change to
  colors is merged.

- **What `run.sh` does:** it starts Microsoft's Playwright image (`mcr.microsoft.com/playwright/python:v1.49.0-jammy`) as your user.
  - The image has the browsers but not the Playwright Python package. The first run therefore installs the package into `tools/ui-screenshots/.pylib/`, which needs internet access once.
- **Settings:** `PDMS_SHOTS_BASE`, `PDMS_SHOTS_USER` and `PDMS_SHOTS_PASSWORD` override the defaults, and are passed through when set in your shell.
- **Output:** PNGs and `report.json` land in `tools/ui-screenshots/out/`. That folder, `.pylib/` and `ids.json` are git-ignored, so screenshots never get committed.

## 4. Check the results

Open the PNGs and `report.json`. Before a redesign round is merged, every page
must have:

- no horizontal page scroll (wide tables scroll inside `.table-wrap`);
- no overlapping or cut-off text;
- exactly one `<h1>`;
- no console errors (a CSP error means an inline style or script slipped in);
- a visible focus outline on everything you can Tab to.

When you add a page, add it to `DESKTOP_PAGES` (and to `PHONE_PAGES` if it is
a key screen). Pages that need a non-admin account go in `EDITOR_PAGES`.
