# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright (C) 2026 PDMS contributors
"""Visual check: screenshot every page of a local PDMS instance with demo data.

Used for the UI redesign (R1, R2). See README.md in this folder for how to
start the local instance and make ids.json. Run it from the host with
`tools/ui-screenshots/run.sh [name-filter ...]`, which runs this file inside
Microsoft's Playwright image.

Writes PNGs to /w/out/ and a findings summary to /w/out/report.json
(horizontal overflow, h1 count, console/CSP errors, HTTP status per page).
A name filter such as `desktop-admin` or `phone-` limits the run.
`--theme=dark`, `--theme=light` or `--theme=system` (or PDMS_SHOTS_THEME)
sets each signed-in account's color scheme first, and writes to
/w/out/<theme>/ instead. Without it, accounts keep their own setting (dark by
default).
"""

import json
import os
import sys

from playwright.sync_api import sync_playwright

BASE = os.environ.get("PDMS_SHOTS_BASE", "http://127.0.0.1:18080")
OUT = os.environ.get("PDMS_SHOTS_OUT", "/w/out")
USER = os.environ.get("PDMS_SHOTS_USER", "admin@example.org")
PASSWORD = os.environ.get("PDMS_SHOTS_PASSWORD", "demo-password-change-me")
IDS = json.load(open(os.environ.get("PDMS_SHOTS_IDS", "/w/ids.json")))

THEME = os.environ.get("PDMS_SHOTS_THEME", "")
FILTERS = []
for arg in sys.argv[1:]:
    if arg.startswith("--theme="):
        THEME = arg.split("=", 1)[1]
    else:
        FILTERS.append(arg)
if THEME:
    OUT = f"{OUT}/{THEME}"


def wanted(name):
    return not FILTERS or any(f in name for f in FILTERS)


DESKTOP_PAGES = [
    ("dashboard", "/"),
    ("dashboard-range-7", "/?range=7"),
    ("documents", "/documents"),
    ("documents-in-review", "/documents?status=in_review"),
    ("documents-archived", "/documents?status=archived"),
    ("documents-search", "/documents?q=contract"),
    ("document", f"/documents/{IDS['doc']}"),
    ("document-overdue", f"/documents/{IDS['doc2']}"),
    ("document-workflow", f"/documents/{IDS['doc3']}"),
    ("document-history", f"/documents/{IDS['doc']}/history"),
    ("calendar", "/calendar"),
    ("calendar-agenda", "/calendar?view=agenda&days=365"),
    ("calendar-feeds", "/calendar/feeds"),
    ("positions", "/positions"),
    ("position", f"/positions/{IDS['position']}"),
    ("reports", "/reports"),
    ("report-overdue", "/reports/overdue"),
    ("report-upcoming", "/reports/upcoming?days=90"),
    ("exports", "/exports"),
    ("exports-search", "/exports?search=1&q=contract"),
    ("notifications", "/notifications"),
    ("audit", "/audit"),
    ("account-tokens", "/account/tokens"),
    ("api-docs", "/api/docs"),
    ("admin-users", "/admin/users"),
    ("admin-user", f"/admin/users/{IDS['user']}"),
    ("admin-departments", "/admin/departments"),
    ("admin-department", f"/admin/departments/{IDS['dept']}"),
    ("admin-groups", "/admin/groups"),
    ("admin-group", f"/admin/groups/{IDS['group']}"),
    ("admin-roles", "/admin/roles"),
    ("admin-grants", "/admin/grants"),
    ("admin-idp", "/admin/idp"),
    ("admin-document-types", "/admin/document-types"),
    ("admin-document-type", f"/admin/document-types/{IDS['doctype']}"),
    ("admin-fields", "/admin/fields"),
    ("admin-field", f"/admin/fields/{IDS['field']}"),
    ("admin-tags", "/admin/tags"),
    ("admin-retention", "/admin/retention"),
    ("admin-deleted", "/admin/deleted"),
    ("admin-notification-rules", "/admin/notification-rules"),
    ("admin-branding", "/admin/branding"),
    ("admin-branding-reset", "/admin/branding/reset"),
    ("admin-notifications", "/admin/notifications"),
    ("admin-webhooks", "/admin/webhooks"),
    ("admin-api", "/admin/api"),
    # Sub-project 3 (D-087) removed the Outlook part; the OCR status has its own page.
    ("admin-ocr", "/admin/ocr"),
    # Added by plan P2 (Entra ID sync); a 404 here means P2 is not merged yet.
    ("admin-entra", "/admin/entra"),
    # Added by plan P4 (SharePoint operations).
    ("admin-sharepoint", "/admin/sharepoint"),
    # Added by S1 (settings in Administration).
    ("admin-microsoft-365", "/admin/microsoft-365"),
    ("admin-sign-in", "/admin/sign-in"),
    ("admin-email", "/admin/email"),
    ("admin-virus-scanning", "/admin/virus-scanning"),
    ("error-404", "/documents/00000000-0000-0000-0000-000000000000"),
    ("source", "/source"),
    ("license", "/license"),
]

# Pages the admin may not open: shot as the Operations department head.
EDITOR = "alice@example.org"
EDITOR_PAGES = [
    ("document-new", "/documents/new"),
    ("document-edit", f"/documents/{IDS['doc']}/edit"),
    ("editor-document", f"/documents/{IDS['doc']}"),
    ("editor-dashboard", "/"),
]

PHONE_PAGES = [
    ("dashboard", "/"),
    ("documents", "/documents"),
    ("document", f"/documents/{IDS['doc']}"),
    ("document-overdue", f"/documents/{IDS['doc2']}"),
    ("reports", "/reports"),
    # R2 key screens (spec §2, answer 7).
    ("positions", "/positions"),
    ("position", f"/positions/{IDS['position']}"),
    ("calendar", "/calendar"),
    ("calendar-agenda", "/calendar?view=agenda&days=365"),
    ("notifications", "/notifications"),
    ("account-tokens", "/account/tokens"),
]

PROBE = """() => {
  const vw = document.documentElement.clientWidth;
  const wide = [];
  for (const el of document.querySelectorAll('body *')) {
    const r = el.getBoundingClientRect();
    if (r.width === 0 || r.height === 0) continue;
    if (r.right > vw + 1) {
      // Ignore anything inside a scroll container that clips it.
      let p = el.parentElement, clipped = false;
      while (p && p !== document.body) {
        const s = getComputedStyle(p);
        if (['auto', 'scroll', 'hidden', 'clip'].includes(s.overflowX)) {
          const pr = p.getBoundingClientRect();
          if (pr.right <= vw + 1) { clipped = true; break; }
        }
        p = p.parentElement;
      }
      if (!clipped) {
        const id = el.tagName.toLowerCase() + (el.id ? '#' + el.id : '') +
          (el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\\s+/).join('.') : '');
        wide.push(id + ' right=' + Math.round(r.right));
      }
    }
  }
  return {
    scrollWidth: document.documentElement.scrollWidth,
    clientWidth: vw,
    h1: [...document.querySelectorAll('h1')].filter(h => h.getClientRects().length).map(h => h.textContent.trim()),
    wide: wide.slice(0, 12),
  };
}"""


def login(page, user=USER):
    page.goto(f"{BASE}/login")
    page.fill("#username", user)
    page.fill("#password", PASSWORD)
    page.click("form[action='/login/local'] button[type=submit]")
    page.wait_for_load_state("networkidle")
    if "/login" in page.url:
        raise SystemExit("login failed: " + page.url)
    if THEME:
        page.goto(f"{BASE}/account/tokens")
        page.check(f"input[type=radio][name='color_scheme'][value='{THEME}']")
        page.click("form[action='/account/appearance'].stack button[type=submit]")
        page.wait_for_load_state("networkidle")


def capture(page, name, url, results, errors, full=True):
    errors.clear()
    resp = page.goto(BASE + url)
    page.wait_for_load_state("networkidle")
    path = f"{OUT}/{name}.png"
    page.screenshot(path=path, full_page=full)
    if page.evaluate("() => document.documentElement.scrollHeight") > 3000:
        page.screenshot(path=f"{OUT}/{name}-top.png", full_page=False)
    probe = page.evaluate(PROBE)
    results[name] = {
        "url": url,
        "status": resp.status if resp else None,
        "overflow": probe["scrollWidth"] > probe["clientWidth"],
        **probe,
        "console": list(errors),
    }
    print(f"{name:32} {results[name]['status']} overflow={results[name]['overflow']} h1={len(probe['h1'])} console={len(errors)}")


def main():
    os.makedirs(OUT, exist_ok=True)
    results = {}
    with sync_playwright() as p:
        browser = p.chromium.launch()
        errors = []

        def ctx(width, height):
            c = browser.new_context(viewport={"width": width, "height": height}, device_scale_factor=1)
            pg = c.new_page()
            pg.on("console", lambda m: errors.append(f"{m.type}: {m.text}") if m.type in ("error", "warning") else None)
            pg.on("pageerror", lambda e: errors.append(f"pageerror: {e}"))
            return c, pg

        # Signed out: sign-in page at both widths.
        for label, (w, h) in {"desktop": (1440, 900), "phone": (390, 844)}.items():
            c, pg = ctx(w, h)
            if wanted(f"{label}-login"):
                capture(pg, f"{label}-login", "/login", results, errors)
            if wanted(f"{label}-login-notice"):
                # The Microsoft button with SSO off lands back here with a notice.
                errors.clear()
                pg.goto(BASE + "/login")
                pg.click("a.button.ms")
                pg.wait_for_load_state("networkidle")
                pg.screenshot(path=f"{OUT}/{label}-login-notice.png", full_page=True)
                results[f"{label}-login-notice"] = {"url": pg.url, "console": list(errors), **pg.evaluate(PROBE)}
                print(f"{label}-login-notice -> {pg.url}")
            c.close()

        # Desktop, signed in.
        c, pg = ctx(1440, 900)
        login(pg)
        for name, url in DESKTOP_PAGES:
            if wanted(f"desktop-{name}"):
                capture(pg, f"desktop-{name}", url, results, errors)

        if wanted("desktop-audit-verify"):
            errors.clear()
            pg.goto(BASE + "/audit")
            pg.click("form[action='/audit/verify'] button[type=submit]")
            pg.wait_for_load_state("networkidle")
            pg.screenshot(path=f"{OUT}/desktop-audit-verify.png", full_page=True)
            results["desktop-audit-verify"] = {"url": pg.url, "console": list(errors), **pg.evaluate(PROBE)}
            print("desktop-audit-verify", pg.url)

        if wanted("desktop-focus"):
            # Keyboard pass: Tab 10 times on the dashboard, screenshot each few.
            errors.clear()
            pg.goto(BASE + "/")
            pg.wait_for_load_state("networkidle")
            focused = []
            for i in range(1, 11):
                pg.keyboard.press("Tab")
                focused.append(pg.evaluate("() => { const e = document.activeElement; return e.tagName + ' ' + (e.getAttribute('href') || e.name || e.id || '') + ' | ' + (e.textContent || '').trim().slice(0, 40); }"))
                if i in (1, 3, 10):
                    pg.screenshot(path=f"{OUT}/desktop-focus-tab{i}.png")
            results["desktop-focus"] = {"focused": focused}
            print("focus order:", *focused, sep="\n  ")
        c.close()

        # Phone, signed in.
        c, pg = ctx(390, 844)
        login(pg)
        for name, url in PHONE_PAGES:
            if wanted(f"phone-{name}"):
                capture(pg, f"phone-{name}", url, results, errors)
        if wanted("phone-menu-open"):
            errors.clear()
            pg.goto(BASE + "/")
            pg.wait_for_load_state("networkidle")
            vis = pg.evaluate("() => ({nav: getComputedStyle(document.querySelector('.sidebar-nav')).display, toggle: getComputedStyle(document.querySelector('.menu-toggle')).display})")
            pg.click("details.menu-toggle > summary")
            pg.screenshot(path=f"{OUT}/phone-menu-open.png", full_page=False)
            results["phone-menu-open"] = {"display": vis, "open": pg.evaluate("() => document.querySelector('details.menu-toggle').open")}
            print("phone-menu-open", results["phone-menu-open"])
        c.close()

        # Editor pass (desktop): new/edit forms and the editor's document view.
        c, pg = ctx(1440, 900)
        login(pg, EDITOR)
        for name, url in EDITOR_PAGES:
            if wanted(f"desktop-{name}"):
                capture(pg, f"desktop-{name}", url, results, errors)
        c.close()

        # "Manage this document" opened, desktop and phone.
        for label, (w, h) in {"desktop": (1440, 900), "phone": (390, 844)}.items():
            if not wanted(f"{label}-document-manage"):
                continue
            c, pg = ctx(w, h)
            login(pg)
            errors.clear()
            pg.goto(BASE + f"/documents/{IDS['doc']}")
            pg.wait_for_load_state("networkidle")
            pg.click("#manage > summary")
            pg.locator("#manage").screenshot(path=f"{OUT}/{label}-document-manage.png")
            results[f"{label}-document-manage"] = {"console": list(errors), **pg.evaluate(PROBE)}
            r = results[f"{label}-document-manage"]
            print(f"{label}-document-manage overflow={r['scrollWidth'] > r['clientWidth']} wide={r['wide']}")
            c.close()

        # Sticky sidebar: scrolled to the bottom of a long page (viewport shot).
        if wanted("desktop-scrolled"):
            c, pg = ctx(1440, 900)
            login(pg)
            pg.goto(BASE + f"/documents/{IDS['doc']}")
            pg.wait_for_load_state("networkidle")
            pg.evaluate("() => window.scrollTo(0, document.documentElement.scrollHeight)")
            pg.wait_for_timeout(200)
            pg.screenshot(path=f"{OUT}/desktop-scrolled.png")
            c.close()

        # Overflow sweep, no screenshots unless something overflows.
        if wanted("sweep"):
            for w in (1024, 768, 390):
                c, pg = ctx(w, 900)
                login(pg)
                for name, url in DESKTOP_PAGES:
                    errors.clear()
                    pg.goto(BASE + url)
                    pg.wait_for_load_state("networkidle")
                    pr = pg.evaluate(PROBE)
                    if pr["scrollWidth"] > pr["clientWidth"] or pr["wide"]:
                        pg.screenshot(path=f"{OUT}/sweep-{w}-{name}.png", full_page=True)
                        print(f"sweep {w} {name}: scrollWidth={pr['scrollWidth']} wide={pr['wide']}")
                    results[f"sweep-{w}-{name}"] = pr
                c.close()
                c, pg = ctx(w, 900)
                login(pg, EDITOR)
                for name, url in EDITOR_PAGES:
                    pg.goto(BASE + url)
                    pg.wait_for_load_state("networkidle")
                    pr = pg.evaluate(PROBE)
                    if pr["scrollWidth"] > pr["clientWidth"] or pr["wide"]:
                        pg.screenshot(path=f"{OUT}/sweep-{w}-{name}.png", full_page=True)
                        print(f"sweep {w} {name}: scrollWidth={pr['scrollWidth']} wide={pr['wide']}")
                c.close()
            print("sweep done")

        # Breakpoint check at desktop.
        c, pg = ctx(1440, 900)
        login(pg)
        pg.goto(BASE + "/")
        results["desktop-breakpoint"] = pg.evaluate("() => ({nav: getComputedStyle(document.querySelector('.sidebar-nav')).display, toggle: getComputedStyle(document.querySelector('.menu-toggle')).display})")
        print("desktop breakpoint", results["desktop-breakpoint"])
        c.close()
        browser.close()

    existing = {}
    try:
        existing = json.load(open(f"{OUT}/report.json"))
    except (OSError, ValueError):
        pass
    existing.update(results)
    json.dump(existing, open(f"{OUT}/report.json", "w"), indent=2)


if __name__ == "__main__":
    main()
