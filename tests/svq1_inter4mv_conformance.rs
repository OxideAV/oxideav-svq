//! SVQ1 25-frame conformance against the docs `inter-4mv` fixture —
//! an independently-minted stream from a SECOND encoder family.
//!
//! `tests/fixtures/inter4mv_176x144_25f.svq1` is the concatenated raw
//! SVQ1 frame payload of `docs/video/svq1/fixtures/inter-4mv/`
//! (docs commit `f210f08`): 176×144 (QCIF — exactly 11×9 luma
//! macroblocks, no edge overhang), yuv410p, one I-frame + 24
//! P-frames. `inter4mv_176x144_25f.yuv410p` is the minting
//! toolchain's own black-box decode (25 × 28 512 B, verified 0
//! decoder errors).
//!
//! NOTE ON THE NAME: the fixture was staged to prove `INTER_4MV`
//! presence via a differential `+mv4`/`-mv4` argument — but the
//! byte-exact mode census below shows the stream contains **zero**
//! INTER_4MV macroblocks (see
//! [`inter4mv_luma_mode_census_is_pinned`] for the refutation
//! record). The fixture keeps its docs name; what it actually pins
//! is different and still substantial.
//!
//! Decoding all 25 frames byte-exact — each P against OUR previous
//! reconstruction, so any divergence cascades and cannot hide —
//! pins, in the Validator role:
//!
//! * cross-encoder generality of the audit/01 §7.1 T03 mode
//!   permutation, spec/06 §6.2.3 Reading B, and the §6.4 median MV
//!   predictor — this stream comes from a different encoder family
//!   than every prior fixture (SKIP / INTER / INTRA mix per frame,
//!   ~7 500 macroblocks total);
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

use oxideav_svq::svq1_plane::{
    decode_frame_with_stats, decode_intra_frame, Svq1DecodedFrame, Svq1FrameModeStats,
};

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

/// Decode the whole 25-frame chain, asserting byte-exactness per
/// frame, and return the per-P-frame T03 mode census.
fn decode_chain() -> Vec<Svq1FrameModeStats> {
    assert_eq!(FRAME_SIZES.iter().sum::<usize>(), FRAMES.len());
    assert_eq!(REF_YUV.len(), 25 * FRAME_YUV);

    let mut offset = 0usize;
    let mut reference: Option<Svq1DecodedFrame> = None;
    let mut stats = Vec::new();
    for (n, &size) in FRAME_SIZES.iter().enumerate() {
        let bytes = &FRAMES[offset..offset + size];
        offset += size;
        let want = &REF_YUV[n * FRAME_YUV..(n + 1) * FRAME_YUV];

        let frame = if n == 0 {
            let frame = decode_intra_frame(bytes).expect("I-frame decodes");
            assert!(frame.header.is_intra(), "frame 0 must be intra");
            frame
        } else {
            let (frame, frame_stats) = decode_frame_with_stats(bytes, reference.as_ref())
                .unwrap_or_else(|e| {
                    panic!("P-frame {n} fails to decode: {e:?}");
                });
            assert!(!frame.header.is_intra(), "frame {n} must be inter");
            stats.push(frame_stats);
            frame
        };

        assert_frame_matches(n, &frame, want);
        reference = Some(frame);
    }
    stats
}

#[test]
fn inter4mv_25_frame_chain_decodes_byte_exact() {
    decode_chain();
}

/// The exact per-P-frame luma INTER_4MV macroblock counts — the
/// observable the fixture notes flag as unobtainable without a full
/// decoder ("exact per-MB INTER_4MV count is NOT independently
/// obtainable"; `docs/video/svq1/fixtures/inter-4mv/notes.md`). This
/// decoder is byte-exact against the minting toolchain's own decode
/// across the whole chain, so its wire census IS the stream's mode
/// record; pinning it makes the count a CI-conformance fact and a
/// Validator-round exhibit.
#[test]
fn inter4mv_luma_mode_census_is_pinned() {
    let stats = decode_chain();
    assert_eq!(stats.len(), 24);

    // Every plane census must cover the full MB grid (11×9 luma,
    // 3×3 chroma).
    for (n, s) in stats.iter().enumerate() {
        assert_eq!(s.y.total(), 11 * 9, "frame {} luma census total", n + 1);
        assert_eq!(s.u.total(), 3 * 3, "frame {} U census total", n + 1);
        assert_eq!(s.v.total(), 3 * 3, "frame {} V census total", n + 1);
    }

    // REFUTATION RECORD: the fixture was minted (docs f210f08) on a
    // differential argument concluding "≥ 1 INTER_4MV MB in P-frame
    // 1" — but the byte-exact census shows ZERO INTER_4MV macroblocks
    // in the ENTIRE stream, on every plane. A mode misread cannot
    // hide here: T03 codeword lengths and the per-mode MV field
    // counts differ, so any mode confusion desynchronises the bit
    // stream, while this decode is byte-exact for all 25 frames (and
    // the T03 position-1 = INTER_4MV mapping is independently pinned
    // by the r386 encoder fixture, which the reference Sorenson
    // decoder binary reproduced byte-exact). The docs' control lemma
    // ("+mv4 is inert unless INTER_4MV is selected", verified only on
    // flat content) therefore does not generalise: the flag changed
    // the minting encoder's mode/MV decisions on this content without
    // the mode ever being emitted.
    for (n, s) in stats.iter().enumerate() {
        assert_eq!(
            (s.y.inter_4mv, s.u.inter_4mv, s.v.inter_4mv),
            (0, 0, 0),
            "frame {} unexpectedly carries INTER_4MV",
            n + 1
        );
    }

    // Pin the exact luma census (skip / inter / intra per P-frame) as
    // the stream's mode record — the observable the fixture notes
    // call for.
    let y_census: Vec<(usize, usize, usize)> = stats
        .iter()
        .map(|s| (s.y.skip, s.y.inter, s.y.intra))
        .collect();
    assert_eq!(
        y_census, EXPECTED_Y_CENSUS,
        "exact per-P-frame luma (skip, inter, intra) MB counts"
    );
}

/// Corrupt variants of the fixture's P-frame must error out cleanly —
/// never panic, never loop. This targets the r391 reference-window
/// clamp arithmetic specifically: bit-flips inside the MV and mode
/// fields produce arbitrary (including far-out-of-window) motion
/// vectors against a real reference frame, the exact input class the
/// clamp normalises.
#[test]
fn corrupt_p_frames_error_cleanly_under_the_window_clamp() {
    let reference = decode_intra_frame(&FRAMES[..FRAME_SIZES[0]]).expect("I-frame decodes");
    let p1 = &FRAMES[FRAME_SIZES[0]..FRAME_SIZES[0] + FRAME_SIZES[1]];

    // Truncations at every byte boundary: decode or clean error.
    for len in 0..p1.len() {
        let _ = decode_frame_with_stats(&p1[..len], Some(&reference));
    }

    // Bit-flip sweep over the P-frame payload (every 5th byte, all
    // 8 bits — dense in the header/MV region, still covering the
    // whole residual tail; the full sweep is ~5× slower for the same
    // panic-surface class).
    let mut mutated = p1.to_vec();
    for byte in (0..p1.len()).step_by(5) {
        for bit in 0..8u8 {
            mutated[byte] ^= 1 << bit;
            let _ = decode_frame_with_stats(&mutated, Some(&reference));
            mutated[byte] ^= 1 << bit;
        }
    }
}

/// Exact luma `(skip, inter, intra)` MB counts for P-frames 1..=24
/// (of 99 MBs each), as decoded by the byte-exact chain — see
/// [`inter4mv_luma_mode_census_is_pinned`].
const EXPECTED_Y_CENSUS: [(usize, usize, usize); 24] = [
    (1, 81, 17),
    (0, 79, 20),
    (0, 74, 25),
    (0, 72, 27),
    (0, 78, 21),
    (0, 76, 23),
    (0, 77, 22),
    (0, 74, 25),
    (0, 75, 24),
    (0, 75, 24),
    (0, 71, 28),
    (0, 76, 23),
    (0, 83, 16),
    (0, 79, 20),
    (0, 88, 11),
    (0, 87, 12),
    (0, 86, 13),
    (0, 84, 15),
    (0, 84, 15),
    (0, 82, 17),
    (0, 84, 15),
    (0, 88, 11),
    (0, 88, 11),
    (0, 83, 16),
];
