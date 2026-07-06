//! SVQ1 INTER_4MV conformance — 25-frame independently-minted fixture.
//!
//! `tests/fixtures/inter4mv_176x144_25f.svq1` is the concatenated raw
//! SVQ1 frame payload of `docs/video/svq1/fixtures/inter-4mv/`
//! (docs commit `f210f08`): 176×144 (QCIF — exactly 11×9 luma
//! macroblocks, no edge overhang), yuv410p, one I-frame + 24
//! P-frames, minted with an INDEPENDENT black-box SVQ1 encoder whose
//! rate-distortion search provably selects `INTER_4MV` (the common
//! reference encoder never emits the mode — see the fixture's
//! differential proof: the `+mv4` / `-mv4` control pair shares a
//! byte-identical I-frame yet every P-frame differs, and 12/12
//! P-frames of a `-g 2` control carry the mode).
//! `inter4mv_176x144_25f.yuv410p` is that toolchain's own black-box
//! decode (25 × 28 512 B, verified 0 decoder errors).
//!
//! Decoding all 25 frames byte-exact — each P against OUR previous
//! reconstruction, so any divergence cascades and cannot hide —
//! pins, in the Validator role:
//!
//! * the `INTER_4MV` decode path against real inter-stream data for
//!   the first time (the mode was previously validated only against
//!   our own encoder's output): four positional T02 differentials on
//!   the wire, strictly serial §6.4.5 predict/store interleave, §6.8.1
//!   sub-block MV cache stores, per-8×8 half-pel MC;
//! * cross-encoder generality of the audit/01 §7.1 T03 mode
//!   permutation, spec/06 §6.2.3 Reading B, and the §6.4 median MV
//!   predictor — this stream comes from a different encoder family
//!   than every prior fixture;
//! * the spec/06 §6.7 / §6.7.4 (#174) edge law FOR THIS ORACLE
//!   FAMILY: despite the fixture's in-frame *content* construction,
//!   its right-edge macroblocks decode MVs whose halfpel MC footprint
//!   exits the visible frame (e.g. frame 1 MBs (4,10) / (5,10),
//!   mv = (+4, −8) at x0 = 160 of 176). Byte-exactness across all 24
//!   chained P-frames pins the **reference-window MV clamp** —
//!   `svq1_plane::clamp_mv_to_reference_window`: the MC read is
//!   clamped to the PADDED canvas window while the §6.8 MV cache
//!   keeps the unclamped vector — and, via the chroma planes (44×36
//!   visible in a 48×48 padded canvas), that the §4.7.3 overhang
//!   region is decoded, stored, and readable as reference samples.
//!   The three rival readings (edge replication with no clamp,
//!   visible-window clamp, clamped cache stores) each diverge on this
//!   stream and are excluded empirically.

use oxideav_svq::svq1_plane::{decode_frame, decode_intra_frame, Svq1DecodedFrame};

const FRAMES: &[u8] = include_bytes!("fixtures/inter4mv_176x144_25f.svq1");
const REF_YUV: &[u8] = include_bytes!("fixtures/inter4mv_176x144_25f.yuv410p");

const W: usize = 176;
const H: usize = 144;
const CW: usize = 44;
const CH: usize = 36;
const FRAME_YUV: usize = W * H + 2 * CW * CH;

/// Per-frame byte sizes from the fixture's `frame_sizes.tsv`
/// (frame 0 is the I-frame; 1..=24 are P-frames).
const FRAME_SIZES: [usize; 25] = [
    2432, 1792, 1872, 1624, 1816, 2148, 2324, 1948, 1892, 2048, 1828, 2564, 2404, 1864, 1940, 1720,
    1332, 1388, 2208, 2024, 1832, 1588, 1504, 1376, 1896,
];

fn assert_frame_matches(n: usize, frame: &Svq1DecodedFrame, want: &[u8]) {
    assert_eq!(frame.width(), W, "frame {n} width");
    assert_eq!(frame.height(), H, "frame {n} height");

    let y = frame.y.visible();
    let u = frame.u.visible();
    let v = frame.v.visible();

    let (want_y, rest) = want.split_at(W * H);
    let (want_u, want_v) = rest.split_at(CW * CH);

    for (plane, got, want, pw) in [
        ("Y", &y, want_y, W),
        ("U", &u, want_u, CW),
        ("V", &v, want_v, CW),
    ] {
        let mismatches = got.iter().zip(want.iter()).filter(|(g, w)| g != w).count();
        if mismatches != 0 {
            let (i, (g, w)) = got
                .iter()
                .zip(want.iter())
                .enumerate()
                .find(|(_, (g, w))| g != w)
                .unwrap();
            panic!(
                "frame {n} plane {plane}: {mismatches} mismatching samples; \
                 first at ({}, {}): got {g}, want {w}",
                i % pw,
                i / pw,
            );
        }
    }
}

#[test]
fn inter4mv_25_frame_chain_decodes_byte_exact() {
    assert_eq!(FRAME_SIZES.iter().sum::<usize>(), FRAMES.len());
    assert_eq!(REF_YUV.len(), 25 * FRAME_YUV);

    let mut offset = 0usize;
    let mut reference: Option<Svq1DecodedFrame> = None;
    for (n, &size) in FRAME_SIZES.iter().enumerate() {
        let bytes = &FRAMES[offset..offset + size];
        offset += size;
        let want = &REF_YUV[n * FRAME_YUV..(n + 1) * FRAME_YUV];

        let frame = if n == 0 {
            let frame = decode_intra_frame(bytes).expect("I-frame decodes");
            assert!(frame.header.is_intra(), "frame 0 must be intra");
            frame
        } else {
            let frame = decode_frame(bytes, reference.as_ref()).unwrap_or_else(|e| {
                panic!("P-frame {n} fails to decode: {e:?}");
            });
            assert!(!frame.header.is_intra(), "frame {n} must be inter");
            frame
        };

        assert_frame_matches(n, &frame, want);
        reference = Some(frame);
    }
}
