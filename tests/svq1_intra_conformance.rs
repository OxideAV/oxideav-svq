//! SVQ1 intra-frame conformance — black-box validation fixture.
//!
//! `tests/fixtures/intra_176x144.svq1` is a single SVQ1 I-frame
//! produced by a reference encoder binary used strictly as a
//! black box (opaque input → opaque output; no source consulted),
//! from a synthetic 176×144 test pattern.
//! `tests/fixtures/intra_176x144.yuv410p` is the same binary's own
//! decode of that frame to raw planar YUV 4:1:0 (Y 176×144, then
//! U 44×36, then V 44×36 — 28 512 bytes).
//!
//! Decoding the frame byte-exact pins, in the Validator role the
//! clean-room docs call for:
//!
//! * the §14.8 codebook-payload layout — **Hypothesis A**
//!   (intra-first, level-ascending) of
//!   `docs/video/svq1/spec/14-codebook-architecture.md` §14.8, as
//!   implemented by `svq1_codebook::half_byte_offset_in_payload`;
//! * the VLC record reading (record index = alphabet position,
//!   value = MSB-first codeword) of
//!   `docs/video/svq1/provenance/07-extractor-vlc-tables.md` §5.2;
//! * the stage-count mapping `N = position − 1` of
//!   `docs/video/svq1/spec/04-multistage-vq-decoder.md` §4.1
//!   (audit-corrected);
//! * the breadth-first walk + subdivision geometry of
//!   `docs/video/svq1/spec/03-block-hierarchy.md` §3.4 / §3.5.

use oxideav_svq::svq1_plane::decode_intra_frame;

const FRAME: &[u8] = include_bytes!("fixtures/intra_176x144.svq1");
const REF_YUV: &[u8] = include_bytes!("fixtures/intra_176x144.yuv410p");

const W: usize = 176;
const H: usize = 144;
const CW: usize = 44;
const CH: usize = 36;

#[test]
fn intra_frame_decodes_byte_exact_against_reference_decode() {
    assert_eq!(REF_YUV.len(), W * H + 2 * CW * CH);

    let frame = decode_intra_frame(FRAME).expect("I-frame decodes");
    assert_eq!(frame.width(), W);
    assert_eq!(frame.height(), H);
    assert_eq!(frame.u.width, CW);
    assert_eq!(frame.u.height, CH);

    let y = frame.y.visible();
    let u = frame.u.visible();
    let v = frame.v.visible();

    let (ref_y, rest) = REF_YUV.split_at(W * H);
    let (ref_u, ref_v) = rest.split_at(CW * CH);

    // Byte-exact per plane. Report the first mismatch coordinate on
    // failure for debuggability.
    for (name, got, want, pw) in [
        ("Y", &y, ref_y, W),
        ("U", &u, ref_u, CW),
        ("V", &v, ref_v, CW),
    ] {
        for (i, (g, w_)) in got.iter().zip(want.iter()).enumerate() {
            assert_eq!(
                g,
                w_,
                "{name} plane first mismatch at ({}, {}): got {g}, reference {w_}",
                i % pw,
                i / pw
            );
        }
        assert_eq!(got.len(), want.len(), "{name} plane length");
    }
}

#[test]
fn intra_frame_header_fields() {
    let frame = decode_intra_frame(FRAME).expect("I-frame decodes");
    assert!(frame.header.is_intra());
    // 176×144 is frame-size code 2 in the wiki's common-size table
    // (docs/video/svq1/wiki/Sorenson_Video_1.wiki §"Stream Format
    // And Header") — but an encoder may also use the explicit
    // escape; accept either as long as the dimensions land.
    assert_eq!(frame.header.width, Some(176));
    assert_eq!(frame.header.height, Some(144));
}
