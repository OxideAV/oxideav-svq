//! SVQ1 block-tree subdivision walker (structural).
//!
//! The SVQ1 wire format described in
//! `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
//! Plane Data" assigns each block size in the codec's six-level
//! hierarchy a *level* in the range `0..=5`:
//!
//! | Level | Block size  | Vector length (samples) |
//! | ----- | ----------- | ----------------------- |
//! | L=5   | 16×16       | 256                     |
//! | L=4   | 16×8        | 128                     |
//! | L=3   | 8×8         |  64                     |
//! | L=2   | 8×4         |  32                     |
//! | L=1   | 4×4         |  16                     |
//! | L=0   | 4×2         |   8                     |
//!
//! Per the wiki spec, decoding walks the tree breadth-first starting
//! from each 16×16 macroblock (L=5). At each non-leaf level the
//! decoder reads ONE bit from the stream:
//!
//! ```text
//! process_block(level)
//!   if (level > 0) && (next_bit == 1)
//!     subdivide block into 2 (level - 1) halves
//!     insert first half into processing queue
//!     insert second half into processing queue
//!   else
//!     ...process block as a mean-removed multistage VQ leaf...
//! ```
//!
//! At L=0 the spec's base case is explicit ("blocks cannot be
//! subdivided beyond level 0 (4×2 samples)"): the walker does NOT
//! read a bit and the block is always quantised in place.
//!
//! ## Why L=4 and L=5 are always subdivided in this build
//!
//! The wiki spec's per-level inner loop fires an error path when a
//! quantised stage list is seen at a level ≥ 4:
//!
//! ```text
//! if (stages > 0) && (level >= 4)
//!   invalid vector, error out of decode since levels 4 and 5 blocks
//!   do not use multistage VQ
//! ```
//!
//! The clean-room codebook-extraction work captured under
//! `docs/video/svq1/spec/14.10-codebook-L4.md` and
//! `docs/video/svq1/spec/14.11-codebook-L5.md` confirms that in the
//! Sorenson Video TM for QT R2.0 build there is **no** L=4 / L=5
//! mean-removed vector codebook stored anywhere — the only
//! signed-byte VQ region in the binary holds exactly the L=0…L=3
//! codebooks. A bitstream that would otherwise tell the decoder to
//! quantise an L=4 or L=5 block in place is therefore malformed for
//! this format: there is nothing to look the vector up against.
//!
//! This module mirrors that fact in code. [`read_block_decision`]
//! reads ONE bit at L=1..=L=5. At L=0 it reads zero bits and always
//! returns [`Svq1BlockDecision::Quantise`].
//!
//! Note the wiki gate above fires on `(stages > 0)` — the STAGE
//! COUNT — not on the subdivide bit itself:
//! `docs/video/svq1/spec/03-block-hierarchy.md` §3.8.2 spells out
//! that a mean-only (`N = 0`) leaf at L=4 / L=5 is acceptable, and
//! the black-box conformance fixture
//! (`tests/svq1_intra_conformance.rs`) confirms real intra streams
//! DO emit `0`-subdivide bits at L=5 for flat 16×16 regions. The
//! walker therefore returns [`Svq1BlockDecision::Quantise`] at
//! every level; the `stages > 0 && level >= 4` rejection
//! ([`crate::Error::InvalidLevelQuantise`]) is enforced by the leaf
//! decoder in [`crate::svq1_plane`], exactly where the wiki
//! pseudocode places it. (An earlier round rejected the subdivide
//! bit here, which mis-parsed conformant streams.)

use crate::bitreader::BitReader;
use crate::error::Result;

/// SVQ1 block hierarchy level. See the table in the module-level
/// doc-comment for the level → block-size mapping enumerated in
/// `docs/video/svq1/wiki/Sorenson_Video_1.wiki` §"Decoding Intraframe
/// Plane Data".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Svq1Level {
    /// 4×2 samples — the leaf level. The wiki spec's base case:
    /// "blocks cannot be subdivided beyond level 0 (4×2 samples)".
    L0,
    /// 4×4 samples.
    L1,
    /// 8×4 samples.
    L2,
    /// 8×8 samples.
    L3,
    /// 16×8 samples. Always subdivided in this build per
    /// `docs/video/svq1/spec/14.10-codebook-L4.md`.
    L4,
    /// 16×16 samples (the macroblock). Always subdivided in this
    /// build per `docs/video/svq1/spec/14.11-codebook-L5.md`.
    L5,
}

impl Svq1Level {
    /// Return the block dimensions `(cols, rows)` in samples for this
    /// level, taken from the wiki spec's level table.
    pub const fn block_dims(self) -> (u8, u8) {
        match self {
            Svq1Level::L0 => (4, 2),
            Svq1Level::L1 => (4, 4),
            Svq1Level::L2 => (8, 4),
            Svq1Level::L3 => (8, 8),
            Svq1Level::L4 => (16, 8),
            Svq1Level::L5 => (16, 16),
        }
    }

    /// Number of samples covered by this level's block
    /// (`cols * rows`).
    pub const fn vector_length(self) -> u16 {
        let (c, r) = self.block_dims();
        (c as u16) * (r as u16)
    }

    /// `true` for the two top levels (L=4, L=5) — a leaf at either
    /// level cannot carry VQ stages (`N ≥ 1`), per the wiki spec's
    /// `(stages > 0) && (level >= 4)` error branch and the
    /// §14.10 / §14.11 clean-room resolution (no codebook stored at
    /// those levels). SKIP (`N = −1`, inter) and mean-only (`N = 0`)
    /// leaves ARE representable at L=4 / L=5 per
    /// `docs/video/svq1/spec/03-block-hierarchy.md` §3.8.2; the gate
    /// this predicate feeds fires on the stage count in the leaf
    /// decoder, not on the subdivide bit.
    pub const fn rejects_in_place_quantise(self) -> bool {
        matches!(self, Svq1Level::L4 | Svq1Level::L5)
    }
}

/// Per-block decoder decision recorded after walking the
/// subdivide-vs-quantise bit at a given level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Svq1BlockDecision {
    /// The decoder subdivides this block into two child halves of
    /// the next level down. The child level is the value returned by
    /// [`subdivide`] for the parent level.
    Subdivide,
    /// The decoder treats this block as a leaf and consumes the
    /// per-leaf stages-count VLC + mean VLC + codebook-selector
    /// payload described in the wiki spec's §"Decoding Intraframe
    /// Plane Data" inner loop. Leaf-payload decoding is still gated
    /// on `docs/video/svq1/CODEBOOK_GAP.md`.
    Quantise,
}

/// Walk one level of the SVQ1 block tree.
///
/// At L=1..=L=5 the function reads ONE bit from `br`. A bit value of
/// `1` means subdivide; `0` means quantise the block in place — at
/// EVERY level, including L=4 and L=5.
///
/// The wiki spec's error branch is `(stages > 0) && (level >= 4)` —
/// it gates the STAGE COUNT, not the subdivide bit
/// (`docs/video/svq1/spec/03-block-hierarchy.md` §3.8.2: "a
/// non-zero stage count is the violation, not a non-subdivide bit
/// per se … a conforming decoder MAY accept a mean-only leaf at
/// L=4 or L=5"). Real intra streams DO emit `0`-subdivide bits at
/// L=5 / L=4 for flat regions (mean-only 16×16 / 16×8 leaves) —
/// pinned empirically by the black-box conformance fixture in
/// `tests/svq1_intra_conformance.rs`, in the Validator role
/// spec/03 §3.10 item 1 called for. The `stages > 0` gate is
/// enforced by the leaf decoder ([`crate::svq1_plane`]) where the
/// wiki places it; earlier rounds rejected the subdivide bit here,
/// which mis-parsed conformant streams.
///
/// At L=0 the function reads zero bits and always returns
/// [`Svq1BlockDecision::Quantise`] (the wiki spec's
/// "blocks cannot be subdivided beyond level 0" base case).
pub fn read_block_decision(level: Svq1Level, br: &mut BitReader<'_>) -> Result<Svq1BlockDecision> {
    if level == Svq1Level::L0 {
        return Ok(Svq1BlockDecision::Quantise);
    }
    let bit = br.read_bit()?;
    if bit == 1 {
        return Ok(Svq1BlockDecision::Subdivide);
    }
    Ok(Svq1BlockDecision::Quantise)
}

/// Return the two child levels a block at `level` subdivides into,
/// or `None` if `level` is the leaf (L=0).
///
/// The wiki spec's §"Decoding Intraframe Plane Data" walks the tree
/// as a sequence of two-child splits at every non-leaf level (16×16
/// → two 16×8 halves; 16×8 → two 8×8 halves; …; 4×4 → two 4×2
/// halves). Both children land at the next level down.
pub const fn subdivide(level: Svq1Level) -> Option<(Svq1Level, Svq1Level)> {
    match level {
        Svq1Level::L0 => None,
        Svq1Level::L1 => Some((Svq1Level::L0, Svq1Level::L0)),
        Svq1Level::L2 => Some((Svq1Level::L1, Svq1Level::L1)),
        Svq1Level::L3 => Some((Svq1Level::L2, Svq1Level::L2)),
        Svq1Level::L4 => Some((Svq1Level::L3, Svq1Level::L3)),
        Svq1Level::L5 => Some((Svq1Level::L4, Svq1Level::L4)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    /// Pack the leading bits of a stream into a single byte, MSB
    /// first, padded with zero bits.
    fn pack(bits: &[u8]) -> [u8; 1] {
        let mut byte: u8 = 0;
        for (i, &b) in bits.iter().enumerate() {
            assert!(b == 0 || b == 1);
            byte |= (b & 1) << (7 - i);
        }
        [byte]
    }

    #[test]
    fn block_dims_match_wiki_table() {
        assert_eq!(Svq1Level::L0.block_dims(), (4, 2));
        assert_eq!(Svq1Level::L1.block_dims(), (4, 4));
        assert_eq!(Svq1Level::L2.block_dims(), (8, 4));
        assert_eq!(Svq1Level::L3.block_dims(), (8, 8));
        assert_eq!(Svq1Level::L4.block_dims(), (16, 8));
        assert_eq!(Svq1Level::L5.block_dims(), (16, 16));
    }

    #[test]
    fn vector_lengths_match_wiki_table() {
        assert_eq!(Svq1Level::L0.vector_length(), 8);
        assert_eq!(Svq1Level::L1.vector_length(), 16);
        assert_eq!(Svq1Level::L2.vector_length(), 32);
        assert_eq!(Svq1Level::L3.vector_length(), 64);
        assert_eq!(Svq1Level::L4.vector_length(), 128);
        assert_eq!(Svq1Level::L5.vector_length(), 256);
    }

    #[test]
    fn rejects_in_place_quantise_is_l4_and_l5_only() {
        assert!(!Svq1Level::L0.rejects_in_place_quantise());
        assert!(!Svq1Level::L1.rejects_in_place_quantise());
        assert!(!Svq1Level::L2.rejects_in_place_quantise());
        assert!(!Svq1Level::L3.rejects_in_place_quantise());
        assert!(Svq1Level::L4.rejects_in_place_quantise());
        assert!(Svq1Level::L5.rejects_in_place_quantise());
    }

    #[test]
    fn subdivide_returns_pair_for_l1_through_l5() {
        assert_eq!(subdivide(Svq1Level::L0), None);
        assert_eq!(
            subdivide(Svq1Level::L1),
            Some((Svq1Level::L0, Svq1Level::L0))
        );
        assert_eq!(
            subdivide(Svq1Level::L2),
            Some((Svq1Level::L1, Svq1Level::L1))
        );
        assert_eq!(
            subdivide(Svq1Level::L3),
            Some((Svq1Level::L2, Svq1Level::L2))
        );
        assert_eq!(
            subdivide(Svq1Level::L4),
            Some((Svq1Level::L3, Svq1Level::L3))
        );
        assert_eq!(
            subdivide(Svq1Level::L5),
            Some((Svq1Level::L4, Svq1Level::L4))
        );
    }

    #[test]
    fn l0_reads_no_bit_and_returns_quantise() {
        // Empty backing slice — reading any bit would fail, so the
        // "no bit consumed" property is checked by the fact that we
        // get Quantise back instead of an error.
        let bytes: [u8; 0] = [];
        let mut br = BitReader::new(&bytes);
        let decision = read_block_decision(Svq1Level::L0, &mut br).unwrap();
        assert_eq!(decision, Svq1BlockDecision::Quantise);
        assert_eq!(br.bits_consumed(), 0);
    }

    #[test]
    fn l3_one_bit_one_returns_subdivide() {
        let bytes = pack(&[1]);
        let mut br = BitReader::new(&bytes);
        let decision = read_block_decision(Svq1Level::L3, &mut br).unwrap();
        assert_eq!(decision, Svq1BlockDecision::Subdivide);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn l3_one_bit_zero_returns_quantise() {
        let bytes = pack(&[0]);
        let mut br = BitReader::new(&bytes);
        let decision = read_block_decision(Svq1Level::L3, &mut br).unwrap();
        assert_eq!(decision, Svq1BlockDecision::Quantise);
        assert_eq!(br.bits_consumed(), 1);
    }

    #[test]
    fn l1_through_l3_zero_bit_quantises() {
        for level in [Svq1Level::L1, Svq1Level::L2, Svq1Level::L3] {
            let bytes = pack(&[0]);
            let mut br = BitReader::new(&bytes);
            let decision = read_block_decision(level, &mut br).unwrap();
            assert_eq!(
                decision,
                Svq1BlockDecision::Quantise,
                "level {level:?} with bit=0 must quantise"
            );
        }
    }

    #[test]
    fn l1_through_l5_one_bit_subdivides() {
        for level in [
            Svq1Level::L1,
            Svq1Level::L2,
            Svq1Level::L3,
            Svq1Level::L4,
            Svq1Level::L5,
        ] {
            let bytes = pack(&[1]);
            let mut br = BitReader::new(&bytes);
            let decision = read_block_decision(level, &mut br).unwrap();
            assert_eq!(
                decision,
                Svq1BlockDecision::Subdivide,
                "level {level:?} with bit=1 must subdivide"
            );
            assert_eq!(br.bits_consumed(), 1);
        }
    }

    /// A `0` bit at L=4 / L=5 is a LEAF decision (spec/03 §3.8.2 —
    /// mean-only leaves at those levels are representable and real
    /// intra streams emit them); the `stages > 0 && level >= 4`
    /// rejection belongs to the leaf decoder, not the walker.
    #[test]
    fn l4_l5_zero_bit_is_a_leaf_decision() {
        for level in [Svq1Level::L4, Svq1Level::L5] {
            let bytes = pack(&[0]);
            let mut br = BitReader::new(&bytes);
            let decision = read_block_decision(level, &mut br).unwrap();
            assert_eq!(
                decision,
                Svq1BlockDecision::Quantise,
                "{level:?} with bit=0 is a leaf; the stage-count gate fires downstream"
            );
            assert_eq!(br.bits_consumed(), 1);
        }
    }

    #[test]
    fn l1_through_l5_truncated_returns_truncated() {
        let bytes: [u8; 0] = [];
        for level in [
            Svq1Level::L1,
            Svq1Level::L2,
            Svq1Level::L3,
            Svq1Level::L4,
            Svq1Level::L5,
        ] {
            let mut br = BitReader::new(&bytes);
            assert_eq!(
                read_block_decision(level, &mut br).unwrap_err(),
                Error::Truncated,
                "level {level:?} must surface truncation"
            );
        }
    }

    #[test]
    fn breadth_first_walk_three_macroblocks() {
        // Walk three independent L=5 macroblocks back-to-back:
        //   MB0: L5=1 (subdivide), top L4=1 (subdivide), top L3=0 (quantise),
        //        bottom L3=0 (quantise), bottom L4=1 (subdivide), then four
        //        L3=0 leaves.
        //   MB1: L5=1 (subdivide), both L4=1 (subdivide), then four L3=0
        //        leaves.
        //   MB2: L5=1 (subdivide), both L4=1 (subdivide), then four L3=0
        //        leaves.
        // The walker only needs to assert that the spec's level→bit
        // count semantics agree with the test's hand-packed stream.
        let stream = pack(&[
            1, 1, 0, 0, 1, 0, 0, // MB0: 7 bits used by L5/L4/L3 decisions only.
        ]);
        let mut br = BitReader::new(&stream);
        // L5 subdivide.
        assert_eq!(
            read_block_decision(Svq1Level::L5, &mut br).unwrap(),
            Svq1BlockDecision::Subdivide
        );
        // Two L4 children: first subdivides, second quantises (illegal
        // at L4 — but the walker only enforces that on a per-bit
        // basis; here we read the legal 1+0 path then the next L4 is
        // a fresh call that subdivides).
        assert_eq!(
            read_block_decision(Svq1Level::L4, &mut br).unwrap(),
            Svq1BlockDecision::Subdivide
        );
        // Two L3 leaves below the first L4 child.
        assert_eq!(
            read_block_decision(Svq1Level::L3, &mut br).unwrap(),
            Svq1BlockDecision::Quantise
        );
        assert_eq!(
            read_block_decision(Svq1Level::L3, &mut br).unwrap(),
            Svq1BlockDecision::Quantise
        );
        // Second L4 child subdivides.
        assert_eq!(
            read_block_decision(Svq1Level::L4, &mut br).unwrap(),
            Svq1BlockDecision::Subdivide
        );
        // Two L3 leaves below it.
        assert_eq!(
            read_block_decision(Svq1Level::L3, &mut br).unwrap(),
            Svq1BlockDecision::Quantise
        );
        assert_eq!(
            read_block_decision(Svq1Level::L3, &mut br).unwrap(),
            Svq1BlockDecision::Quantise
        );
        assert_eq!(br.bits_consumed(), 7);
    }
}
