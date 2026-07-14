//! Fuzz SVQ1 P/B-frame decode against a held reference frame: the
//! first input byte selects the reference geometry (the committed
//! 176×144 fixture, or a synthesised 160×120 frame exercising the
//! overhang-macroblock geometry), the rest is fed to `decode_frame`
//! as untrusted frame bytes. Decode must reconstruct or error — never
//! panic, never read outside the reference canvases.

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq1_enc::{encode_intra_frame, Svq1EncoderMode, Svq1PlaneRef};
use oxideav_svq::svq1_plane::{chroma_dim, decode_frame, Svq1DecodedFrame};
use std::sync::OnceLock;

/// The committed 176×144 intra fixture (a real reference-encoder
/// stream; see `tests/fixtures/`).
const INTRA_176X144: &[u8] = include_bytes!("../../tests/fixtures/intra_176x144.svq1");

fn plane(width: usize, height: usize, seed: u8) -> Vec<u8> {
    (0..width * height)
        .map(|i| (i as u32).wrapping_mul(97).wrapping_add(seed as u32) as u8)
        .collect()
}

/// Decode of `encode_intra_frame` over a deterministic gradient at
/// `width × height` — used to fuzz non-multiple-of-16 (overhang)
/// reference geometry without committing another fixture.
fn synth_reference(width: usize, height: usize) -> Svq1DecodedFrame {
    let (cw, ch) = (chroma_dim(width), chroma_dim(height));
    let (y, u, v) = (
        plane(width, height, 11),
        plane(cw, ch, 57),
        plane(cw, ch, 199),
    );
    let bytes = encode_intra_frame(
        Svq1PlaneRef {
            samples: &y,
            width,
            height,
        },
        Svq1PlaneRef {
            samples: &u,
            width: cw,
            height: ch,
        },
        Svq1PlaneRef {
            samples: &v,
            width: cw,
            height: ch,
        },
        Svq1EncoderMode::Adaptive { lambda: 8 },
    )
    .expect("synth intra encode");
    decode_frame(&bytes, None).expect("synth intra decode")
}

fn references() -> &'static [Svq1DecodedFrame; 2] {
    static REFS: OnceLock<[Svq1DecodedFrame; 2]> = OnceLock::new();
    REFS.get_or_init(|| {
        [
            decode_frame(INTRA_176X144, None).expect("fixture decode"),
            synth_reference(160, 120),
        ]
    })
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let reference = &references()[(data[0] & 1) as usize];
    if let Ok(frame) = decode_frame(&data[1..], Some(reference)) {
        // Successful P/B decode inherits the reference geometry;
        // intra frames in the input carry their own.
        assert_eq!(frame.y.samples.len(), frame.y.stride * frame.y.rows);
        assert_eq!(frame.u.width, chroma_dim(frame.width()));
        assert_eq!(frame.v.height, chroma_dim(frame.height()));
    }
});
