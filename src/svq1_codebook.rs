//! SVQ1 mean-removed multistage VQ codebook payload (L=0..L=3).
//!
//! ## Provenance
//!
//! The 23004-byte signed-i8 payload + 36-byte descriptor prefix are
//! parsed at build time from `tables/codebook-l0l3.csv` +
//! `tables/codebook-descriptor.csv`, which are bit-exact mirrors of
//! `docs/video/svq1/tables/codebook-{l0l3,descriptor}.csv`. The docs
//! were produced by Extractor 02
//! (`docs/video/svq1/provenance/02-codebook-extraction.md`) from the
//! reference binary `quicktimethirdparty.qtx`
//! SHA-256 `ac3509bf22aa1458dfc6e1af980956c0153b4c287af452ae5b9cac6f923be169`,
//! file offset `0x5d200..0x62c00` (VMA `0x67dcd200..0x67dd2c00`).
//!
//! No external implementation source is consulted.
//!
//! ## Size arithmetic
//!
//! Per `docs/video/svq1/tables/codebook-l0l3.meta`:
//!
//! ```text
//!     L0: vec_len=8B    stage=128B    6-stage codebook=768B
//!     L1: vec_len=16B   stage=256B    6-stage codebook=1536B
//!     L2: vec_len=32B   stage=512B    6-stage codebook=3072B
//!     L3: vec_len=64B   stage=1024B   6-stage codebook=6144B
//! ```
//!
//! The full region is `2 × (768 + 1536 + 3072 + 6144) = 23040 B`,
//! decomposed as a 36-byte descriptor/LUT prefix + 23004-byte vector
//! payload.
//!
//! ## L=4 / L=5 status
//!
//! No codebook exists at L=4 (16×8) or L=5 (16×16) in this build —
//! see [`docs/video/svq1/spec/14.10-codebook-L4.md`] and
//! [`docs/video/svq1/spec/14.11-codebook-L5.md`]. The block-shape LUT
//! at descriptor offset `+0x14` caps quantised block sizes to L=0..L=3
//! (no entry exceeds 4); see
//! [`Svq1Level::codebook_bytes_per_half`] for the codebook size
//! returned per level (`None` for L=4 / L=5).
//!
//! The two `tables/codebook-l{4,5}.meta` records produced by the docs
//! collaborator's Extractor 02 pass (mirrored bit-exact under
//! `crates/oxideav-svq/tables/`) are parsed at build time into the
//! typed [`Svq1AbsentLevelRecord`] constants [`SVQ1_L4_ABSENCE`] and
//! [`SVQ1_L5_ABSENCE`]. The build script verifies that each record's
//! `status` field reads `ABSENT` and that the canonical
//! vector-length / per-half byte-count fields match the values
//! derivable from [`Svq1Level::vector_length`], so a future docs
//! revision that flips a level back to "present" or silently changes
//! the canonical sizes fails the build before any consumer relies on
//! the `None` invariant. See [`Svq1Level::absence_record`] for the
//! ergonomic `Svq1Level → Option<Svq1AbsentLevelRecord>` accessor.
//!
//! ## Open work (not blocked on this crate)
//!
//! The exact intra-vs-inter ordering and the stage-vs-level
//! interleave *within* the 23004-byte payload is a sibling docs spec
//! task per the `codebook-l0l3.meta` "NOTE: the precise intra/inter
//! ordering and stage-vs-level interleave WITHIN this region is the
//! L0..L3 spec's concern (sibling task)". Until the layout spec lands,
//! this module exposes the raw payload + per-level byte counts but
//! does NOT yet expose a `(level, stage, intra_or_inter, vector_idx)
//! → &[i8]` accessor.

use crate::svq1_blocktree::Svq1Level;

include!(concat!(env!("OUT_DIR"), "/svq1_codebook_data.rs"));

/// Total byte count of the mean-removed VQ vector payload for
/// L=0..L=3 (intra + inter halves), excluding the 36-byte descriptor
/// prefix. See `docs/video/svq1/tables/codebook-l0l3.meta`.
pub const SVQ1_CODEBOOK_PAYLOAD_BYTES: usize = 23004;

/// Total byte count of the 36-byte descriptor + block-shape prefix
/// at file offset `0x5d200..0x5d224` of the reference binary.
pub const SVQ1_CODEBOOK_DESCRIPTOR_BYTES: usize = 36;

/// Length of the block-shape LUT at descriptor offset `+0x14`.
pub const SVQ1_BLOCK_SHAPE_LUT_LEN: usize = 16;

/// Number of mean-removed multistage VQ stages per level (intra or
/// inter half). The SVQ1 wiki references in `docs/video/svq1/wiki/`
/// describe each codebook as a stack of 6 stages of 16 entries; the
/// extracted region size matches that exactly for L=0..L=3.
pub const SVQ1_STAGES_PER_LEVEL: usize = 6;

/// Number of vector entries per stage.
pub const SVQ1_ENTRIES_PER_STAGE: usize = 16;

/// Typed mirror of a `codebook-lN.meta` `status: ABSENT` record under
/// `docs/video/svq1/tables/`.
///
/// Each instance corresponds to one architecturally-absent SVQ1
/// codebook level (L=4 or L=5 in the Sorenson Video TM for QT R2.0
/// build). The fields are exactly the meta file's scalar keys
/// (`level`, `block_size`, `canonical_vector_len_bytes`,
/// `canonical_6stage_intra_or_inter_bytes`) — the multi-line
/// `resolution` and `evidence_rvas` YAML-block scalars are not
/// mirrored here; consult `docs/video/svq1/tables/codebook-l{4,5}.meta`
/// for the binary evidence that pinned each record as ABSENT.
///
/// The `canonical_*` fields document the size the codebook **would**
/// have under the conventional six-level extension of the
/// codebook hierarchy. `canonical_vector_len_bytes` equals
/// `block_cols * block_rows` samples;
/// `canonical_6stage_intra_or_inter_bytes` equals
/// `canonical_vector_len_bytes` multiplied by `16` (entries per
/// stage) and by `6` (stages per level). The values are deliberately
/// kept in the constant so the build assertion can verify that the
/// docs and the code agree on the would-be footprint that was ruled
/// out.
///
/// See [`SVQ1_L4_ABSENCE`] and [`SVQ1_L5_ABSENCE`] for the two
/// instances populated by `build.rs` from
/// `tables/codebook-l{4,5}.meta`, and [`Svq1Level::absence_record`]
/// for the ergonomic accessor that returns one of those constants
/// (or `None` for the L=0..L=3 codebooks that ARE present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Svq1AbsentLevelRecord {
    /// Level number — `4` or `5`, mirroring the `level:` key.
    pub level: u8,
    /// Block size — `"16x8"` or `"16x16"`, mirroring the
    /// `block_size:` key. `'static str` because the build script
    /// substitutes the literal at compile time.
    pub block_size: &'static str,
    /// Canonical per-vector byte count — `128` (L=4) or `256` (L=5),
    /// mirroring the `canonical_vector_len_bytes:` key. Equal to
    /// `block_cols * block_rows` per the level's spec table.
    pub canonical_vector_len_bytes: u32,
    /// Canonical per-half codebook size in bytes — `12288` (L=4) or
    /// `24576` (L=5), mirroring the
    /// `canonical_6stage_intra_or_inter_bytes:` key. Equal to
    /// `canonical_vector_len_bytes * 16 entries * 6 stages`. The full
    /// would-be codebook (both halves) is twice this value; neither
    /// fits in the reference binary's signed-byte VQ region.
    pub canonical_6stage_intra_or_inter_bytes: u32,
}

impl Svq1Level {
    /// Byte count of the L=`self` codebook for **one** half
    /// (intra OR inter), or `None` for the always-subdivided levels
    /// L=4 and L=5.
    ///
    /// Computed as `vector_length() * SVQ1_ENTRIES_PER_STAGE *
    /// SVQ1_STAGES_PER_LEVEL`. The four returned values
    /// (768, 1536, 3072, 6144) are exactly the per-level breakdown
    /// recorded in `docs/video/svq1/tables/codebook-l0l3.meta`:
    ///
    /// | Level | vec_len | × 16 entries | × 6 stages | bytes/half |
    /// |-------|--------:|-------------:|-----------:|-----------:|
    /// | L=0   |    8    |    128       |    768     |    768     |
    /// | L=1   |   16    |    256       |   1536     |   1536     |
    /// | L=2   |   32    |    512       |   3072     |   3072     |
    /// | L=3   |   64    |   1024       |   6144     |   6144     |
    /// | L=4   |  128    |  (subdivided) |  N/A      |   None     |
    /// | L=5   |  256    |  (subdivided) |  N/A      |   None     |
    ///
    /// Per-half × 2 (intra + inter) summed across L=0..L=3 is
    /// `2 × (768 + 1536 + 3072 + 6144) = 23040 B`, matching the
    /// extracted region total of 23040 B (`= 36 B descriptor +
    /// 23004 B payload`).
    pub const fn codebook_bytes_per_half(self) -> Option<usize> {
        match self {
            Svq1Level::L0 => Some(8 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L1 => Some(16 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L2 => Some(32 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L3 => Some(64 * SVQ1_ENTRIES_PER_STAGE * SVQ1_STAGES_PER_LEVEL),
            Svq1Level::L4 | Svq1Level::L5 => None,
        }
    }

    /// Return the [`Svq1AbsentLevelRecord`] for the always-subdivided
    /// levels (L=4 / L=5), or `None` for the L=0..L=3 levels that DO
    /// have a codebook stored in the reference binary.
    ///
    /// The returned record is one of the [`SVQ1_L4_ABSENCE`] /
    /// [`SVQ1_L5_ABSENCE`] constants that `build.rs` populates from
    /// `tables/codebook-l{4,5}.meta`. Use this when you want to
    /// surface a structured "no codebook at this level" diagnostic
    /// — e.g. when rejecting a malformed bitstream that asks for an
    /// in-place quantisation at L=4 or L=5 (the
    /// [`crate::Error::InvalidLevelQuantise`] case) — without
    /// hard-coding the canonical-size numbers in the caller.
    ///
    /// Invariant guaranteed by the build script:
    /// `self.absence_record().is_some() ==
    ///  self.codebook_bytes_per_half().is_none()` for every
    /// `Svq1Level`. Both predicates fire on exactly the L=4 / L=5 set.
    pub const fn absence_record(self) -> Option<Svq1AbsentLevelRecord> {
        match self {
            Svq1Level::L4 => Some(SVQ1_L4_ABSENCE),
            Svq1Level::L5 => Some(SVQ1_L5_ABSENCE),
            Svq1Level::L0 | Svq1Level::L1 | Svq1Level::L2 | Svq1Level::L3 => None,
        }
    }
}

/// Borrowed view over the full 23004-byte L=0..L=3 codebook payload.
///
/// The interpretation of slices *within* this payload (intra vs inter
/// ordering, stage-vs-level interleave) is still a sibling docs spec
/// task — see the module-level "Open work" note.
pub fn codebook_l0l3_payload() -> &'static [i8] {
    &SVQ1_CODEBOOK_L0L3_BYTES
}

/// Borrowed view over the 36-byte descriptor prefix at file offset
/// `0x5d200..0x5d224` of the reference binary.
pub fn codebook_descriptor() -> &'static [u8] {
    &SVQ1_CODEBOOK_DESCRIPTOR
}

/// Borrowed view over the 16-entry block-shape LUT at descriptor
/// offset `+0x14`. All entries are in the range 1..=4.
pub fn block_shape_lut() -> &'static [u8] {
    &SVQ1_BLOCK_SHAPE_LUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_length_matches_documented_size() {
        // docs/video/svq1/tables/codebook-l0l3.meta: byte_length: 23004
        assert_eq!(SVQ1_CODEBOOK_L0L3_BYTES.len(), SVQ1_CODEBOOK_PAYLOAD_BYTES);
        assert_eq!(SVQ1_CODEBOOK_PAYLOAD_BYTES, 23004);
    }

    #[test]
    fn descriptor_length_matches_documented_size() {
        // docs/video/svq1/tables/codebook-descriptor.meta: byte_length: 36
        assert_eq!(
            SVQ1_CODEBOOK_DESCRIPTOR.len(),
            SVQ1_CODEBOOK_DESCRIPTOR_BYTES
        );
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR_BYTES, 36);
    }

    #[test]
    fn full_region_matches_size_arithmetic() {
        // codebook-l0l3.meta line 24: "region total 23040 B = 23040 B
        // canonical (= 36 B over the 23004 B vector payload; the 36 B
        // is the descriptor/LUT prefix)."
        let descriptor_plus_payload =
            SVQ1_CODEBOOK_DESCRIPTOR.len() + SVQ1_CODEBOOK_L0L3_BYTES.len();
        assert_eq!(descriptor_plus_payload, 23040);

        // Per-half canonical L0..L3 sum:
        // 768 + 1536 + 3072 + 6144 = 11520; both halves = 23040.
        let per_half = Svq1Level::L0.codebook_bytes_per_half().unwrap()
            + Svq1Level::L1.codebook_bytes_per_half().unwrap()
            + Svq1Level::L2.codebook_bytes_per_half().unwrap()
            + Svq1Level::L3.codebook_bytes_per_half().unwrap();
        assert_eq!(per_half, 11520);
        assert_eq!(per_half * 2, 23040);
        assert_eq!(per_half * 2, descriptor_plus_payload);
    }

    #[test]
    fn per_level_byte_counts_match_docs_meta() {
        // docs/video/svq1/tables/codebook-l0l3.meta lines 26-29
        assert_eq!(Svq1Level::L0.codebook_bytes_per_half(), Some(768));
        assert_eq!(Svq1Level::L1.codebook_bytes_per_half(), Some(1536));
        assert_eq!(Svq1Level::L2.codebook_bytes_per_half(), Some(3072));
        assert_eq!(Svq1Level::L3.codebook_bytes_per_half(), Some(6144));
    }

    #[test]
    fn l4_l5_have_no_codebook() {
        // docs/video/svq1/spec/14.10-codebook-L4.md + 14.11-codebook-L5.md
        // RESOLVED ABSENT — block-shape LUT caps at 4 (four block
        // sizes); 16×8 and 16×16 are always subdivided.
        assert_eq!(Svq1Level::L4.codebook_bytes_per_half(), None);
        assert_eq!(Svq1Level::L5.codebook_bytes_per_half(), None);
    }

    #[test]
    fn block_shape_lut_matches_descriptor_csv() {
        // docs/video/svq1/tables/codebook-descriptor.meta line 22:
        // "Block-shape LUT bytes: 04 04 03 02 04 03 03 02 03 03 02 02 03 02 02 01."
        let expected: [u8; 16] = [
            0x04, 0x04, 0x03, 0x02, 0x04, 0x03, 0x03, 0x02, 0x03, 0x03, 0x02, 0x02, 0x03, 0x02,
            0x02, 0x01,
        ];
        assert_eq!(SVQ1_BLOCK_SHAPE_LUT, expected);
        assert_eq!(block_shape_lut(), &expected[..]);
    }

    #[test]
    fn block_shape_lut_caps_at_four() {
        // Every entry must be in 1..=4 per the spec/14.10 + 14.11
        // resolution: a value of 5 or 6 would imply an L=4 or L=5
        // codebook is consulted in place; none exists.
        for &v in block_shape_lut() {
            assert!(
                (1..=4).contains(&v),
                "block-shape LUT entry {v} out of 1..=4"
            );
        }
    }

    #[test]
    fn descriptor_csv_first_record_matches() {
        // docs/video/svq1/tables/codebook-descriptor.csv row 1 (after
        // header): byte 0 = 0x03, byte 3 = 0x18, byte 4 = 0x02.
        // The descriptor's first 9-byte record b0 cycles 5..0 per the
        // meta line 20-21 "Two identical descriptor groups precede the
        // LUT (0x5d1cc and 0x5d1f4); each enumerates the full
        // six-level block hierarchy 5..0".
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[0], 0x03);
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[3], 0x18);
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[4], 0x02);
        // Block-shape LUT starts at offset 0x14
        assert_eq!(SVQ1_CODEBOOK_DESCRIPTOR[0x14], 0x04);
    }

    #[test]
    fn codebook_l0l3_first_bytes_match_hex_table_row1() {
        // docs/video/svq1/tables/codebook-l0l3.hex row 1:
        // "02 01 00 ff 01 00 ff ff 01 00 ff fe 00 ff fe fd"
        // The CSV/hex pair are extracted from the same source slice,
        // so checking the first 16 bytes anchors the build-script
        // CSV parse to the documented hex view.
        let expected_signed: [i8; 16] = [
            0x02, 0x01, 0x00, -1, 0x01, 0x00, -1, -1, 0x01, 0x00, -1, -2, 0x00, -1, -2, -3,
        ];
        assert_eq!(&SVQ1_CODEBOOK_L0L3_BYTES[..16], &expected_signed[..]);
    }

    #[test]
    fn codebook_l0l3_payload_accessor_aliases_static() {
        let p = codebook_l0l3_payload();
        assert_eq!(p.len(), 23004);
        assert_eq!(p.as_ptr(), SVQ1_CODEBOOK_L0L3_BYTES.as_ptr());
    }

    #[test]
    fn codebook_descriptor_accessor_aliases_static() {
        let d = codebook_descriptor();
        assert_eq!(d.len(), 36);
        assert_eq!(d.as_ptr(), SVQ1_CODEBOOK_DESCRIPTOR.as_ptr());
    }

    #[test]
    fn svq1_l4_absence_matches_meta_constants() {
        // tables/codebook-l4.meta: level=4, block_size=16x8,
        // canonical_vector_len_bytes=128,
        // canonical_6stage_intra_or_inter_bytes=12288, status=ABSENT.
        // The build script asserts `status == ABSENT` at compile
        // time — here we just sanity-check the scalar fields the
        // generated constant carries forward.
        assert_eq!(SVQ1_L4_ABSENCE.level, 4);
        assert_eq!(SVQ1_L4_ABSENCE.block_size, "16x8");
        assert_eq!(SVQ1_L4_ABSENCE.canonical_vector_len_bytes, 128);
        assert_eq!(SVQ1_L4_ABSENCE.canonical_6stage_intra_or_inter_bytes, 12288);
        // Per-half byte count must equal vector_len * 16 entries * 6 stages.
        assert_eq!(
            SVQ1_L4_ABSENCE.canonical_6stage_intra_or_inter_bytes,
            SVQ1_L4_ABSENCE.canonical_vector_len_bytes
                * SVQ1_ENTRIES_PER_STAGE as u32
                * SVQ1_STAGES_PER_LEVEL as u32
        );
    }

    #[test]
    fn svq1_l5_absence_matches_meta_constants() {
        // tables/codebook-l5.meta: level=5, block_size=16x16,
        // canonical_vector_len_bytes=256,
        // canonical_6stage_intra_or_inter_bytes=24576, status=ABSENT.
        assert_eq!(SVQ1_L5_ABSENCE.level, 5);
        assert_eq!(SVQ1_L5_ABSENCE.block_size, "16x16");
        assert_eq!(SVQ1_L5_ABSENCE.canonical_vector_len_bytes, 256);
        assert_eq!(SVQ1_L5_ABSENCE.canonical_6stage_intra_or_inter_bytes, 24576);
        assert_eq!(
            SVQ1_L5_ABSENCE.canonical_6stage_intra_or_inter_bytes,
            SVQ1_L5_ABSENCE.canonical_vector_len_bytes
                * SVQ1_ENTRIES_PER_STAGE as u32
                * SVQ1_STAGES_PER_LEVEL as u32
        );
    }

    #[test]
    fn absence_record_canonical_sizes_match_vector_length() {
        // The build-asserted `canonical_vector_len_bytes` field of
        // each absence record must agree with the corresponding
        // `Svq1Level::vector_length()` value. This is the property
        // that lets `Svq1Level::absence_record` stand in for the
        // raw meta-file content.
        assert_eq!(
            SVQ1_L4_ABSENCE.canonical_vector_len_bytes,
            Svq1Level::L4.vector_length() as u32
        );
        assert_eq!(
            SVQ1_L5_ABSENCE.canonical_vector_len_bytes,
            Svq1Level::L5.vector_length() as u32
        );
    }

    #[test]
    fn absence_record_accessor_returns_l4_l5_records() {
        // L=4 / L=5 → Some(record); L=0..L=3 → None.
        assert_eq!(Svq1Level::L4.absence_record(), Some(SVQ1_L4_ABSENCE));
        assert_eq!(Svq1Level::L5.absence_record(), Some(SVQ1_L5_ABSENCE));
        assert_eq!(Svq1Level::L0.absence_record(), None);
        assert_eq!(Svq1Level::L1.absence_record(), None);
        assert_eq!(Svq1Level::L2.absence_record(), None);
        assert_eq!(Svq1Level::L3.absence_record(), None);
    }

    #[test]
    fn absence_and_codebook_predicates_are_complementary() {
        // Documented invariant on Svq1Level::absence_record: an L=N
        // for which `absence_record()` returns Some MUST be the same
        // L=N for which `codebook_bytes_per_half()` returns None,
        // and vice versa. This locks the two surfaces together.
        for level in [
            Svq1Level::L0,
            Svq1Level::L1,
            Svq1Level::L2,
            Svq1Level::L3,
            Svq1Level::L4,
            Svq1Level::L5,
        ] {
            assert_eq!(
                level.absence_record().is_some(),
                level.codebook_bytes_per_half().is_none(),
                "absence_record / codebook_bytes_per_half disagree at {level:?}"
            );
        }
    }
}
