// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Which stored files the browser may show (D-118).

/// The stored (sniffed) media types the browser may show inline.
pub const VIEWABLE_TYPES: [&str; 5] = [
    "application/pdf",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
];

/// Whether a stored media type may be shown inline. An exact match only.
pub fn viewable(media_type: &str) -> bool {
    VIEWABLE_TYPES.contains(&media_type)
}

/// How a file is sent to the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Attachment,
    Inline,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Attachment => "attachment",
            Disposition::Inline => "inline",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_listed_types_are_viewable() {
        for t in [
            "application/pdf",
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
        ] {
            assert!(viewable(t), "{t}");
        }
        for t in [
            "",
            "text/plain",
            "text/html",
            "image/svg+xml",
            "image/tiff",
            "application/msword",
            "application/pdf; charset=utf-8",
            "APPLICATION/PDF",
        ] {
            assert!(!viewable(t), "{t}");
        }
    }
}
