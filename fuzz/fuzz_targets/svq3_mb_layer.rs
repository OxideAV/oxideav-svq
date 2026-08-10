//! Fuzz the SVQ3 macroblock-layer building blocks on arbitrary bits:
//! MB-type classification, the intra-4×4 prediction-mode VLC walk,
//! the Golomb `(run, value)` coefficient walkers, the inter-MB motion
//! header, and the per-macroblock intra reconstruction paths driven
//! with wire-magnitude hostile coefficients. Every layer must return
//! a structured `Error` or reconstruct — never panic (including on
//! arithmetic overflow in the dequant / transform interleave).

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq3::Svq3FrameType;
use oxideav_svq::svq3_coeff::{
    decode_chroma_dc_2x2, decode_residual_4x4_alt, decode_residual_4x4_normal,
};
use oxideav_svq::svq3_mb::{decode_intra_4x4_modes, read_mb_type};
use oxideav_svq::svq3_mv::{read_inter_macroblock_header, read_quantiser_delta};
use oxideav_svq::svq3_recon::{
    decode_and_reconstruct_intra_luma_macroblock, LumaMacroblock, MB_LUMA_BLOCKS,
};
use oxideav_svq::svq3_scan::{ALT_SCAN_4X4_SCAN, NORMAL_ZIGZAG_4X4_SCAN};
use oxideav_svq::BitReader;

const FRAME_TYPES: [Svq3FrameType; 3] = [
    Svq3FrameType::Predicted,
    Svq3FrameType::Bidirectional,
    Svq3FrameType::Intra,
];

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let sel = data[0] % 5;
    let q = (data[1] % 32) as u32;
    let top_avail = data[2] & 1 != 0;
    let left_avail = data[2] & 2 != 0;
    let bits = &data[4..];

    match sel {
        0 => {
            // MB-type Golomb walk for every frame type, sequentially
            // (a P stream is a sequence of type codes at minimum).
            for frame_type in FRAME_TYPES {
                let mut br = BitReader::new(bits);
                while let Ok(t) = read_mb_type(&mut br, frame_type) {
                    let _ = t.num_motion_vectors();
                }
            }
        }
        1 => {
            let mut br = BitReader::new(bits);
            let _ = decode_intra_4x4_modes(&mut br, top_avail, left_avail);
        }
        2 => {
            // Residual block decoders over the same bits, independently.
            let mut dc = [0i32; 4];
            let mut br = BitReader::new(bits);
            while decode_chroma_dc_2x2(&mut br, &mut dc).is_ok()
                && br.bits_consumed() < bits.len() * 8
            {}
            let mut block = [0i32; 16];
            let mut br = BitReader::new(bits);
            while decode_residual_4x4_alt(&mut br, &ALT_SCAN_4X4_SCAN, &mut block).is_ok()
                && br.bits_consumed() < bits.len() * 8
            {}
            let start = (data[3] & 1) as usize;
            let mut br = BitReader::new(bits);
            while decode_residual_4x4_normal(&mut br, &NORMAL_ZIGZAG_4X4_SCAN, start, &mut block)
                .is_ok()
                && br.bits_consumed() < bits.len() * 8
            {}
        }
        3 => {
            // Inter-MB envelope: type, precision selector, MV
            // differentials, then a quantiser delta.
            let mut br = BitReader::new(bits);
            for frame_type in [Svq3FrameType::Predicted, Svq3FrameType::Bidirectional] {
                if let Ok(mb_type) = read_mb_type(&mut br, frame_type) {
                    let _ = read_inter_macroblock_header(
                        &mut br,
                        mb_type,
                        frame_type,
                        data[2] & 4 != 0,
                        data[2] & 8 != 0,
                    );
                }
                let _ = read_quantiser_delta(&mut br);
            }
        }
        _ => {
            // Bits → intra-4×4 luma macroblock reconstruction with
            // hostile placed coefficients (wire-reachable magnitudes:
            // the normal-scan walker yields values up to code >> 4).
            let mut coeffs = [[0i32; 16]; MB_LUMA_BLOCKS];
            let mut k = 4usize;
            for block in coeffs.iter_mut() {
                for c in block.iter_mut() {
                    if k + 4 <= data.len() {
                        *c = i32::from_le_bytes([data[k], data[k + 1], data[k + 2], data[k + 3]]);
                        k += 4;
                    }
                }
            }
            let mut mb = LumaMacroblock::new();
            mb.above_available = top_avail;
            mb.left_available = left_avail;
            mb.above = [data[1]; 16];
            mb.leftcol = [data[2]; 16];
            mb.corner = data[3];
            let mut br = BitReader::new(bits);
            let _ = decode_and_reconstruct_intra_luma_macroblock(
                &mut br, &mut mb, &coeffs, q, top_avail, left_avail,
            );
        }
    }
});
