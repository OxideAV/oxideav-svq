//! Fuzz the SVQ3 access-unit decoder end to end
//! (`Svq3PictureDecoder::decode_access_unit`): a fuzz-derived but
//! structurally valid `SEQH` (geometry + motion-precision flags), then
//! a chain of access units cut from the remaining bytes — I pictures
//! (with and without the spec/09 edge filter), P pictures against the
//! previous picture (skip copies, every partition shape, sub-pel motion
//! compensation, the uncoded tail copy), intra macroblocks inside P
//! slices — must either produce a picture of the right geometry or
//! error cleanly (no panics, no overflow, no out-of-range indexing).

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq3::{num_macroblocks, parse_extradata, Svq3SequenceHeader};
use oxideav_svq::{Svq3DecodeOptions, Svq3PictureDecoder};

/// Build a structurally valid explicit-dimension SEQH from two fuzz
/// bytes: dimensions 16…128 (both axes), the half-/third-pel enables
/// from the high bits.
fn seqh_from(w_byte: u8, h_byte: u8) -> Svq3SequenceHeader {
    let width = 16 + ((w_byte as u32) % 8) * 16;
    let height = 16 + ((h_byte as u32) % 8) * 16;
    let mut bits: Vec<u8> = Vec::new();
    let mut push = |width: u32, value: u32| {
        for i in (0..width).rev() {
            bits.push(((value >> i) & 1) as u8);
        }
    };
    push(3, 7);
    push(12, width);
    push(12, height);
    push(1, (w_byte >> 6) as u32 & 1); // halfpel
    push(1, (h_byte >> 6) as u32 & 1); // thirdpel
    push(1, 0); // postfilter hint
    push(1, 0); // extended mode
    push(2, 0b11);
    push(1, 1); // no B frames
    push(1, 0);
    push(1, 0); // no escape bytes
    push(1, 0); // not protected
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
    if data.len() < 3 {
        return;
    }
    let seqh = seqh_from(data[0], data[1]);
    let mbs = num_macroblocks(&seqh) as usize;
    let mut dec = Svq3PictureDecoder::new(seqh)
        .expect("non-empty grid")
        .with_options(Svq3DecodeOptions {
            intra_edge_filter: data[2] & 1 != 0,
        });
    // Cut the rest into access units: each is prefixed by one length
    // byte (0 = the remainder).
    let mut rest = &data[3..];
    let mut rounds = 0;
    while !rest.is_empty() && rounds < 6 {
        let len = rest[0] as usize;
        let body = &rest[1..];
        let (au, tail) = if len == 0 || len >= body.len() {
            (body, &body[body.len()..])
        } else {
            body.split_at(len)
        };
        if let Ok(decoded) = dec.decode_access_unit(au) {
            assert_eq!(decoded.picture.luma().len(), mbs * 256);
            assert_eq!(decoded.picture.cb().len(), mbs * 64);
            assert_eq!(decoded.picture.cr().len(), mbs * 64);
            // The reference is what was just decoded.
            assert_eq!(
                dec.reference().map(|r| r.luma()),
                Some(decoded.picture.luma())
            );
        }
        rest = tail;
        rounds += 1;
    }
});
