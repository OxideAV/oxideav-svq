//! SVQ1 inter-encoder conformance — black-box cross-validation of
//! the P-frame path, including INTER_4MV.
//!
//! `tests/fixtures/enc_4mv_176x144_4f.svq1` is OUR encoder's
//! deterministic output for the synthetic sequence below (an adaptive
//! intra frame followed by three P-frames over quadrant-divergent
//! motion, which drives the mode decision into INTER_4MV — verified
//! at fixture-generation time by comparing against an
//! `allow_4mv = false` encode, which produces a ~5× larger stream).
//! `tests/fixtures/enc_4mv_176x144_4f.yuv410p` is the reference
//! decoder binary's decode of that chain (black-box only; the frames
//! were wrapped in a minimal container for delivery), which matched
//! OUR decoder's output byte-exact at fixture-generation time.
//!
//! This is the first INTER_4MV stream validated on the wire in
//! either direction — the reference ENCODER binary never emits the
//! mode, so the decoder-side INTER_4MV path (spec/06 §6.4.4 serial
//! predictors, per-sub-block MC of spec/04 §4.6.3) had no
//! real-stream fixture until now.
//!
//! The tests pin (1) encoder determinism against the committed
//! stream and (2) our decode of that stream against the committed
//! reference decode — a regression on either side of the wire breaks
//! CI.

use oxideav_svq::svq1_enc::{encode_intra_frame, Svq1EncoderMode, Svq1PlaneRef};
use oxideav_svq::svq1_enc_inter::{encode_inter_frame, Svq1InterParams};
use oxideav_svq::svq1_plane::{chroma_dim, decode_frame, decode_intra_frame, Svq1DecodedFrame};

const CHAIN: &[u8] = include_bytes!("fixtures/enc_4mv_176x144_4f.svq1");
const REF_YUV: &[u8] = include_bytes!("fixtures/enc_4mv_176x144_4f.yuv410p");

/// Per-frame packet sizes, recorded at fixture-generation time.
const SIZES: [usize; 4] = [7601, 1357, 1178, 1076];

const W: usize = 176;
const H: usize = 144;

fn textured_plane(width: usize, height: usize, seed: u32) -> Vec<u8> {
    (0..width * height)
        .map(|i| {
            let x = (i % width) as u32;
            let y = (i / width) as u32;
            let hash = (x.wrapping_mul(31) ^ y.wrapping_mul(17) ^ seed).wrapping_mul(2654435761);
            let gradient = (x * 2 + y * 3) % 200;
            ((gradient + (hash >> 24) % 56) % 256) as u8
        })
        .collect()
}

/// Quadrant-divergent warp: quadrant `(qx, qy)` of each 16×16
/// macroblock shifts by `(qx * 2 + t, qy * 2)` with edge clamping —
/// four distinct per-8×8 motions per macroblock.
fn warp(plane: &[u8], w: usize, h: usize, t: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for row in 0..h {
        for col in 0..w {
            let (qx, qy) = ((col % 16) / 8, (row % 16) / 8);
            let sx = col.saturating_sub(qx * 2 + t).min(w - 1);
            let sy = row.saturating_sub(qy * 2).min(h - 1);
            out[row * w + col] = plane[sy * w + sx];
        }
    }
    out
}

fn plane_ref(samples: &[u8], width: usize, height: usize) -> Svq1PlaneRef<'_> {
    Svq1PlaneRef {
        samples,
        width,
        height,
    }
}

/// Re-run the deterministic encode and compare against the committed
/// stream byte-for-byte.
#[test]
fn encoder_reproduces_committed_4mv_chain() {
    let (cw, ch) = (chroma_dim(W), chroma_dim(H));
    let y0 = textured_plane(W, H, 7);
    let u0 = textured_plane(cw, ch, 101);
    let v0 = textured_plane(cw, ch, 202);
    let i_bytes = encode_intra_frame(
        plane_ref(&y0, W, H),
        plane_ref(&u0, cw, ch),
        plane_ref(&v0, cw, ch),
        Svq1EncoderMode::Adaptive { lambda: 32 },
    )
    .expect("intra encodes");
    let mut all = i_bytes.clone();
    let mut reference = decode_intra_frame(&i_bytes).expect("intra decodes");

    for t in 1..=3usize {
        let y = warp(&reference.y.visible(), W, H, t);
        let u = warp(&reference.u.visible(), cw, ch, t);
        let v = warp(&reference.v.visible(), cw, ch, t);
        let p = encode_inter_frame(
            plane_ref(&y, W, H),
            plane_ref(&u, cw, ch),
            plane_ref(&v, cw, ch),
            &reference,
            &Svq1InterParams {
                lambda: 24,
                temporal_reference: t as u8,
                ..Default::default()
            },
        )
        .expect("P encodes");
        all.extend_from_slice(&p.bytes);
        reference = p.reconstruction;
    }

    assert_eq!(all.len(), CHAIN.len(), "chain length");
    assert_eq!(all, CHAIN, "encoder output must be deterministic");
}

/// Decode the committed chain frame by frame and compare against the
/// committed reference-decoder output.
#[test]
fn committed_4mv_chain_decodes_byte_exact_against_reference() {
    let (cw, ch) = (chroma_dim(W), chroma_dim(H));
    let frame_yuv = W * H + 2 * cw * ch;
    assert_eq!(CHAIN.len(), SIZES.iter().sum::<usize>());
    assert_eq!(REF_YUV.len(), 4 * frame_yuv);

    let mut offset = 0usize;
    let mut reference: Option<Svq1DecodedFrame> = None;
    for (frame_no, &size) in SIZES.iter().enumerate() {
        let chunk = &CHAIN[offset..offset + size];
        offset += size;
        let frame = decode_frame(chunk, reference.as_ref())
            .unwrap_or_else(|e| panic!("frame {frame_no} decodes: {e:?}"));
        assert_eq!(frame.header.is_intra(), frame_no == 0);

        let want = &REF_YUV[frame_no * frame_yuv..(frame_no + 1) * frame_yuv];
        let (want_y, rest) = want.split_at(W * H);
        let (want_u, want_v) = rest.split_at(cw * ch);
        assert_eq!(frame.y.visible(), want_y, "frame {frame_no} Y plane");
        assert_eq!(frame.u.visible(), want_u, "frame {frame_no} U plane");
        assert_eq!(frame.v.visible(), want_v, "frame {frame_no} V plane");

        reference = Some(frame);
    }
}

/// The committed chain genuinely exercises INTER_4MV: re-encoding
/// with `allow_4mv = false` must produce a different (and larger)
/// stream.
#[test]
fn committed_chain_relies_on_4mv() {
    let (cw, ch) = (chroma_dim(W), chroma_dim(H));
    let y0 = textured_plane(W, H, 7);
    let u0 = textured_plane(cw, ch, 101);
    let v0 = textured_plane(cw, ch, 202);
    let i_bytes = encode_intra_frame(
        plane_ref(&y0, W, H),
        plane_ref(&u0, cw, ch),
        plane_ref(&v0, cw, ch),
        Svq1EncoderMode::Adaptive { lambda: 32 },
    )
    .expect("intra encodes");
    let reference = decode_intra_frame(&i_bytes).expect("intra decodes");

    let y = warp(&reference.y.visible(), W, H, 1);
    let u = warp(&reference.u.visible(), cw, ch, 1);
    let v = warp(&reference.v.visible(), cw, ch, 1);
    let single = encode_inter_frame(
        plane_ref(&y, W, H),
        plane_ref(&u, cw, ch),
        plane_ref(&v, cw, ch),
        &reference,
        &Svq1InterParams {
            lambda: 24,
            temporal_reference: 1,
            allow_4mv: false,
            ..Default::default()
        },
    )
    .expect("single-MV encodes");

    assert!(
        single.bytes.len() > SIZES[1] * 2,
        "without 4MV the first P-frame should be much larger ({} vs {})",
        single.bytes.len(),
        SIZES[1]
    );
}
