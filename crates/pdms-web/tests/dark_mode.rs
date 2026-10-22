// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
//! Dark mode (R2 spec §7): the token blocks' contrast, the per-person switch,
//! and its audit trail.

mod support;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use pdms_core::branding::color::{
    DARK_GROUND, DARK_INK, DARK_SURFACE, MIN_MARK_CONTRAST, MIN_TEXT_CONTRAST, contrast, parse_hex,
};
use support::*;

const CSS: &str = include_str!("../static/app.css");

/// The custom properties set inside the block that starts at `selector`.
fn tokens(selector: &str) -> BTreeMap<String, String> {
    let start = CSS
        .find(selector)
        .unwrap_or_else(|| panic!("no {selector}"));
    let open = CSS[start..].find('{').unwrap() + start;
    let close = CSS[open..].find('}').unwrap() + open;
    CSS[open + 1..close]
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let (k, v) = l.strip_prefix("--")?.split_once(':')?;
            let v = v.split("/*").next().unwrap().trim().trim_end_matches(';');
            Some((k.trim().to_owned(), v.to_owned()))
        })
        .collect()
}

/// Every text pair at 4.5:1 and every mark at 3:1 (R1 spec §4, R2 spec §7).
fn check(name: &str, t: &BTreeMap<String, String>) {
    let c = |k: &str| {
        parse_hex(t.get(k).unwrap_or_else(|| panic!("{name}: no --{k}")))
            .unwrap_or_else(|| panic!("{name}: --{k} is not a hex color"))
    };
    let text = [
        ("ink", "ground"),
        ("ink", "surface"),
        ("ink", "thead"),
        ("muted", "ground"),
        ("muted", "surface"),
        ("muted", "thead"),
        ("crit-fg", "crit-tint"),
        ("crit-fg", "surface"),
        ("warn-fg", "warn-tint"),
        ("warn-fg", "surface"),
        ("info-fg", "info-tint"),
        ("info-fg", "surface"),
        ("good-fg", "good-tint"),
        ("good-fg", "surface"),
        ("neutral-fg", "neutral-tint"),
        ("neutral-fg", "surface"),
        ("ink", "neutral-tint"),
    ];
    for (fg, bg) in text {
        let r = contrast(c(fg), c(bg));
        assert!(
            r >= MIN_TEXT_CONTRAST,
            "{name}: --{fg} on --{bg} is {r:.2}:1"
        );
    }
    let marks = [
        ("crit-dot", "surface"),
        ("warn-dot", "surface"),
        ("info-dot", "surface"),
        ("good-dot", "surface"),
        ("neutral-dot", "surface"),
        ("control-border", "surface"),
        ("focus", "surface"),
        ("focus", "ground"),
    ];
    for (fg, bg) in marks {
        let r = contrast(c(fg), c(bg));
        assert!(
            r >= MIN_MARK_CONTRAST,
            "{name}: --{fg} on --{bg} is {r:.2}:1"
        );
    }
}

#[test]
fn both_palettes_meet_contrast_and_the_dark_blocks_match() {
    let light = tokens(":root {");
    check("light", &light);
    let dark = tokens(r#":root[data-theme="dark"] {"#);
    let system = tokens(r#":root[data-theme="system"] {"#);
    assert_eq!(dark, system, "the dark and system blocks must be identical");
    // The dark block overrides colors only; anything it lacks comes from :root.
    let mut merged = light.clone();
    merged.extend(dark.clone());
    check("dark", &merged);
    // The accent derivations are checked against these same colors.
    for (k, v) in [
        ("ground", DARK_GROUND),
        ("surface", DARK_SURFACE),
        ("ink", DARK_INK),
    ] {
        assert_eq!(
            parse_hex(&dark[k]),
            Some(v),
            "--{k} and branding::color disagree"
        );
    }
}

#[tokio::test]
async fn each_person_switches_their_own_scheme() {
    let app = app().await;
    let (_, secret) = user(&app.core, "ada@example.org", None, None).await;
    let mut b = Browser::new(&app);
    b.login("ada@example.org", &secret).await;

    // Dark is the default, including before sign-in.
    let home = b.get("/").await;
    assert!(home.body.contains(r#"<html lang="en" data-theme="dark">"#));
    assert!(
        home.body.contains("Light mode</button>"),
        "the sidebar switch offers light"
    );
    let mut anon = Browser::new(&app);
    assert!(
        anon.get("/login")
            .await
            .body
            .contains(r#"data-theme="dark""#)
    );

    // The sidebar switch returns to the page it was on.
    let r = b
        .post(
            "/account/appearance",
            &[
                ("_csrf", home.csrf().as_str()),
                ("color_scheme", "light"),
                ("back", "/calendar?view=agenda"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location(), "/calendar?view=agenda");
    let page = b.get("/documents").await;
    assert!(page.body.contains(r#"data-theme="light""#));
    assert!(page.body.contains("Dark mode</button>"));
    assert!(
        actions(&app.core)
            .await
            .contains(&"user.color_scheme_set".to_owned())
    );

    // "Match my device", from the account page.
    let account = b.get("/account/tokens").await;
    assert!(
        account
            .body
            .contains(r#"<h2 id="appearance-h">Appearance</h2>"#)
    );
    assert!(account.body.contains(r#"value="light" checked"#));
    b.post(
        "/account/appearance",
        &[
            ("_csrf", account.csrf().as_str()),
            ("color_scheme", "system"),
        ],
    )
    .await;
    assert!(b.get("/").await.body.contains(r#"data-theme="system""#));

    // Only a local page is followed back; an unknown scheme is refused.
    let r = b
        .post(
            "/account/appearance",
            &[
                ("_csrf", account.csrf().as_str()),
                ("color_scheme", "dark"),
                ("back", "https://elsewhere.example/"),
            ],
        )
        .await;
    assert_eq!(r.location(), "/");
    let r = b
        .post(
            "/account/appearance",
            &[("_csrf", account.csrf().as_str()), ("color_scheme", "neon")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(
        b.get("/").await.body.contains(r#"data-theme="dark""#),
        "unchanged"
    );
    // Without a token the switch does nothing.
    assert_eq!(
        b.post("/account/appearance", &[("color_scheme", "light")])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn theme_css_carries_the_dark_accent_shades() {
    let app = app().await;
    let mut anon = Browser::new(&app);
    let css = anon.get("/theme.css").await.body;
    assert!(css.contains("--accent-mark:"));
    assert!(css.contains(r#":root[data-theme="dark"]"#), "{css}");
    assert!(css.contains(r#":root[data-theme="system"]"#), "{css}");
    assert!(css.contains("@media screen"), "print stays light: {css}");
}
