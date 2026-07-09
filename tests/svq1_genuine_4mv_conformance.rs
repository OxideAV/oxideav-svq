//! Genuine-INTER_4MV conformance — the docs `inter-4mv/` fixture
//! (issue #197, superseding the retracted #161 stream) is the first
//! SVQ1 stream that genuinely contains `INTER_4MV` macroblocks on the
//! wire, and whose decode is confirmed by an **independent** black-box
//! oracle.
//!
//! `tests/fixtures/enc_4mv_176x144_4f.svq1` is byte-identical
//! (SHA-256 `52c8fcb9…`) to `docs/video/svq1/fixtures/inter-4mv/
//! input.svq1`; `tests/fixtures/enc_4mv_176x144_4f.yuv410p` is
//! byte-identical (SHA-256 `226e44c6…`) to that fixture's
//! `expected.yuv` — the independent black-box decode oracle described
//! in the fixture notes. The stream is 176×144 (QCIF, exactly 11×9
//! luma + 3×3 chroma macroblocks, no edge overhang), `yuv410p`, one
//! I-frame + three P-frames of quadrant-divergent warp.
//!
//! Where `svq1_enc_inter_conformance.rs` pins the encoder→decoder
//! round trip on this chain (encoder determinism + our decode matching
//! the oracle pixels), and `svq1_inter4mv_conformance.rs` pins the
//! *retracted* 25-frame #161 stream (which carries ZERO 4MV MBs), this
//! file pins the observable that makes #197 a genuine-4MV fixture: the
//! **exact per-plane T03 mode census**. The fixture notes' independent
//! mode census (read off the wire by a separate tool, corroborated by
//! three mutually-independent decoders reconstructing the same pixels)
//! reports every luma macroblock of every P-frame as `INTER_4MV` plus
//! the majority of chroma macroblocks — **348** `INTER_4MV`
//! macroblocks in all. A mode misread cannot hide here: a 4MV MB reads
//! four differential MVs and fetches four distinct 8×8 sub-block
//! references, so any mislabelled mode both desynchronises the T03/MV
//! bitstream AND reconstructs different pixels — yet this decode is
//! byte-exact against the oracle across all four frames. Pinning the
//! census therefore makes "this decoder reads 348 INTER_4MV MBs, in
//! agreement with the independent census" a CI-conformance fact.

use oxideav_svq::svq1_plane::{
    chroma_dim, decode_frame_with_stats, decode_intra_frame, Svq1DecodedFrame, Svq1FrameModeStats,
    Svq1PlaneModeStats,
};

const CHAIN: &[u8] = include_bytes!("fixtures/enc_4mv_176x144_4f.svq1");
const REF_YUV: &[u8] = include_bytes!("fixtures/enc_4mv_176x144_4f.yuv410p");

/// Per-frame packet sizes from the fixture's `frame_sizes.tsv`
/// (frame 0 = I, 1..=3 = P).
const SIZES: [usize; 4] = [7601, 1357, 1178, 1076];

const W: usize = 176;
const H: usize = 144;

/// Decode the four-frame chain, asserting each plane byte-exact
/// against the independent oracle, and return the per-P-frame census.
fn decode_chain() -> Vec<Svq1FrameModeStats> {
    let (cw, ch) = (chroma_dim(W), chroma_dim(H));
    let frame_yuv = W * H + 2 * cw * ch;
    assert_eq!(CHAIN.len(), SIZES.iter().sum::<usize>());
    assert_eq!(REF_YUV.len(), 4 * frame_yuv);

    let mut offset = 0usize;
    let mut reference: Option<Svq1DecodedFrame> = None;
    let mut stats = Vec::new();
    for (n, &size) in SIZES.iter().enumerate() {
        let chunk = &CHAIN[offset..offset + size];
        offset += size;
        let want = &REF_YUV[n * frame_yuv..(n + 1) * frame_yuv];
        let (want_y, rest) = want.split_at(W * H);
        let (want_u, want_v) = rest.split_at(cw * ch);

        let frame = if n == 0 {
            let frame = decode_intra_frame(chunk).expect("I-frame decodes");
            assert!(frame.header.is_intra(), "frame 0 must be intra");
            frame
        } else {
            let (frame, frame_stats) = decode_frame_with_stats(chunk, reference.as_ref())
                .unwrap_or_else(|e| panic!("P-frame {n} decodes: {e:?}"));
            assert!(!frame.header.is_intra(), "frame {n} must be inter");
            stats.push(frame_stats);
            frame
        };

        assert_eq!(frame.y.visible(), want_y, "frame {n} Y plane");
        assert_eq!(frame.u.visible(), want_u, "frame {n} U plane");
        assert_eq!(frame.v.visible(), want_v, "frame {n} V plane");
        reference = Some(frame);
    }
    stats
}

#[test]
fn genuine_4mv_chain_decodes_byte_exact() {
    let stats = decode_chain();
    assert_eq!(stats.len(), 3);
}

/// The full per-plane T03 census for each of the three P-frames,
/// pinned exactly as our byte-exact decode reads them off the wire —
/// in agreement, macroblock-for-macroblock, with the fixture notes'
/// independent mode census (`docs/video/svq1/fixtures/inter-4mv/
/// notes.md` §"Confirmation that INTER_4MV MBs are actually present").
///
/// One plane's `(skip, inter, inter_4mv, intra)` mode counts. Ordering
/// matches the T03 census; the I-frame carries no T03 field and is not
/// counted here.
type PlaneCensus = (usize, usize, usize, usize);
/// One frame's `(Y, U, V)` per-plane census.
type FrameCensus = (PlaneCensus, PlaneCensus, PlaneCensus);

const EXPECTED_CENSUS: [FrameCensus; 3] = [
    // frame 1: Y all 4MV; U all 4MV; V 8 4MV + 1 intra
    ((0, 0, 99, 0), (0, 0, 9, 0), (0, 0, 8, 1)),
    // frame 2: identical mode geometry to frame 1
    ((0, 0, 99, 0), (0, 0, 9, 0), (0, 0, 8, 1)),
    // frame 3: the lone non-4MV chroma MB moves from V to U
    ((0, 0, 99, 0), (0, 0, 8, 1), (0, 0, 9, 0)),
];

fn tuple(p: &Svq1PlaneModeStats) -> PlaneCensus {
    (p.skip, p.inter, p.inter_4mv, p.intra)
}

#[test]
fn genuine_4mv_mode_census_is_pinned() {
    let stats = decode_chain();

    // Every plane census covers the full MB grid (11×9 luma, 3×3
    // chroma) — the walk is bit-aligned to the last macroblock.
    for (n, s) in stats.iter().enumerate() {
        assert_eq!(s.y.total(), 11 * 9, "frame {} luma census total", n + 1);
        assert_eq!(s.u.total(), 3 * 3, "frame {} U census total", n + 1);
        assert_eq!(s.v.total(), 3 * 3, "frame {} V census total", n + 1);
    }

    let got: Vec<_> = stats
        .iter()
        .map(|s| (tuple(&s.y), tuple(&s.u), tuple(&s.v)))
        .collect();
    assert_eq!(
        got,
        EXPECTED_CENSUS.to_vec(),
        "per-plane (skip, inter, inter_4mv, intra) census must match the \
         independent mode census"
    );

    // Every luma macroblock of every P-frame is INTER_4MV, with zero
    // SKIP / INTER / INTRA in luma — the defining property of this
    // fixture, unreachable unless the 4MV serial-predictor + per-
    // sub-block MC path is exercised on every MB.
    for (n, s) in stats.iter().enumerate() {
        assert_eq!(
            (s.y.skip, s.y.inter, s.y.inter_4mv, s.y.intra),
            (0, 0, 99, 0),
            "frame {} luma is not fully INTER_4MV",
            n + 1
        );
    }

    // The headline total: 348 INTER_4MV macroblocks across the three
    // P-frames (3×99 luma + 9+9+8 U + 8+8+9 V).
    let total_4mv: usize = stats
        .iter()
        .map(|s| s.y.inter_4mv + s.u.inter_4mv + s.v.inter_4mv)
        .sum();
    assert_eq!(total_4mv, 348, "total INTER_4MV macroblock count");
}

/// Corrupt variants of every genuine-4MV P-frame must error out
/// cleanly — never panic, never loop. Unlike the retracted #161
/// stream (whose P-frames carry SKIP / INTER / INTRA only), every
/// macroblock here is INTER_4MV, so this sweep is the one that
/// exercises the four-differential MV read and the **per-sub-block**
/// reference-window clamp under corruption: bit-flips inside the MV
/// fields produce four arbitrary (including far-out-of-window) 8×8
/// sub-block motions against a real reference frame, the exact input
/// class the per-sub-block clamp normalises.
#[test]
fn corrupt_genuine_4mv_p_frames_error_cleanly() {
    // Chain over the clean reconstructions so each P-frame is fuzzed
    // against the reference geometry it was actually coded against.
    let mut offset = SIZES[0];
    let mut reference = decode_intra_frame(&CHAIN[..SIZES[0]]).expect("I-frame decodes");

    for &size in &SIZES[1..] {
        let p = &CHAIN[offset..offset + size];

        // Truncations at every byte boundary: decode or clean error,
        // never a panic or hang.
        for len in 0..p.len() {
            let _ = decode_frame_with_stats(&p[..len], Some(&reference));
        }

        // Dense bit-flip sweep (every 3rd byte, all 8 bits) over the
        // whole 4MV payload — mode field, four MV differentials, and
        // the residual leaf walk.
        let mut mutated = p.to_vec();
        for byte in (0..p.len()).step_by(3) {
            for bit in 0..8u8 {
                mutated[byte] ^= 1 << bit;
                let _ = decode_frame_with_stats(&mutated, Some(&reference));
                mutated[byte] ^= 1 << bit;
            }
        }

        // Advance the chain on the clean decode.
        reference = decode_frame_with_stats(p, Some(&reference))
            .expect("clean P-frame decodes")
            .0;
        offset += size;
    }
}
