/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

const POLY: u32 = 0xEDB8_8320;

const TABLES: [[u32; 256]; 4] = {
    let mut t = [[0u32; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut n = 1;
        while n < 4 {
            let prev = t[n - 1][i];
            t[n][i] = (prev >> 8) ^ t[0][(prev & 0xFF) as usize];
            n += 1;
        }
        i += 1;
    }
    t
};

/// CRC-32 (IEEE 802.3, reflected), fed in pieces.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Crc32(u32);

impl Crc32 {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut c = !self.0;
        let mut words = data.chunks_exact(4);
        for w in &mut words {
            c ^= u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
            c = TABLES[3][(c & 0xFF) as usize]
                ^ TABLES[2][((c >> 8) & 0xFF) as usize]
                ^ TABLES[1][((c >> 16) & 0xFF) as usize]
                ^ TABLES[0][(c >> 24) as usize];
        }
        for &b in words.remainder() {
            c = (c >> 8) ^ TABLES[0][((c ^ u32::from(b)) & 0xFF) as usize];
        }
        self.0 = !c;
    }

    pub const fn value(&self) -> u32 {
        self.0
    }

    pub fn checksum(data: &[u8]) -> u32 {
        let mut crc = Self::new();
        crc.update(data);
        crc.0
    }
}

#[cfg(test)]
mod tests {
    use super::Crc32;

    #[test]
    fn matches_the_check_value() {
        assert_eq!(Crc32::checksum(b"123456789"), 0xCBF4_3926);
        assert_eq!(Crc32::checksum(b""), 0);
        assert_eq!(Crc32::checksum(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn pieces_give_the_whole() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + i / 13) as u8).collect();
        let whole = Crc32::checksum(&data);
        for step in [1, 3, 4, 5, 64, 999] {
            let mut crc = Crc32::new();
            for piece in data.chunks(step) {
                crc.update(piece);
            }
            assert_eq!(crc.value(), whole, "pieces of {step}");
        }
    }
}
