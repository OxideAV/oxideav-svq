//! SVQ1 inter-frame (P) conformance — black-box validation fixture.
//!
//! `tests/fixtures/inter_176x144_p.svq1` is the SECOND frame (a
//! P-frame) of a two-frame sequence produced by a reference encoder
//! binary used strictly as a black box, whose FIRST frame is
//! byte-identical to `tests/fixtures/intra_176x144.svq1` (verified
//! at fixture-generation time). `tests/fixtures/inter_176x144_p.yuv410p`
//! is the same binary's own decode of that second frame to raw
//! planar YUV 4:1:0.
//!
//! Decoding the P-frame byte-exact against the reference decode —
//! with our own decode of the first frame as the reference picture —
//! pins, in the Validator role the clean-room docs call for:
//!
//! * spec/06 §6.2.3 **Reading B** for the T02 motion-vector
//!   component wire format (single signed codeword, `position − 32`;
//!   no separate sign bit);
//! * the audit/01 §7.1 T03 mode permutation (**rotated against the
//!   wiki numbering**: position 3 = SKIP, 0 = INTER, 1 = INTER_4MV,
//!   2 = INTRA — SKIP takes the 1-bit codeword);
//! * the §6.4 median MV predictor + §6.6 clip + §6.8 per-plane MV
//!   cache and the §6.5 half-pel interpolator
//!   (`(a + b + 1) >> 1` rounding) as wired by `svq1_mc` /
//!   `svq1_mv_cache`.

use oxideav_svq::svq1_plane::{decode_frame, decode_intra_frame};

const I_FRAME: &[u8] = include_bytes!("fixtures/intra_176x144.svq1");
const P_FRAME: &[u8] = include_bytes!("fixtures/inter_176x144_p.svq1");
const REF_YUV: &[u8] = include_bytes!("fixtures/inter_176x144_p.yuv410p");

const W: usize = 176;
const H: usize = 144;
const CW: usize = 44;
const CH: usize = 36;

#[test]
fn p_frame_decodes_byte_exact_against_reference_decode() {
    assert_eq!(REF_YUV.len(), W * H + 2 * CW * CH);

    let reference = decode_intra_frame(I_FRAME).expect("I-frame decodes");
    let frame = decode_frame(P_FRAME, Some(&reference)).expect("P-frame decodes");
    assert!(!frame.header.is_intra());
    assert_eq!(frame.width(), W);
    assert_eq!(frame.height(), H);

    let y = frame.y.visible();
    let u = frame.u.visible();
    let v = frame.v.visible();

    let (ref_y, rest) = REF_YUV.split_at(W * H);
    let (ref_u, ref_v) = rest.split_at(CW * CH);

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
fn p_frame_without_reference_is_rejected() {
    assert!(matches!(
        decode_frame(P_FRAME, None),
        Err(oxideav_svq::Error::MissingReference)
    ));
}

/// Six-frame chain fixture (I + 5 P) from a different synthetic
/// source at a coarser quantiser, reference-decoded frame by frame:
/// exercises multi-frame reference chaining (each P predicts from
/// OUR previous reconstruction), a mode mix of SKIP + INTER + INTRA
/// macroblocks, and a broad half-pel MV spread. Every frame must be
/// byte-exact, so any drift in the reconstruction loop compounds and
/// is caught at the frame where it first appears.
#[test]
fn six_frame_chain_decodes_byte_exact() {
    const CHAIN: &[u8] = include_bytes!("fixtures/chain_176x144_6f.svq1");
    const CHAIN_REF: &[u8] = include_bytes!("fixtures/chain_176x144_6f.yuv410p");
    /// Per-frame packet byte sizes, recorded at fixture-generation
    /// time from the container's sample table.
    const SIZES: [usize; 6] = [3476, 2076, 1512, 1736, 1632, 1868];
    const FRAME_YUV: usize = W * H + 2 * CW * CH;

    assert_eq!(CHAIN.len(), SIZES.iter().sum::<usize>());
    assert_eq!(CHAIN_REF.len(), 6 * FRAME_YUV);

    let mut offset = 0usize;
    let mut reference: Option<oxideav_svq::svq1_plane::Svq1DecodedFrame> = None;
    for (frame_no, &size) in SIZES.iter().enumerate() {
        let chunk = &CHAIN[offset..offset + size];
        offset += size;
        let frame = decode_frame(chunk, reference.as_ref())
            .unwrap_or_else(|e| panic!("frame {frame_no} decodes: {e:?}"));
        assert_eq!(frame.header.is_intra(), frame_no == 0);

        let want = &CHAIN_REF[frame_no * FRAME_YUV..(frame_no + 1) * FRAME_YUV];
        let (want_y, rest) = want.split_at(W * H);
        let (want_u, want_v) = rest.split_at(CW * CH);
        assert_eq!(frame.y.visible(), want_y, "frame {frame_no} Y plane");
        assert_eq!(frame.u.visible(), want_u, "frame {frame_no} U plane");
        assert_eq!(frame.v.visible(), want_v, "frame {frame_no} V plane");

        reference = Some(frame);
    }
}
