//! SVQ1 helper lookup tables — saturating-clip LUT + bit-position
//! / bit-mask LUT.
//!
//! ## Provenance
//!
//! Both LUTs are parsed at build time from the clean-room CSV mirrors
//! under `tables/`:
//!
//! * `tables/clip_lut.csv` — bit-exact mirror of
//!   `docs/video/svq1/tables/clip_lut.csv` (768 bytes, file offset
//!   `0x5a100..0x5a400`, VMA `0x67dca100..0x67dca400`, section
//!   `.rdata`).
//! * `tables/svc_bitmask_lut.csv` — bit-exact mirror of
//!   `docs/video/svq1/tables/svc_bitmask_lut.csv` (16 bytes, file
//!   offset `0x5c1c4..0x5c1d4`, VMA `0x67dcc1c4..0x67dcc1d4`,
//!   section `.rdata`).
//!
//! The two CSVs were produced by Extractor 02
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
//! ## Open work (not blocked on this crate)
//!
//! Neither LUT is wired into the structural decode path yet — the
//! pixel-reconstruction path that would consume them remains gated on
//! the L=0..L=3 intra-vs-inter / stage-vs-level layout doc still
//! pending for the codebook payload. This module exposes the bit-exact
//! constants + ergonomic accessors so the future pixel reconstruction
//! work can lift them in without re-extracting.

include!(concat!(env!("OUT_DIR"), "/svq1_codebook_data.rs"));

/// Length of the saturating-clip helper LUT — 768 bytes per
/// `docs/video/svq1/tables/clip_lut.meta` (`byte_length: 768`).
pub const SVQ1_CLIP_LUT_BYTES: usize = 768;

/// Length of the bit-position / bit-mask helper LUT — 16 bytes per
/// `docs/video/svq1/tables/svc_bitmask_lut.meta` (`byte_length: 16`).
pub const SVQ1_BITMASK_LUT_BYTES: usize = 16;

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
}
