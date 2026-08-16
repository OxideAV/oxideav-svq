//! Fuzz the SVQ3 intra access-unit frame walk
//! (`svq3_frame::decode_intra_access_unit`): arbitrary access-unit
//! bytes against a fuzz-derived (but structurally valid) sequence
//! header — the slice envelope walk, the per-macroblock type dispatch,
//! the intra-4×4 / intra-16×16 / separate-DC grammars, the residual
//! decoders and the whole reconstruction composition must either
//! produce a picture or error cleanly (no panics, no loops, no
//! out-of-range indexing).

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq3::{num_macroblocks, parse_extradata, Svq3SequenceHeader};
use oxideav_svq::svq3_frame::decode_intra_access_unit;

/// Build a structurally valid explicit-dimension SEQH from two fuzz
/// bytes (dimensions clamped to keep the macroblock grid small enough
/// for a bounded run).
fn seqh_from(w_byte: u8, h_byte: u8) -> Svq3SequenceHeader {
    // Both dimensions span 16..=128.
    let width = 16 + ((w_byte as u32) % 8) * 16;
    let height = 16 + ((h_byte as u32) % 8) * 16;
    // Pack: 3-bit code 7, 12-bit width, 12-bit height, 2 MV flags,
    // 4 unknown bits, no-B flag, optional-loop stop, protected 0 —
    // 35 bits.
    let mut bits: Vec<u8> = Vec::new();
    let mut push = |width: u32, value: u32| {
        for i in (0..width).rev() {
            bits.push(((value >> i) & 1) as u8);
        }
    };
    push(3, 7);
    push(12, width);
    push(12, height);
    push(1, (w_byte >> 6) as u32 & 1);
    push(1, (h_byte >> 6) as u32 & 1);
    push(4, 0);
    push(1, 1);
    push(1, 0);
    push(1, 0);
    let mut payload = vec![0u8; bits.len().div_ceil(8)];
    for (i, &b) in bits.iter().enumerate() {
        payload[i / 8] |= b << (7 - (i % 8));
    }
    let mut extradata = Vec::new();
    extradata.extend_from_slice(b"SEQH");
    extradata.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    extradata.extend_from_slice(&payload);
    parse_extradata(&extradata).expect("constructed SEQH parses")
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let seqh = seqh_from(data[0], data[1]);
    let au = &data[2..];
    if let Ok(picture) = decode_intra_access_unit(&seqh, au) {
        // Structural invariant: the canvas matches the macroblock grid.
        let mbs = num_macroblocks(&seqh) as usize;
        assert_eq!(
            picture.luma().len(),
            mbs * 16 * 16,
            "luma canvas covers the macroblock grid"
        );
        assert_eq!(picture.cb().len(), mbs * 8 * 8);
        assert_eq!(picture.cr().len(), mbs * 8 * 8);
    }
});
