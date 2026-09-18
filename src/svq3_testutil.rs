//! Test-only bit packer for the SVQ3 modules: builds slice payloads
//! from fixed-width fields and universal-code numbers in the
//! `docs/video/svq3/spec/07-slice-and-macroblock-layer.md` §1 layout
//! (`n = 0 → 1`, `n ≥ 1 → 0 d₁ 0 d₂ … 0 dₙ 1`).

/// MSB-first bit accumulator.
pub(crate) struct Packer {
    bits: Vec<u8>,
}

impl Packer {
    pub(crate) fn new() -> Self {
        Self { bits: Vec::new() }
    }

    /// Append `width` bits of `value`, MSB first.
    pub(crate) fn push(&mut self, width: u32, value: u32) {
        assert!((1..=32).contains(&width));
        assert!(width == 32 || value < (1u32 << width));
        for i in (0..width).rev() {
            self.bits.push(((value >> i) & 1) as u8);
        }
    }

    /// Append one universal codeword for code number `n`
    /// (spec/07 §1: `0 d₁ 0 d₂ … 0 dₖ 1`, `n = 2ᵏ − 1 + value`).
    pub(crate) fn ue(&mut self, n: u32) {
        let k = 31 - (n + 1).leading_zeros();
        let value = n + 1 - (1u32 << k);
        for i in (0..k).rev() {
            self.push(1, 0);
            self.push(1, (value >> i) & 1);
        }
        self.push(1, 1);
    }

    /// Append the signed fold of spec/06 §1.1 for `v`
    /// (`0 → 0`, `+m → 2m − 1`, `−m → 2m`).
    pub(crate) fn se(&mut self, v: i32) {
        let code = if v == 0 {
            0
        } else if v > 0 {
            (v as u32) * 2 - 1
        } else {
            (-v as u32) * 2
        };
        self.ue(code);
    }

    /// Number of bits accumulated so far.
    pub(crate) fn len(&self) -> usize {
        self.bits.len()
    }

    /// Pad with zero bits to the next byte boundary and return the
    /// packed bytes.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        let mut out = vec![0u8; self.bits.len().div_ceil(8)];
        for (i, &b) in self.bits.iter().enumerate() {
            out[i / 8] |= b << (7 - (i % 8));
        }
        out
    }
}

/// Pack `(width, value)` items MSB-first into bytes.
pub(crate) fn pack(items: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Packer::new();
    for &(width, value) in items {
        p.push(width, value);
    }
    p.into_bytes()
}

/// The `(width, bits)` form of one universal codeword, for callers
/// that assemble `(width, value)` lists.
pub(crate) fn uvlc(n: u32) -> (u32, u32) {
    let k = 31 - (n + 1).leading_zeros();
    let value = n + 1 - (1u32 << k);
    let mut bits: u32 = 0;
    for i in (0..k).rev() {
        bits = (bits << 2) | ((value >> i) & 1);
    }
    (2 * k + 1, (bits << 1) | 1)
}

/// The `(width, bits)` form of the signed fold of `v`.
pub(crate) fn svlc(v: i32) -> (u32, u32) {
    let code = if v == 0 {
        0
    } else if v > 0 {
        (v as u32) * 2 - 1
    } else {
        (-v as u32) * 2
    };
    uvlc(code)
}
