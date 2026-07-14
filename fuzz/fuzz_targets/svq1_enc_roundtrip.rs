//! Differential encoder↔decoder fuzz: derive frame geometry, encoder
//! mode, and P-frame tuning from the input, encode fuzz-derived plane
//! content, and require that
//!
//! * every encode succeeds (the input domain is fully legal),
//! * our decoder accepts every emitted stream,
//! * the decoded I-frame geometry matches the encode input, and
//! * the decoded P-frame is byte-identical to the encoder's own
//!   `Svq1EncodedFrame::reconstruction` (the decoder-authoritative
//!   reconstruction contract of `encode_inter_frame`).
//!
//! Any divergence is a real encoder/decoder disagreement — the same
//! invariant the conformance suite pins on fixtures, driven here over
//! arbitrary content, dimensions (including overhang), and knobs.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq1_enc::{encode_intra_frame, Svq1EncoderMode, Svq1PlaneRef};
use oxideav_svq::svq1_enc_inter::{encode_inter_frame, Svq1InterParams};
use oxideav_svq::svq1_plane::{chroma_dim, decode_frame};

fn plane(width: usize, height: usize, payload: &[u8], salt: u32) -> Vec<u8> {
    if payload.is_empty() {
        return vec![salt as u8; width * height];
    }
    (0..width * height)
        .map(|i| {
            let j = (i as u32).wrapping_mul(2654435761).wrapping_add(salt) as usize;
            payload[j % payload.len()]
        })
        .collect()
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    // Keep dimensions small for throughput but sweep every mod-16
    // residue so overhang macroblocks (spec/02 §2.3.1) are exercised.
    let width = 1 + (data[0] as usize % 48);
    let height = 1 + (data[1] as usize % 48);
    let mode = match data[2] % 5 {
        0 => Svq1EncoderMode::MeanOnlyL5,
        1 => Svq1EncoderMode::MeanOnlyL3,
        2 => Svq1EncoderMode::MeanPlusOneStageL3,
        3 => Svq1EncoderMode::MultiStageL3,
        _ => Svq1EncoderMode::Adaptive {
            lambda: (data[3] as u64) << (data[4] % 12),
        },
    };
    let params = Svq1InterParams {
        lambda: (data[5] as u64) << (data[6] % 10),
        search_radius: data[7] % 12,
        allow_4mv: data[7] & 0x10 != 0,
        droppable: data[7] & 0x20 != 0,
        temporal_reference: data[3],
    };
    let payload = &data[8..];

    let (cw, ch) = (chroma_dim(width), chroma_dim(height));
    let y = plane(width, height, payload, 0xA5);
    let u = plane(cw, ch, payload, 0x3C);
    let v = plane(cw, ch, payload, 0xE1);
    let yr = Svq1PlaneRef {
        samples: &y,
        width,
        height,
    };
    let ur = Svq1PlaneRef {
        samples: &u,
        width: cw,
        height: ch,
    };
    let vr = Svq1PlaneRef {
        samples: &v,
        width: cw,
        height: ch,
    };

    let intra_bytes = encode_intra_frame(yr, ur, vr, mode).expect("legal intra encode");
    let reference = decode_frame(&intra_bytes, None).expect("own intra stream must decode");
    assert_eq!(reference.width(), width);
    assert_eq!(reference.height(), height);

    // Second frame: shifted content so motion search has work to do.
    let y2 = plane(width, height, payload, 0x51);
    let u2 = plane(cw, ch, payload, 0x9B);
    let v2 = plane(cw, ch, payload, 0x27);
    let encoded = encode_inter_frame(
        Svq1PlaneRef {
            samples: &y2,
            width,
            height,
        },
        Svq1PlaneRef {
            samples: &u2,
            width: cw,
            height: ch,
        },
        Svq1PlaneRef {
            samples: &v2,
            width: cw,
            height: ch,
        },
        &reference,
        &params,
    )
    .expect("legal inter encode");

    let decoded = decode_frame(&encoded.bytes, Some(&reference)).expect("own P stream must decode");
    assert_eq!(
        decoded.y.samples, encoded.reconstruction.y.samples,
        "luma decode != encoder reconstruction"
    );
    assert_eq!(
        decoded.u.samples, encoded.reconstruction.u.samples,
        "Cb decode != encoder reconstruction"
    );
    assert_eq!(
        decoded.v.samples, encoded.reconstruction.v.samples,
        "Cr decode != encoder reconstruction"
    );
});
