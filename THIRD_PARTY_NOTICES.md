# Third-party notices

PDMS is licensed under the GNU Affero General Public License, version 3 or
later (see [LICENSE](LICENSE)). The web interface bundles the third-party
assets below into the binary; each keeps its own license. Rust crate
dependencies are listed in `Cargo.lock` and keep their own licenses too (see
[README.md](README.md)).

## Source Sans 3 (font)

- **Copyright:** © 2010–2021 Adobe (http://www.adobe.com/), with Reserved
  Font Name "Source".
- **License:** SIL Open Font License, version 1.1. The full text is in
  [`crates/pdms-web/static/fonts/OFL.txt`](crates/pdms-web/static/fonts/OFL.txt).
- **Files:** `crates/pdms-web/static/fonts/source-sans-3-latin-400.woff2`,
  `…-600.woff2` and `…-700.woff2`, served at `/static/fonts/`.
- **Obtained from:** the `@fontsource/source-sans-3` package, version 5.0.12.
  The Latin-subset WOFF2 files are unmodified.

## Lucide icons

- **Copyright:** portions © Cole Bemis 2013–2022, as part of Feather (MIT);
  all other © Lucide Contributors 2022.
- **License:** ISC. The full text is in
  [`crates/pdms-web/templates/icons/LICENSE-lucide.txt`](crates/pdms-web/templates/icons/LICENSE-lucide.txt).
- **Version:** `lucide-static` 0.460.0.
- **Files:** `crates/pdms-web/templates/icons/*.html`, one template partial per
  icon. Each holds the icon's SVG elements unchanged, inside an `<svg>`
  wrapper with the app's own size and accessibility attributes.

## htmx

- **Version:** 2.0.4, in `crates/pdms-web/static/htmx.min.js`, served at
  `/static/htmx.min.js`.
- **License:** BSD Zero Clause License (SPDX `0BSD`), as published upstream.
  The minified file carries no license header, and 0BSD requires no notice
  to be kept.
