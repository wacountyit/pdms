// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors
//! Microsoft's QuickXorHash: the content hash SharePoint and OneDrive report
//! for files (`file.hashes.quickXorHash`). Reference algorithm:
//! <https://learn.microsoft.com/onedrive/developer/code-snippets/quickxorhash>

use base64::Engine as _;

const WIDTH: u64 = 160;
const SHIFT: u64 = 11;

/// Incremental QuickXorHash. Feed bytes with [`update`](Self::update) in any
/// chunking; [`finish_base64`](Self::finish_base64) returns the value Graph reports.
#[derive(Clone, Debug, Default)]
pub struct QuickXorHash {
    /// The 160-bit register: bits 0–63, 64–127, and 128–159 (low 32 bits of the last cell).
    cells: [u64; 3],
    length: u64,
}

impl QuickXorHash {
    pub fn new() -> Self {
        Self::default()
    }

    /// Byte `k` of the input is XORed into the register at bit `(k * 11) mod 160`.
    /// Bytes 160 apart land on the same bit, so they are combined first.
    pub fn update(&mut self, data: &[u8]) {
        let start = (self.length % WIDTH) * SHIFT % WIDTH;
        for i in 0..data.len().min(WIDTH as usize) {
            let x = data
                .iter()
                .skip(i)
                .step_by(WIDTH as usize)
                .fold(0u8, |a, b| a ^ b);
            self.place(x, (start + SHIFT * i as u64) % WIDTH);
        }
        self.length += data.len() as u64;
    }

    fn place(&mut self, x: u8, at: u64) {
        for bit in 0..8u64 {
            if (x >> bit) & 1 == 1 {
                let p = (at + bit) % WIDTH;
                if let Some(cell) = self.cells.get_mut((p / 64) as usize) {
                    *cell ^= 1u64 << (p % 64);
                }
            }
        }
    }

    /// The 20-byte digest: the register in little-endian order, with the
    /// input length (little-endian u64) XORed into the last 8 bytes.
    pub fn finish(&self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[..8].copy_from_slice(&self.cells[0].to_le_bytes());
        out[8..16].copy_from_slice(&self.cells[1].to_le_bytes());
        out[16..].copy_from_slice(&self.cells[2].to_le_bytes()[..4]);
        for (o, l) in out[12..].iter_mut().zip(self.length.to_le_bytes()) {
            *o ^= l;
        }
        out
    }

    pub fn finish_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.finish())
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::QuickXorHash;

    /// (input, expected), both base64.
    const VECTORS: &[(&str, &str)] = &[
        ("", "AAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        ("Sg==", "SgAAAAAAAAAAAAAAAQAAAAAAAAA="),
        ("tbQ=", "taAFAAAAAAAAAAAAAgAAAAAAAAA="),
        ("0pZP", "0rDEEwAAAAAAAAAAAwAAAAAAAAA="),
        ("/aNzzCFPS/A=", "/RjFHJgRgicsAR4ACAAAAAAAAAA="),
        (
            "T6LYJIfDh81JrAK309H2JMJTXis=",
            "zBTHrspn3mEcohlJdIUAbjGNaNg=",
        ),
        (
            "DWAAX5/CIfrmErgZa8ot6ZraeSbu",
            "LR2Z0PjuRYGKQB/mhQAuMrAGZbQ=",
        ),
        (
            "Mb7EGva2rEE5fENDL85P+BsapHEEjv2/siVhKjvAQe02feExVOQSkfmuYzU/kTF1MaKjPmKF/w+cbvwfdWL8aQ==",
            "n1anP5NfvD4XDYWIeRPW3ZkPv1Y=",
        ),
        (
            "PfxMcUd0vIW6VbHG/uj/Y0W6qEoKmyBD0nYebEKazKaKG+UaDqBEcmQjbfQeVnVLuodMoPp7P7TR1htX5n2VnkHh22xDyoJ8C/ZQKiSNqQfXvh83judf4RVr9exJCud8Uvgip6aVZTaPrJHVjQhMCp/dEnGvqg0oN5OVkM2qqAXvA0teKUDhgNM71sDBVBCGXxNOR2bpbD1iM4dnuT0ey4L+loXEHTL0fqMeUcEi2asgImnlNakwenDzz0x57aBwyq3AspCFGB1ncX4yYCr/OaCcS5OKi/00WH+wNQU3",
            "QX/YEpG0gDsmhEpCdWhsxDzsfVE=",
        ),
        (
            "w0LGhqU1WXFbdavqDE4kAjEzWLGGzmTNikzqnsiXHx2KRReKVTxkv27u3UcEz9+lbMvYl4xFf2Z4aE1xRBBNd1Ke5C0zToSaYw5o4B/7X99nKK2/XaUX1byLow2aju2XJl2OpKpJg+tSJ2fmjIJTkfuYUz574dFX6/VXxSxwGH/xQEAKS5TCsBK3CwnuG1p5SAsQq3gGVozDWyjEBcWDMdy8/AIFrj/y03Lfc/RNRCQTAfZbnf2QwV7sluw4fH3XJr07UoD0YqN+7XZzidtrwqMY26fpLZnyZjnBEt1FAZWO7RnKG5asg8xRk9YaDdedXdQSJAOy6bWEWlABj+tVAigBxavaluUH8LOj+yfCFldJjNLdi90fVHkUD/m4Mr5OtmupNMXPwuG3EQlqWUVpQoYpUYKLsk7a5Mvg6UFkiH596y5IbJEVCI1Kb3D1",
            "e3+wo77iKcILiZegnzyUNcjCdoQ=",
        ),
    ];

    fn decode(s: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }

    #[test]
    fn matches_published_vectors() {
        for (input, expected) in VECTORS {
            let mut h = QuickXorHash::new();
            h.update(&decode(input));
            assert_eq!(&h.finish_base64(), expected, "input {input}");
        }
    }

    #[test]
    fn chunking_does_not_change_the_hash() {
        for (input, expected) in VECTORS {
            let data = decode(input);
            for size in [1, 7, 160, 161, 1000] {
                let mut h = QuickXorHash::new();
                for chunk in data.chunks(size) {
                    h.update(chunk);
                }
                assert_eq!(
                    &h.finish_base64(),
                    expected,
                    "input {input}, chunks of {size}"
                );
            }
        }
    }

    #[test]
    fn length_is_folded_into_the_last_eight_bytes() {
        let mut h = QuickXorHash::new();
        h.update(&[0u8; 3]);
        let out = h.finish();
        assert_eq!(out[12], 3);
        assert!(out[..12].iter().all(|b| *b == 0));
    }
}
