//! Integration tests pinning the SVQ1 L=4 / L=5 codebook ABSENCE
//! contract end-to-end across the four crate-public surfaces that
//! depend on it.
//!
//! The docs collaborator's Extractor 02 pass
//! (`docs/video/svq1/provenance/02-codebook-extraction.md`) resolved
//! the long-standing L=4 / L=5 codebook gap in the Sorenson Video TM
//! for QT R2.0 build as **architecturally absent**: no codebook exists
//! at 16×8 (L=4) or 16×16 (L=5); both block sizes are always
//! subdivided to ≤8×8 before quantisation. The corresponding
//! `tables/codebook-l4.meta` and `tables/codebook-l5.meta` records are
//! mirrored bit-exact under `crates/oxideav-svq/tables/`, parsed by
//! the build script, and consumed by:
//!
//! * [`oxideav_svq::svq1_codebook::Svq1Level::codebook_bytes_per_half`]
//!   — returns `None` for L=4 / L=5.
//! * [`oxideav_svq::svq1_codebook::Svq1Level::absence_record`] —
//!   returns `Some(SVQ1_L{4,5}_ABSENCE)` for L=4 / L=5.
//! * The constants
//!   [`oxideav_svq::svq1_codebook::SVQ1_L4_ABSENCE`] and
//!   [`oxideav_svq::svq1_codebook::SVQ1_L5_ABSENCE`] themselves —
//!   bit-exact mirrors of the meta-file scalar keys.
//! * The [`oxideav_svq::svq1_blocktree::read_block_decision`] walker
//!   — surfaces an in-place quantise request at L=4 / L=5 as the
//!   [`oxideav_svq::Error::InvalidLevelQuantise`] structural-failure
//!   variant.
//!
//! Each of these is also unit-tested inside the relevant module; the
//! integration-test layer below exercises the full chain from a
//! `Svq1Level` value through the absence record (matching the meta
//! file's scalars) and into the bit-decoder's rejection path, so that
//! a future change which keeps the unit tests green but breaks the
//! end-to-end contract surfaces here.

use oxideav_svq::svq1_blocktree::{read_block_decision, Svq1Level};
use oxideav_svq::svq1_codebook::{SVQ1_L4_ABSENCE, SVQ1_L5_ABSENCE};
use oxideav_svq::{BitReader, Error};

/// `Svq1Level::L4` is reported as having no codebook through every
/// surface that exposes the L=4 absence: the `codebook_bytes_per_half`
/// `None`, the `absence_record` `Some` carrying `SVQ1_L4_ABSENCE`, and
/// the underlying constant's scalar fields agreeing with
/// `docs/video/svq1/tables/codebook-l4.meta` line-by-line.
#[test]
fn svq1_level_l4_is_absent_end_to_end() {
    assert_eq!(Svq1Level::L4.codebook_bytes_per_half(), None);
    let record = Svq1Level::L4
        .absence_record()
        .expect("L=4 must report an absence record");
    assert_eq!(record, SVQ1_L4_ABSENCE);
    // tables/codebook-l4.meta:
    //   level: 4
    //   block_size: 16x8
    //   canonical_vector_len_bytes: 128
    //   canonical_6stage_intra_or_inter_bytes: 12288
    //   status: ABSENT
    assert_eq!(record.level, 4);
    assert_eq!(record.block_size, "16x8");
    assert_eq!(record.canonical_vector_len_bytes, 128);
    assert_eq!(record.canonical_6stage_intra_or_inter_bytes, 12288);
    // The "would-be" full codebook (both halves) would need 24576 B —
    // larger than half of the 23004-byte payload that actually exists.
    assert_eq!(record.canonical_6stage_intra_or_inter_bytes * 2, 24576);
    // The block dimensions reported by Svq1Level must agree with the
    // meta record's block_size string.
    assert_eq!(Svq1Level::L4.block_dims(), (16, 8));
    assert_eq!(Svq1Level::L4.vector_length(), 128);
}

/// `Svq1Level::L5` is reported as having no codebook through every
/// surface that exposes the L=5 absence; the underlying constant's
/// scalar fields agree with `docs/video/svq1/tables/codebook-l5.meta`.
#[test]
fn svq1_level_l5_is_absent_end_to_end() {
    assert_eq!(Svq1Level::L5.codebook_bytes_per_half(), None);
    let record = Svq1Level::L5
        .absence_record()
        .expect("L=5 must report an absence record");
    assert_eq!(record, SVQ1_L5_ABSENCE);
    // tables/codebook-l5.meta:
    //   level: 5
    //   block_size: 16x16
    //   canonical_vector_len_bytes: 256
    //   canonical_6stage_intra_or_inter_bytes: 24576
    //   status: ABSENT
    assert_eq!(record.level, 5);
    assert_eq!(record.block_size, "16x16");
    assert_eq!(record.canonical_vector_len_bytes, 256);
    assert_eq!(record.canonical_6stage_intra_or_inter_bytes, 24576);
    // The "would-be" full codebook (both halves) would need 49152 B —
    // more than twice the 23004-byte payload that actually exists.
    assert_eq!(record.canonical_6stage_intra_or_inter_bytes * 2, 49152);
    assert_eq!(Svq1Level::L5.block_dims(), (16, 16));
    assert_eq!(Svq1Level::L5.vector_length(), 256);
}

/// L=0..L=3 must NOT carry an absence record — these are the levels
/// whose codebook bytes ARE present in `tables/codebook-l0l3.csv`.
#[test]
fn svq1_levels_l0_through_l3_are_not_absent() {
    for (level, expected_per_half) in [
        (Svq1Level::L0, 768usize),
        (Svq1Level::L1, 1536),
        (Svq1Level::L2, 3072),
        (Svq1Level::L3, 6144),
    ] {
        assert_eq!(level.absence_record(), None, "{level:?} must not be absent");
        assert_eq!(
            level.codebook_bytes_per_half(),
            Some(expected_per_half),
            "{level:?} per-half byte count must match",
        );
    }
}

/// A bitstream that would otherwise tell the decoder to quantise an
/// L=4 block in place must surface as
/// [`Error::InvalidLevelQuantise`] — closing the loop between the
/// blocktree walker and the L=4 codebook absence record. The structural
/// `0` bit (the "quantise this block" branch at every level) is what
/// triggers the rejection at L=4.
#[test]
fn svq1_blocktree_rejects_in_place_quantise_at_l4_with_typed_error() {
    let bytes = [0b0000_0000_u8];
    let mut br = BitReader::new(&bytes);
    let err = read_block_decision(Svq1Level::L4, &mut br)
        .expect_err("L=4 + bit=0 must reject as InvalidLevelQuantise");
    assert_eq!(err, Error::InvalidLevelQuantise(Svq1Level::L4));
    // The walker consumes the bit before surfacing the error, so a
    // downstream caller can re-synchronise on the next macroblock.
    assert_eq!(br.bits_consumed(), 1);

    // The level surfaced by the error must agree with the absence
    // record's scalar `level` field — so a consumer recovering from
    // the structural failure can name the level using the same
    // documented numeric.
    let Error::InvalidLevelQuantise(reported_level) = err else {
        unreachable!()
    };
    let record = reported_level
        .absence_record()
        .expect("the rejected level must always carry an absence record");
    assert_eq!(u32::from(record.level), 4);
}

/// Same end-to-end shape as the L=4 case, with the L=5 (16×16) level.
#[test]
fn svq1_blocktree_rejects_in_place_quantise_at_l5_with_typed_error() {
    let bytes = [0b0000_0000_u8];
    let mut br = BitReader::new(&bytes);
    let err = read_block_decision(Svq1Level::L5, &mut br)
        .expect_err("L=5 + bit=0 must reject as InvalidLevelQuantise");
    assert_eq!(err, Error::InvalidLevelQuantise(Svq1Level::L5));
    assert_eq!(br.bits_consumed(), 1);

    let Error::InvalidLevelQuantise(reported_level) = err else {
        unreachable!()
    };
    let record = reported_level
        .absence_record()
        .expect("the rejected level must always carry an absence record");
    assert_eq!(u32::from(record.level), 5);
}

/// The 16-entry block-shape LUT at descriptor offset `+0x14` caps
/// quantised block sizes to L=0..L=3 (every entry is in `1..=4`). This
/// corroborates the absence resolution end-to-end: a cap value above
/// 4 would imply an L=4 / L=5 codebook is consulted in place; none
/// exists.
#[test]
fn svq1_block_shape_lut_corroborates_absence() {
    let lut = oxideav_svq::svq1_codebook::block_shape_lut();
    assert_eq!(lut.len(), 16);
    for &v in lut {
        assert!(
            (1..=4).contains(&v),
            "block-shape LUT entry {v} out of 1..=4 — would imply an L={} codebook present",
            v.saturating_sub(1),
        );
    }
    // Documented exact byte string from codebook-descriptor.meta
    // line 22.
    let expected: [u8; 16] = [
        0x04, 0x04, 0x03, 0x02, 0x04, 0x03, 0x03, 0x02, 0x03, 0x03, 0x02, 0x02, 0x03, 0x02, 0x02,
        0x01,
    ];
    assert_eq!(lut, expected);
}

/// The four crate-public surfaces that expose the L=4 / L=5 absence
/// are mutually consistent for EVERY level (not just L=4 / L=5).
#[test]
fn svq1_absence_surfaces_are_mutually_consistent() {
    for level in [
        Svq1Level::L0,
        Svq1Level::L1,
        Svq1Level::L2,
        Svq1Level::L3,
        Svq1Level::L4,
        Svq1Level::L5,
    ] {
        let has_codebook = level.codebook_bytes_per_half().is_some();
        let is_absent = level.absence_record().is_some();
        let rejects = level.rejects_in_place_quantise();
        // Exactly one of "has a codebook" / "carries an absence record"
        // must be true; both, or neither, is a bug.
        assert!(
            has_codebook ^ is_absent,
            "{level:?}: codebook_bytes_per_half / absence_record disagree"
        );
        // A level rejects in-place quantisation iff it carries an
        // absence record (per the blocktree walker's contract).
        assert_eq!(rejects, is_absent, "{level:?}: rejects vs absent disagree");
    }
}
