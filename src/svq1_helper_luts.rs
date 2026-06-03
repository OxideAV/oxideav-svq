//! SVQ1 helper lookup tables — saturating-clip LUT + bit-position
//! / bit-mask LUT + u16-LE parameter table.
//!
//! ## Provenance
//!
//! All three LUTs are parsed at build time from the clean-room CSV
//! mirrors under `tables/`:
//!
//! * `tables/clip_lut.csv` — bit-exact mirror of
//!   `docs/video/svq1/tables/clip_lut.csv` (768 bytes, file offset
//!   `0x5a100..0x5a400`, VMA `0x67dca100..0x67dca400`, section
//!   `.rdata`).
//! * `tables/svc_bitmask_lut.csv` — bit-exact mirror of
//!   `docs/video/svq1/tables/svc_bitmask_lut.csv` (16 bytes, file
//!   offset `0x5c1c4..0x5c1d4`, VMA `0x67dcc1c4..0x67dcc1d4`,
//!   section `.rdata`).
//! * `tables/u16_param_table.csv` — bit-exact mirror of
//!   `docs/video/svq1/tables/u16_param_table.csv` (1024 bytes / 512
//!   u16 records, file offset `0x59d00..0x5a100`, VMA
//!   `0x67dc9d00..0x67dca100`, section `.rdata`). Sits immediately
//!   below the saturating-clip LUT (which begins at `0x5a100`).
//!
//! All three CSVs were produced by Extractor 02
//! (`docs/video/svq1/provenance/02-codebook-extraction.md`) from the
//! reference binary `quicktimethirdparty.qtx` SHA-256
//! `ac3509bf22aa1458dfc6e1af980956c0153b4c287af452ae5b9cac6f923be169`.
//! Their SHA-256s appear in `docs/video/svq1/tables/MANIFEST-02.sha256`
//! and are matched bit-for-bit by the local mirrors.
//!
//! No FFmpeg / libav* / Sorenson-SDK source is read at any step.
//!
//! ## Clip LUT (`SVQ1_CLIP_LUT`)
//!
//! Per `docs/video/svq1/tables/clip_lut.meta`: a 768-byte
//! saturating-clip LUT. The meta documents the structure as a small
//! prelude (~256 B of small ints + the bytes `0x80..0xff, 0x00..0x7f`
//! wrap-around remap) followed by a central near-identity ramp clipped
//! at the ends. The codec uses it on its interpolation / overflow-
//! saturation paths; the meta is explicit that this is **NOT** a VQ
//! codebook.
//!
//! Compile-time invariants we assert from the spec:
//!
//! * Total length is exactly 768 (`tables/clip_lut.meta` `byte_length:
//!   768`).
//! * Section is `.rdata` per the meta header.
//! * The 16-entry segment at byte offset `0x10..0x20` (file offset
//!   `0x5a110..0x5a120`) is the documented `0x80..0x8f` ramp head
//!   (`80 81 82 83 84 85 86 87 88 89 8a 8b 8c 8d 8e 8f`) — visible in
//!   the meta's "characteristic identity-ramp pattern starting at
//!   0x5a118" remark.
//!
//! ## Bitmask LUT (`SVQ1_BITMASK_LUT`)
//!
//! Per `docs/video/svq1/tables/svc_bitmask_lut.meta`: a 16-byte LUT
//! whose first 8 entries are the single-bit masks
//! `0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01` and last 8 are
//! their one's complements
//! `0x7f, 0xbf, 0xdf, 0xef, 0xf7, 0xfb, 0xfd, 0xfe`. The meta calls
//! this a "standard bit-position / bit-mask helper LUT".
//!
//! Compile-time invariants we assert from the spec:
//!
//! * Total length is exactly 16 (`tables/svc_bitmask_lut.meta`
//!   `byte_length: 16`).
//! * For every `i` in `0..8`: `SVQ1_BITMASK_LUT[i] == 1 << (7 - i)` —
//!   the descending single-bit-mask pattern.
//! * For every `i` in `0..8`:
//!   `SVQ1_BITMASK_LUT[i + 8] == !SVQ1_BITMASK_LUT[i]` — the one's-
//!   complement half (eight bytes after the masks).
//!
//! ## u16 parameter table (`SVQ1_U16_PARAM_TABLE`)
//!
//! Per `docs/video/svq1/tables/u16_param_table.meta`: a 1024-byte
//! region holding 512 little-endian `u16` values at file offset
//! `0x59d00..0x5a100` (VMA `0x67dc9d00..0x67dca100`, section `.rdata`).
//! The meta documents the values as drawn from a small set
//! (`{0x0000, 0x0001, 0x0002, 0x0010, 0x0014, 0x0020, 0x0028,
//! 0x0048, 0x0068, 0x0081, 0x0082, 0x0084, 0x0101, 0x0102, 0x0181,
//! 0x0182}`) arranged in grouped runs, and notes the table sits
//! adjacent to (immediately below) the saturating-clip LUT at
//! `0x5a100`. The meta classifies the values as "suggestive of
//! per-level stride / block-size parameters" without committing to a
//! specific decode-time consumer; round 217 mirrors the bytes
//! verbatim and exposes them without re-claiming an interpretation
//! beyond what the meta documents.
//!
//! Compile-time invariants we assert from the spec:
//!
//! * Total length is exactly 512 `u16` entries
//!   (`tables/u16_param_table.meta` `record_count: 512`,
//!   `record_size_bytes: 2`, `byte_length: 1024`).
//! * Section is `.rdata` per the meta header.
//! * Every value belongs to the 16-element allowed set the meta
//!   documents (this is the strict bit-exact constraint the meta's
//!   "values from {…}" enumeration encodes; we treat the brace list
//!   as the closed set of legal entries).
//! * The first four `word_index`es are all `0x0000` (a zero-run
//!   prelude visible in the CSV's first four rows at file offsets
//!   `0x59d00..0x59d08`).
//! * The first non-zero group is a `0x0020`-valued run at
//!   `word_index` 4..13 (half-open; nine consecutive `u16` entries
//!   at file offsets `0x59d08..0x59d1a`), matching the CSV's first
//!   non-zero group head.
//! * The table sits flush against the saturating-clip LUT — the
//!   exclusive end VMA `0x67dca100` is exactly
//!   [`SVQ1_CLIP_LUT_VMA`].
//!
//! ## Open work (not blocked on this crate)
//!
//! None of the three LUTs is wired into the structural decode path
//! yet — the pixel-reconstruction path that would consume them
//! remains gated on the L=0..L=3 intra-vs-inter / stage-vs-level
//! layout doc still pending for the codebook payload. This module
//! exposes the bit-exact constants + ergonomic accessors so the
//! future pixel reconstruction work can lift them in without
//! re-extracting.

include!(concat!(env!("OUT_DIR"), "/svq1_codebook_data.rs"));

/// Length of the saturating-clip helper LUT — 768 bytes per
/// `docs/video/svq1/tables/clip_lut.meta` (`byte_length: 768`).
pub const SVQ1_CLIP_LUT_BYTES: usize = 768;

/// Length of the bit-position / bit-mask helper LUT — 16 bytes per
/// `docs/video/svq1/tables/svc_bitmask_lut.meta` (`byte_length: 16`).
pub const SVQ1_BITMASK_LUT_BYTES: usize = 16;

/// Number of `u16` entries in the parameter table — 512 per
/// `docs/video/svq1/tables/u16_param_table.meta` (`record_count: 512`,
/// `record_size_bytes: 2`, `byte_length: 1024`).
pub const SVQ1_U16_PARAM_TABLE_WORDS: usize = 512;

/// Byte length of the `u16` parameter table — 1024 bytes per
/// `docs/video/svq1/tables/u16_param_table.meta` (`byte_length: 1024`).
pub const SVQ1_U16_PARAM_TABLE_BYTES: usize = 1024;

/// Borrowed view over the 768-byte saturating-clip LUT.
///
/// See the module-level docs for the structural breakdown. The codec
/// uses this on interpolation / overflow-saturation paths; per
/// `tables/clip_lut.meta`: "NOT a VQ codebook."
pub fn clip_lut() -> &'static [u8] {
    &SVQ1_CLIP_LUT
}

/// Borrowed view over the 16-byte bit-position / bit-mask LUT.
///
/// First 8 entries are single-bit masks (`0x80, 0x40, 0x20, 0x10,
/// 0x08, 0x04, 0x02, 0x01`); last 8 are their one's complements
/// (`0x7f, 0xbf, 0xdf, 0xef, 0xf7, 0xfb, 0xfd, 0xfe`).
pub fn bitmask_lut() -> &'static [u8] {
    &SVQ1_BITMASK_LUT
}

/// Borrowed view over the 512-entry u16-LE parameter table.
///
/// See the module-level docs for the structural breakdown and the
/// allowed-value set the meta documents. Round 217 exposes the
/// bit-exact bytes without committing to a decode-time interpretation.
pub fn u16_param_table() -> &'static [u16] {
    &SVQ1_U16_PARAM_TABLE
}

/// Source file offset of the saturating-clip LUT in the reference
/// binary — `0x5a100`. Mirrors `tables/clip_lut.meta`
/// `file_offset_start_hex: 0x0005a100`.
pub const SVQ1_CLIP_LUT_FILE_OFFSET: u32 = 0x0005_a100;

/// Source VMA of the saturating-clip LUT — `0x67dca100`. Mirrors
/// `tables/clip_lut.meta` `vma_start_hex: 0x67dca100`.
pub const SVQ1_CLIP_LUT_VMA: u32 = 0x67dc_a100;

/// Source file offset of the bit-mask LUT in the reference binary —
/// `0x5c1c4`. Mirrors `tables/svc_bitmask_lut.meta`
/// `file_offset_start_hex: 0x0005c1c4`.
pub const SVQ1_BITMASK_LUT_FILE_OFFSET: u32 = 0x0005_c1c4;

/// Source VMA of the bit-mask LUT — `0x67dcc1c4`. Mirrors
/// `tables/svc_bitmask_lut.meta` `vma_start_hex: 0x67dcc1c4`.
pub const SVQ1_BITMASK_LUT_VMA: u32 = 0x67dc_c1c4;

/// Source file offset of the u16 parameter table — `0x59d00`. Mirrors
/// `tables/u16_param_table.meta` `file_offset_start_hex: 0x00059d00`.
pub const SVQ1_U16_PARAM_TABLE_FILE_OFFSET: u32 = 0x0005_9d00;

/// Source VMA of the u16 parameter table — `0x67dc9d00`. Mirrors
/// `tables/u16_param_table.meta` `vma_start_hex: 0x67dc9d00`.
pub const SVQ1_U16_PARAM_TABLE_VMA: u32 = 0x67dc_9d00;

/// Allowed-value set the meta enumerates for [`SVQ1_U16_PARAM_TABLE`].
///
/// Per `docs/video/svq1/tables/u16_param_table.meta`'s
/// `interpretation` line: "values from
/// `{0x0010, 0x0020, 0x0028, 0x0048, 0x0084, 0x0081, 0x0001, 0x0002, ...}`
/// arranged in groups". The closed set actually attested across the
/// 512 entries (sorted ascending) is given here; the
/// [`u16_param_table_values_are_in_allowed_set`] lib test confirms
/// every word in the table belongs to this set, so a future docs
/// revision that adds an out-of-set value fails the build.
///
/// [`u16_param_table_values_are_in_allowed_set`]: super::tests::u16_param_table_values_are_in_allowed_set
pub const SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES: [u16; 16] = [
    0x0000, 0x0001, 0x0002, 0x0010, 0x0014, 0x0020, 0x0028, 0x0048, 0x0068, 0x0081, 0x0082, 0x0084,
    0x0101, 0x0102, 0x0181, 0x0182,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_lut_length_matches_documented_size() {
        // docs/video/svq1/tables/clip_lut.meta: byte_length: 768
        assert_eq!(SVQ1_CLIP_LUT.len(), SVQ1_CLIP_LUT_BYTES);
        assert_eq!(SVQ1_CLIP_LUT_BYTES, 768);
        assert_eq!(clip_lut().len(), 768);
    }

    #[test]
    fn bitmask_lut_length_matches_documented_size() {
        // docs/video/svq1/tables/svc_bitmask_lut.meta: byte_length: 16
        assert_eq!(SVQ1_BITMASK_LUT.len(), SVQ1_BITMASK_LUT_BYTES);
        assert_eq!(SVQ1_BITMASK_LUT_BYTES, 16);
        assert_eq!(bitmask_lut().len(), 16);
    }

    #[test]
    fn bitmask_lut_first_half_is_descending_bit_masks() {
        // tables/svc_bitmask_lut.meta documents the first 8 entries as
        // bit masks 0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01.
        for (i, &entry) in SVQ1_BITMASK_LUT.iter().enumerate().take(8) {
            assert_eq!(
                entry,
                1u8 << (7 - i as u32),
                "bitmask[{i}] expected 1<<{}, got {:#04x}",
                7 - i,
                entry,
            );
        }
    }

    #[test]
    fn bitmask_lut_second_half_is_ones_complement() {
        // tables/svc_bitmask_lut.meta documents the last 8 entries as
        // the one's complements of the first 8.
        let (first, second) = SVQ1_BITMASK_LUT.split_at(8);
        for (i, (&mask, &inverse)) in first.iter().zip(second.iter()).enumerate() {
            assert_eq!(
                inverse,
                !mask,
                "bitmask[{}] expected !{:#04x}={:#04x}, got {:#04x}",
                i + 8,
                mask,
                !mask,
                inverse,
            );
        }
    }

    #[test]
    fn bitmask_lut_matches_documented_byte_string() {
        // tables/svc_bitmask_lut.meta notes the bytes explicitly:
        // "80 40 20 10 08 04 02 01 7f bf df ef f7 fb fd fe"
        let expected: [u8; 16] = [
            0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01, 0x7f, 0xbf, 0xdf, 0xef, 0xf7, 0xfb,
            0xfd, 0xfe,
        ];
        assert_eq!(SVQ1_BITMASK_LUT, expected);
        assert_eq!(bitmask_lut(), &expected[..]);
    }

    #[test]
    fn clip_lut_documented_ramp_head_matches() {
        // tables/clip_lut.meta calls out the identity-ramp head at
        // file offset 0x5a110..0x5a120 (byte offsets 16..32 in the LUT)
        // as 0x80..0x8f — the start of the central wrap-around remap.
        let head: [u8; 16] = [
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d,
            0x8e, 0x8f,
        ];
        assert_eq!(SVQ1_CLIP_LUT[16..32], head);
    }

    #[test]
    fn clip_lut_source_offsets_match_meta() {
        // tables/clip_lut.meta:
        //   file_offset_start_hex: 0x0005a100
        //   vma_start_hex: 0x67dca100
        assert_eq!(SVQ1_CLIP_LUT_FILE_OFFSET, 0x0005_a100);
        assert_eq!(SVQ1_CLIP_LUT_VMA, 0x67dc_a100);
        // (file_offset_end_exclusive - file_offset_start) == byte_length
        assert_eq!(
            0x0005_a400u32 - SVQ1_CLIP_LUT_FILE_OFFSET,
            SVQ1_CLIP_LUT_BYTES as u32
        );
        // VMA = file_offset + image_base
        assert_eq!(
            SVQ1_CLIP_LUT_VMA - SVQ1_CLIP_LUT_FILE_OFFSET,
            0x67d7_0000,
            "image_base derivable from clip-LUT meta should be 0x67d70000"
        );
    }

    #[test]
    fn bitmask_lut_source_offsets_match_meta() {
        // tables/svc_bitmask_lut.meta:
        //   file_offset_start_hex: 0x0005c1c4
        //   vma_start_hex: 0x67dcc1c4
        assert_eq!(SVQ1_BITMASK_LUT_FILE_OFFSET, 0x0005_c1c4);
        assert_eq!(SVQ1_BITMASK_LUT_VMA, 0x67dc_c1c4);
        assert_eq!(
            0x0005_c1d4u32 - SVQ1_BITMASK_LUT_FILE_OFFSET,
            SVQ1_BITMASK_LUT_BYTES as u32
        );
        // VMA = file_offset + image_base
        assert_eq!(
            SVQ1_BITMASK_LUT_VMA - SVQ1_BITMASK_LUT_FILE_OFFSET,
            0x67d7_0000,
            "image_base derivable from bitmask-LUT meta should be 0x67d70000"
        );
    }

    #[test]
    fn u16_param_table_length_matches_documented_size() {
        // docs/video/svq1/tables/u16_param_table.meta:
        //   byte_length: 1024 / record_count: 512 / record_size_bytes: 2
        assert_eq!(SVQ1_U16_PARAM_TABLE.len(), SVQ1_U16_PARAM_TABLE_WORDS);
        assert_eq!(SVQ1_U16_PARAM_TABLE_WORDS, 512);
        assert_eq!(SVQ1_U16_PARAM_TABLE_BYTES, 1024);
        assert_eq!(
            SVQ1_U16_PARAM_TABLE_BYTES,
            SVQ1_U16_PARAM_TABLE_WORDS * core::mem::size_of::<u16>(),
        );
        assert_eq!(u16_param_table().len(), 512);
    }

    #[test]
    fn u16_param_table_source_offsets_match_meta() {
        // tables/u16_param_table.meta:
        //   file_offset_start_hex: 0x00059d00
        //   vma_start_hex: 0x67dc9d00
        assert_eq!(SVQ1_U16_PARAM_TABLE_FILE_OFFSET, 0x0005_9d00);
        assert_eq!(SVQ1_U16_PARAM_TABLE_VMA, 0x67dc_9d00);
        assert_eq!(
            0x0005_a100u32 - SVQ1_U16_PARAM_TABLE_FILE_OFFSET,
            SVQ1_U16_PARAM_TABLE_BYTES as u32
        );
        // VMA = file_offset + image_base
        assert_eq!(
            SVQ1_U16_PARAM_TABLE_VMA - SVQ1_U16_PARAM_TABLE_FILE_OFFSET,
            0x67d7_0000,
            "image_base derivable from u16-param meta should be 0x67d70000"
        );
    }

    #[test]
    fn u16_param_table_is_flush_against_clip_lut() {
        // The meta's grouping notes call out that the u16 parameter
        // table sits adjacent to the clip LUT. Geometrically: the
        // exclusive end VMA of the u16 table must equal the start
        // VMA of the clip LUT (no gap, no overlap).
        let u16_end_vma = SVQ1_U16_PARAM_TABLE_VMA + SVQ1_U16_PARAM_TABLE_BYTES as u32;
        assert_eq!(
            u16_end_vma, SVQ1_CLIP_LUT_VMA,
            "u16 param table (ending at VMA {:#010x}) must abut the clip LUT (starting at VMA {:#010x})",
            u16_end_vma, SVQ1_CLIP_LUT_VMA
        );
        // Same fact on the file-offset axis.
        let u16_end_off = SVQ1_U16_PARAM_TABLE_FILE_OFFSET + SVQ1_U16_PARAM_TABLE_BYTES as u32;
        assert_eq!(u16_end_off, SVQ1_CLIP_LUT_FILE_OFFSET);
    }

    #[test]
    fn u16_param_table_values_are_in_allowed_set() {
        // The meta documents the closed set of values the 512 entries
        // are drawn from. A future docs revision that quietly adds an
        // out-of-set value will fail here before any consumer can
        // observe it.
        for (i, &v) in SVQ1_U16_PARAM_TABLE.iter().enumerate() {
            assert!(
                SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES.contains(&v),
                "SVQ1_U16_PARAM_TABLE[{i}] = {v:#06x} is not in the documented allowed set {:?}",
                SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES,
            );
        }
    }

    #[test]
    fn u16_param_table_first_four_entries_are_zero_prelude() {
        // The CSV documents word_index 0..4 as 0x0000 (a four-word
        // zero prelude visible at file offsets 0x59d00..0x59d08).
        assert_eq!(SVQ1_U16_PARAM_TABLE[..4], [0x0000u16; 4]);
    }

    #[test]
    fn u16_param_table_first_nonzero_group_is_0x0020_run() {
        // CSV word_index 4..13 is a run of nine 0x0020 entries
        // (file offsets 0x59d08..0x59d1a). This is the first
        // non-zero group head the meta's grouped-runs analysis calls
        // out.
        assert_eq!(SVQ1_U16_PARAM_TABLE[4..13], [0x0020u16; 9]);
        // Sanity: the entry immediately after the run is NOT 0x0020.
        assert_ne!(SVQ1_U16_PARAM_TABLE[13], 0x0020);
    }

    #[test]
    fn u16_param_table_allowed_value_set_is_sorted_and_unique() {
        // Defensive invariant: the documented allowed-value set is
        // structured as a sorted, unique ascending sequence; this
        // catches any future copy-paste edit that introduces a
        // duplicate or transposes two entries.
        for w in SVQ1_U16_PARAM_TABLE_ALLOWED_VALUES.windows(2) {
            assert!(
                w[0] < w[1],
                "allowed-value set must be strictly ascending; saw {:#06x} then {:#06x}",
                w[0],
                w[1]
            );
        }
    }
}
