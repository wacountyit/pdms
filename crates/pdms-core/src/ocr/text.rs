// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Built-in (Rust) text extraction: embedded PDF text, Word `.docx`, and
//! plain text. Scanned documents need OCR, which runs in a sidecar.

use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

pub const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// What the built-in extractors can read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Docx,
    Plain,
    /// Only OCR can read it.
    Image,
    Unsupported,
}

pub fn kind(media_type: &str) -> Kind {
    match media_type {
        "application/pdf" => Kind::Pdf,
        DOCX => Kind::Docx,
        "text/plain" | "text/csv" | "text/markdown" => Kind::Plain,
        "image/png" | "image/jpeg" | "image/tiff" => Kind::Image,
        _ => Kind::Unsupported,
    }
}

/// Embedded text of a PDF (empty for scanned PDFs). Runs on a blocking
/// thread; a malformed file is an error, never a crash.
pub async fn pdf(path: &Path) -> Result<String, String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        pdf_extract::extract_text(&path).map_err(|e| format!("PDF text: {e}"))
    })
    .await
    .map_err(|e| format!("PDF text extraction stopped: {e}"))?
}

#[allow(clippy::expect_used)]
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").expect("static regex"));

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The text of a Word document's body: paragraphs become lines.
pub fn docx_xml_to_text(xml: &str) -> String {
    let marked = xml
        .replace("</w:p>", "\n")
        .replace("<w:tab/>", "\t")
        .replace("<w:br/>", "\n");
    xml_unescape(&TAG.replace_all(&marked, ""))
}

pub async fn docx(path: &Path) -> Result<String, String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| format!("not a Word document: {e}"))?;
        let mut entry = z
            .by_name("word/document.xml")
            .map_err(|_| "not a Word document (no word/document.xml)".to_owned())?;
        let mut xml = String::new();
        entry
            .by_ref()
            .take(64 * 1024 * 1024)
            .read_to_string(&mut xml)
            .map_err(|e| e.to_string())?;
        Ok(docx_xml_to_text(&xml))
    })
    .await
    .map_err(|e| format!("Word text extraction stopped: {e}"))?
}

pub async fn plain(path: &Path) -> Result<String, String> {
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Characters that carry content (for deciding whether a PDF is scanned).
pub fn meaningful_chars(s: &str) -> usize {
    s.chars().filter(|c| c.is_alphanumeric()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_xml_becomes_lines() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Effective</w:t></w:r><w:r><w:t xml:space="preserve"> January 1, 2027</w:t></w:r></w:p><w:p><w:r><w:t>Smith &amp; Sons</w:t><w:tab/><w:t>&lt;LLC&gt;</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(
            docx_xml_to_text(xml),
            "Effective January 1, 2027\nSmith & Sons\t<LLC>\n"
        );
    }

    #[test]
    fn kinds() {
        assert_eq!(kind("application/pdf"), Kind::Pdf);
        assert_eq!(kind(DOCX), Kind::Docx);
        assert_eq!(kind("image/tiff"), Kind::Image);
        assert_eq!(kind("application/zip"), Kind::Unsupported);
        assert_eq!(meaningful_chars("  12 ab\n\t-"), 4);
    }
}
