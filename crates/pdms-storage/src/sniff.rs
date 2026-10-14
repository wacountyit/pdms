// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Decide a file's type from its content, never from the client's claim, and
//! accept only document and image formats.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub media_type: &'static str,
    pub label: &'static str,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SniffError {
    #[error("the file is empty")]
    Empty,
    #[error("files of type {0} are not accepted")]
    NotAllowed(String),
    #[error("unrecognized binary file; upload PDF, Office, OpenDocument, image, or text files")]
    Unrecognized,
}

/// (media type, label) of every accepted type.
pub const ALLOWED: &[(&str, &str)] = &[
    ("application/pdf", "PDF"),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "Word document",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "Excel workbook",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "PowerPoint presentation",
    ),
    ("application/msword", "Word 97–2003 document"),
    ("application/vnd.ms-excel", "Excel 97–2003 workbook"),
    (
        "application/vnd.ms-powerpoint",
        "PowerPoint 97–2003 presentation",
    ),
    ("application/vnd.ms-outlook", "Outlook message"),
    (
        "application/vnd.oasis.opendocument.text",
        "OpenDocument text",
    ),
    (
        "application/vnd.oasis.opendocument.spreadsheet",
        "OpenDocument spreadsheet",
    ),
    (
        "application/vnd.oasis.opendocument.presentation",
        "OpenDocument presentation",
    ),
    ("application/rtf", "Rich text"),
    ("image/png", "PNG image"),
    ("image/jpeg", "JPEG image"),
    ("image/gif", "GIF image"),
    ("image/tiff", "TIFF image"),
    ("image/webp", "WebP image"),
    ("text/plain", "Text"),
    ("text/csv", "CSV"),
    ("message/rfc822", "Email message"),
];

fn allowed(mime: &str) -> Option<Detected> {
    ALLOWED
        .iter()
        .find(|(m, _)| *m == mime)
        .map(|(media_type, label)| Detected { media_type, label })
}

fn extension(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Detect from the first bytes (see [`crate::spool::HEAD_BYTES`]) and the file name.
pub fn detect(head: &[u8], file_name: &str) -> Result<Detected, SniffError> {
    if head.is_empty() {
        return Err(SniffError::Empty);
    }
    let ext = extension(file_name);
    if let Some(kind) = infer::get(head) {
        let mime = kind.mime_type();
        if let Some(d) = allowed(mime) {
            return Ok(d);
        }
        let by_ext = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .find(|(e, _)| *e == ext)
                .and_then(|(_, m)| allowed(m))
        };
        let fallback = match mime {
            // Compound File Binary: legacy Office and Outlook .msg share the container.
            "application/x-ole-storage" => by_ext(&[
                ("doc", "application/msword"),
                ("xls", "application/vnd.ms-excel"),
                ("ppt", "application/vnd.ms-powerpoint"),
                ("msg", "application/vnd.ms-outlook"),
            ]),
            // OOXML/ODF whose zip layout the sniffer could not classify.
            "application/zip" => by_ext(&[
                (
                    "docx",
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                ),
                (
                    "xlsx",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                ),
                (
                    "pptx",
                    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                ),
                ("odt", "application/vnd.oasis.opendocument.text"),
                ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
                ("odp", "application/vnd.oasis.opendocument.presentation"),
            ]),
            _ => None,
        };
        return fallback.ok_or_else(|| SniffError::NotAllowed(mime.to_owned()));
    }
    if looks_like_text(head) {
        let mime = match ext.as_str() {
            "csv" => "text/csv",
            "eml" => "message/rfc822",
            _ => "text/plain",
        };
        return allowed(mime).ok_or(SniffError::Unrecognized);
    }
    Err(SniffError::Unrecognized)
}

/// UTF-8 (a multi-byte character may be cut off at the end) with no NUL and
/// few control characters.
fn looks_like_text(head: &[u8]) -> bool {
    let text = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(e) if e.error_len().is_none() => match std::str::from_utf8(&head[..e.valid_up_to()]) {
            Ok(s) => s,
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    let controls = text
        .chars()
        .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
        .count();
    !text.contains('\0') && controls * 100 <= text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_documents_by_content() {
        assert_eq!(
            detect(b"%PDF-1.7\n...", "x.bin").unwrap().media_type,
            "application/pdf"
        );
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
        assert_eq!(
            detect(&png, "photo.pdf").unwrap().media_type,
            "image/png",
            "content wins over name"
        );
        assert_eq!(
            detect(b"name,amount\nA,1\n", "data.csv")
                .unwrap()
                .media_type,
            "text/csv"
        );
        assert_eq!(
            detect("Résumé notes\n".as_bytes(), "notes.txt")
                .unwrap()
                .media_type,
            "text/plain"
        );
    }

    #[test]
    fn rejects_executables_and_unknown_binaries() {
        let exe = [b'M', b'Z', 0x90, 0, 3, 0, 0, 0, 4, 0, 0, 0, 0xFF, 0xFF];
        assert!(matches!(
            detect(&exe, "contract.pdf"),
            Err(SniffError::NotAllowed(_))
        ));
        let elf = [0x7F, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0];
        assert!(detect(&elf, "a.txt").is_err());
        assert_eq!(
            detect(&[0, 1, 2, 3, 0xFE, 0xFF], "a.dat"),
            Err(SniffError::Unrecognized)
        );
        assert_eq!(detect(b"", "a.pdf"), Err(SniffError::Empty));
    }

    #[test]
    fn ole_and_zip_containers_need_a_matching_extension() {
        let ole = [
            0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(
            detect(&ole, "mail.msg").unwrap().media_type,
            "application/vnd.ms-outlook"
        );
        assert!(detect(&ole, "thing.exe").is_err());
        let zip = [b'P', b'K', 3, 4, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0];
        assert!(detect(&zip, "letter.docx").is_ok());
        assert!(matches!(
            detect(&zip, "archive.zip"),
            Err(SniffError::NotAllowed(_))
        ));
    }
}
