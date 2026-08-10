//! SVQ3 coded-block-pattern decode.
//!
//! Implements `docs/video/svq3/spec/03-coded-block-pattern.md`: a
//! macroblock's coded-block-pattern is a single integer in `0..=47`,
//!
//! ```text
//! cbp = cbp_luma | (cbp_chroma << 4)
//! ```
//!
//! where `cbp_luma` (4 bits) carries one bit per 8×8 luma quadrant in
//! raster order (bit 0 = top-left … bit 3 = bottom-right, §1.1) and
//! `cbp_chroma` is a single **3-valued class shared by both chroma
//! planes** (§1.2): 0 = no chroma residual, 1 = chroma DC only, 2 =
//! chroma DC and AC.
//!
//! The element is carried as one universal-code code number
//! ([`crate::svq3::read_universal_code`], spec/06 §1) mapped through
//! one of two 48-entry tables — **intra and inter macroblocks use
//! different mapping tables** (§4), transcribed here in the decode
//! direction (code number → pattern) from
//! `docs/video/svq3/tables/01-cbp-code-mapping.csv`. Which table
//! applies is decided by the macroblock's prediction class, not the
//! frame type: an intra-predicted macroblock inside a P/B frame takes
//! the intra table (§4).
//!
//! Intra 16×16 macroblocks (type code numbers 9…32) carry **no** CBP
//! element at all — their pattern is implied by the type
//! ([`crate::svq3_mb::Intra16x16Params`], spec/03 §2 / spec/04 §4.5).

use crate::bitreader::BitReader;
use crate::error::{Error, Result};
use crate::svq3::read_universal_code;

/// The intra CBP mapping, decode direction: `INTRA_CBP_TABLE[code]`
/// is the coded-block-pattern value for code number `code`
/// (`docs/video/svq3/tables/01-cbp-code-mapping.csv`, `intra_cbp`
/// column; cross-checked byte-identical against the decompressor's
/// decode-direction table per the CSV `.meta` / spec/03 §4).
pub const INTRA_CBP_TABLE: [u8; 48] = [
    47, 31, 15, 0, 23, 27, 29, 30, 7, 11, 13, 14, 39, 43, 45, 46, 16, 3, 5, 10, 12, 19, 21, 26, 28,
    35, 37, 42, 44, 1, 2, 4, 8, 17, 18, 20, 24, 6, 9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
];

/// The inter CBP mapping, decode direction
/// (`docs/video/svq3/tables/01-cbp-code-mapping.csv`, `inter_cbp`
/// column).
pub const INTER_CBP_TABLE: [u8; 48] = [
    0, 16, 1, 2, 4, 8, 32, 3, 5, 10, 12, 15, 47, 7, 11, 13, 14, 6, 9, 31, 35, 37, 42, 44, 33, 34,
    36, 40, 39, 43, 45, 46, 17, 18, 20, 24, 19, 21, 26, 28, 23, 27, 29, 30, 22, 25, 38, 41,
];

/// A decoded coded-block-pattern, split into its two subfields
/// (spec/03 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodedBlockPattern {
    /// One bit per 8×8 luma quadrant in raster order: bit 0 = (0, 0),
    /// bit 1 = (8, 0), bit 2 = (0, 8), bit 3 = (8, 8). A clear bit
    /// means the whole quadrant (all four of its 4×4 blocks) is
    /// absent from the bitstream.
    pub luma: u8,
    /// The 3-valued chroma class shared by both planes: 0 = no chroma
    /// residual, 1 = chroma DC only, 2 = chroma DC and AC. A plane
    /// whose own content class is lower is padded with zero
    /// coefficients — there is no per-plane chroma flag.
    pub chroma: u8,
}

impl CodedBlockPattern {
    /// Split a raw `0..=47` pattern value into its subfields.
    #[must_use]
    pub const fn from_value(cbp: u8) -> Self {
        Self {
            luma: cbp & 0x0F,
            chroma: cbp >> 4,
        }
    }

    /// The raw `cbp_luma | (cbp_chroma << 4)` composition.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.luma | (self.chroma << 4)
    }

    /// Whether luma 8×8 quadrant `quadrant` (raster order, `0..=3`)
    /// carries coefficients.
    #[must_use]
    pub const fn luma_quadrant_coded(self, quadrant: usize) -> bool {
        (self.luma >> quadrant) & 1 == 1
    }

    /// Whether the 2×2 chroma DC block of each plane is present
    /// (class ≥ 1).
    #[must_use]
    pub const fn chroma_dc_coded(self) -> bool {
        self.chroma >= 1
    }

    /// Whether the chroma AC coefficients of the four 4×4 blocks of
    /// each plane are present (class 2).
    #[must_use]
    pub const fn chroma_ac_coded(self) -> bool {
        self.chroma == 2
    }
}

/// Read one coded-block-pattern element for an **intra-predicted**
/// macroblock (the intra 4×4 type): one universal-code code number
/// mapped through [`INTRA_CBP_TABLE`].
///
/// Returns [`Error::InvalidFrameCode`] when the code number is
/// outside the 48-entry alphabet (spec/03 §4: both mappings are
/// bijections over exactly 48 values).
pub fn read_cbp_intra(br: &mut BitReader<'_>) -> Result<CodedBlockPattern> {
    let code = read_universal_code(br)?;
    let Some(&cbp) = INTRA_CBP_TABLE.get(code as usize) else {
        return Err(Error::InvalidFrameCode(code));
    };
    Ok(CodedBlockPattern::from_value(cbp))
}

/// Read one coded-block-pattern element for an **inter-predicted**
/// macroblock: one universal-code code number mapped through
/// [`INTER_CBP_TABLE`].
///
/// Returns [`Error::InvalidFrameCode`] when the code number is
/// outside the 48-entry alphabet.
pub fn read_cbp_inter(br: &mut BitReader<'_>) -> Result<CodedBlockPattern> {
    let code = read_universal_code(br)?;
    let Some(&cbp) = INTER_CBP_TABLE.get(code as usize) else {
        return Err(Error::InvalidFrameCode(code));
    };
    Ok(CodedBlockPattern::from_value(cbp))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both mapping tables are exact permutations of the 48-value
    /// pattern space (spec/03 §4: bijections between the 48 code
    /// numbers and the 48 patterns).
    #[test]
    fn tables_are_permutations_of_the_pattern_space() {
        for table in [&INTRA_CBP_TABLE, &INTER_CBP_TABLE] {
            let mut seen = [false; 48];
            for &v in table.iter() {
                assert!(v < 48, "pattern {v} out of range");
                let luma = v & 0x0F;
                let chroma = v >> 4;
                assert!(luma <= 15 && chroma <= 2, "invalid subfields in {v}");
                assert!(!seen[v as usize], "pattern {v} repeated");
                seen[v as usize] = true;
            }
            assert!(seen.iter().all(|&s| s), "not all 48 patterns covered");
        }
    }

    /// spec/03 §4's statistics sanity table: the first five rows of
    /// each mapping.
    #[test]
    fn first_code_numbers_match_spec_table() {
        assert_eq!(INTRA_CBP_TABLE[0], 47, "intra code 0 buys everything");
        assert_eq!(INTRA_CBP_TABLE[1], 31);
        assert_eq!(INTRA_CBP_TABLE[2], 15);
        assert_eq!(INTRA_CBP_TABLE[3], 0, "all-zero costs code 3 in intra");
        assert_eq!(INTRA_CBP_TABLE[4], 23);
        assert_eq!(INTER_CBP_TABLE[0], 0, "inter code 0 buys nothing coded");
        assert_eq!(INTER_CBP_TABLE[1], 16, "chroma DC only");
        assert_eq!(INTER_CBP_TABLE[2], 1);
        assert_eq!(INTER_CBP_TABLE[3], 2);
        assert_eq!(INTER_CBP_TABLE[4], 4);
    }

    /// The two tables are unrelated permutations, not shifts or
    /// inverses of one another (spec/03 §4).
    #[test]
    fn intra_and_inter_tables_differ() {
        assert_ne!(INTRA_CBP_TABLE, INTER_CBP_TABLE);
    }

    #[test]
    fn pattern_split_and_predicates() {
        let p = CodedBlockPattern::from_value(47);
        assert_eq!(p.luma, 15);
        assert_eq!(p.chroma, 2);
        assert_eq!(p.value(), 47);
        assert!(p.luma_quadrant_coded(0));
        assert!(p.luma_quadrant_coded(3));
        assert!(p.chroma_dc_coded());
        assert!(p.chroma_ac_coded());

        let q = CodedBlockPattern::from_value(16);
        assert_eq!(q.luma, 0);
        assert_eq!(q.chroma, 1);
        assert!(!q.luma_quadrant_coded(0));
        assert!(q.chroma_dc_coded());
        assert!(!q.chroma_ac_coded());

        let z = CodedBlockPattern::from_value(0);
        assert!(!z.chroma_dc_coded());
        assert!(!z.chroma_ac_coded());
        assert_eq!(z.value(), 0);
    }

    /// Wire read: universal code 0 ("1") maps to pattern 47 on the
    /// intra table and 0 on the inter table — the divergence spec/03
    /// §4 highlights (a decoder applying the wrong table
    /// desynchronises immediately).
    #[test]
    fn wire_reads_use_the_right_table() {
        let bits = [0b1000_0000u8];
        let mut br = BitReader::new(&bits);
        assert_eq!(read_cbp_intra(&mut br).unwrap().value(), 47);
        let mut br = BitReader::new(&bits);
        assert_eq!(read_cbp_inter(&mut br).unwrap().value(), 0);
    }

    #[test]
    fn wire_read_rejects_out_of_alphabet_code() {
        // Universal code 48: n = 5, value = 48 + 1 - 32 = 17 = 0b10001
        // → bits "0 0 1 0 (0 0) (0 0) (0 1) 1" = "00100000011".
        let bits = [0b0010_0000, 0b0110_0000];
        let mut br = BitReader::new(&bits);
        assert!(matches!(
            read_cbp_intra(&mut br),
            Err(Error::InvalidFrameCode(48))
        ));
    }
}
