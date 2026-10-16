// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
#![allow(clippy::unwrap_used)]

//! Templates and inline HTML must stay compatible with the strict CSP:
//! no style attributes or `<style>` blocks, no inline scripts, no
//! event-handler attributes and no `javascript:` URLs.

use std::path::{Path, PathBuf};

use regex::Regex;

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            files(&p, ext, out);
        } else if p.extension().is_some_and(|e| e == ext) {
            out.push(p);
        }
    }
}

fn offences(text: &str) -> Vec<(usize, String)> {
    let style_attr = Regex::new(r#"(?:^|[\s"'])style\s*="#).unwrap();
    let style_block = Regex::new(r"<style(?:[\s>]|$)").unwrap();
    let script_tag = Regex::new(r"<script\b([^>]*)").unwrap();
    let handler = Regex::new(r#"\son[a-z]+\s*="#).unwrap();
    let js_url = Regex::new(r#"=\s*["']?\s*javascript:"#).unwrap();
    let mut found = Vec::new();
    for (i, line) in text.lines().enumerate() {
        // Rust sources hold inline HTML in string literals with escaped quotes.
        let lower = line
            .to_ascii_lowercase()
            .replace("\\\"", "\"")
            .replace("\\'", "'");
        if style_attr.is_match(&lower) {
            found.push((i + 1, "style attribute".into()));
        }
        if style_block.is_match(&lower) {
            found.push((i + 1, "style element".into()));
        }
        for tag in script_tag.captures_iter(&lower) {
            let attrs = &tag[1];
            if !attrs.contains("src=") && !attrs.contains("type=\"application/json\"") {
                found.push((i + 1, "inline script".into()));
            }
        }
        if handler.is_match(&lower) {
            found.push((i + 1, "event handler attribute".into()));
        }
        if js_url.is_match(&lower) {
            found.push((i + 1, "javascript: URL".into()));
        }
    }
    found
}

/// Every template, plus every Rust source that serves an inline HTML page.
fn scanned_paths() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    files(&root.join("templates"), "html", &mut paths);
    let mut sources = Vec::new();
    files(&root.join("src"), "rs", &mut sources);
    for p in sources {
        let text = std::fs::read_to_string(&p).unwrap().to_ascii_lowercase();
        if text.contains("<!doctype html") {
            paths.push(p);
        }
    }
    paths.sort();
    paths
}

#[test]
fn templates_and_inline_html_are_csp_clean() {
    let paths = scanned_paths();
    // A guard that silently scans nothing would always pass.
    for must in ["templates/base.html", "src/routes/ops.rs"] {
        assert!(
            paths.iter().any(|p| p.ends_with(must)),
            "{must} was not scanned"
        );
    }
    let mut report = Vec::new();
    for p in &paths {
        let text = std::fs::read_to_string(p).unwrap();
        for (line, what) in offences(&text) {
            report.push(format!("{}:{line}: {what}", p.display()));
        }
    }
    assert!(
        report.is_empty(),
        "CSP-incompatible markup:\n{}",
        report.join("\n")
    );
}

#[test]
fn guard_detects_each_offence() {
    assert_eq!(offences(r#"<p style="color:red">"#).len(), 1);
    assert_eq!(offences("<script>alert(1)</script>").len(), 1);
    assert_eq!(offences(r##"<a href="#" onclick="x()">"##).len(), 1);
    assert!(offences(r#"<script src="/static/htmx.min.js" defer></script>"#).is_empty());
    assert!(offences(r#"<button hx-on="x">"#).is_empty());
    // Inline HTML in a Rust string literal, with escaped quotes.
    assert_eq!(offences(r#""<p style=\"color:red\">""#).len(), 1);
    assert_eq!(offences(r#""<p STYLE = 'color:red'>""#).len(), 1);
    assert_eq!(offences("<style>p { color: red }</style>").len(), 1);
    assert_eq!(offences(r#"<a href="javascript:x()">"#).len(), 1);
    assert_eq!(
        offences(r#"<script src="/static/htmx.min.js"></script><script>x()</script>"#).len(),
        1
    );
    assert!(offences(r#"<script type="application/json">{"a": 1}</script>"#).is_empty());
    assert!(offences(r#"<p data-style="x" class="muted">"#).is_empty());
    assert!(offences(r#"<link rel="stylesheet" href="/static/app.css">"#).is_empty());
}
