// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Seal upload checks: real type from the first bytes, dimensions from the header.

pub const MAX_BYTES: usize = 512 * 1024;
pub const MIN_SIDE: u32 = 16;
pub const MAX_SIDE: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    NotPngOrJpeg,
    TooLarge,
    BadDimensions,
    Unreadable,
}

impl SealError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotPngOrJpeg => "The seal must be a PNG or JPEG image.",
            Self::TooLarge => "The seal must be 512 KB or smaller.",
            Self::BadDimensions => "The seal must be between 16 and 2048 pixels on each side.",
            Self::Unreadable => "The seal image could not be read. Save it again as PNG or JPEG.",
        }
    }
}

const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from(*b.get(i)?) << 8 | u32::from(*b.get(i + 1)?))
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(be16(b, i)? << 16 | be16(b, i + 2)?)
}

fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    (b.get(12..16)? == b"IHDR").then_some(())?;
    Some((be32(b, 16)?, be32(b, 20)?))
}

fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 3 < b.len() {
        if b[i] != 0xFF {
            return None;
        }
        while b.get(i + 1) == Some(&0xFF) {
            i += 1;
        }
        let marker = *b.get(i + 1)?;
        if marker == 0x01 || (0xD0..=0xD8).contains(&marker) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
        if is_sof {
            return Some((be16(b, i + 7)?, be16(b, i + 5)?));
        }
        if marker == 0xD9 || len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Returns `(content_type, width, height)` or why the file is refused.
pub fn sniff(bytes: &[u8]) -> Result<(&'static str, u32, u32), SealError> {
    if bytes.len() > MAX_BYTES {
        return Err(SealError::TooLarge);
    }
    let (ctype, size) = if bytes.starts_with(&PNG_SIG) {
        ("image/png", png_size(bytes))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("image/jpeg", jpeg_size(bytes))
    } else {
        return Err(SealError::NotPngOrJpeg);
    };
    let (w, h) = size.ok_or(SealError::Unreadable)?;
    if !(MIN_SIDE..=MAX_SIDE).contains(&w) || !(MIN_SIDE..=MAX_SIDE).contains(&h) {
        return Err(SealError::BadDimensions);
    }
    Ok((ctype, w, h))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Minimal PNG header bytes: signature + IHDR chunk with the given size.
    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = PNG_SIG.to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    /// Minimal JPEG: SOI, an APP0 segment, then SOF0 with the given size.
    pub(crate) fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn accepts_png_and_jpeg_with_sizes() {
        assert_eq!(sniff(&png(200, 180)), Ok(("image/png", 200, 180)));
        assert_eq!(sniff(&jpeg(300, 120)), Ok(("image/jpeg", 300, 120)));
    }

    #[test]
    fn sniff_refuses_svg_and_renamed_text() {
        assert_eq!(
            sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
            Err(SealError::NotPngOrJpeg)
        );
        assert_eq!(
            sniff(b"<html><script>alert(1)</script>"),
            Err(SealError::NotPngOrJpeg)
        );
        assert_eq!(
            sniff(b"GIF89a\x10\x00\x10\x00"),
            Err(SealError::NotPngOrJpeg)
        );
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Err(SealError::NotPngOrJpeg));
    }

    #[test]
    fn refuses_bad_dimensions_size_and_truncated_headers() {
        assert_eq!(sniff(&png(8, 200)), Err(SealError::BadDimensions));
        assert_eq!(sniff(&png(4096, 200)), Err(SealError::BadDimensions));
        assert_eq!(sniff(&png(200, 200)[..14]), Err(SealError::Unreadable));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xD9]), Err(SealError::Unreadable));
        let mut big = png(100, 100);
        big.resize(MAX_BYTES + 1, 0);
        assert_eq!(sniff(&big), Err(SealError::TooLarge));
    }
}
