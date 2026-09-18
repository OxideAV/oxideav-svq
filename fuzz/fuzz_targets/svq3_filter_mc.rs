//! Fuzz the two sample-domain kernels of the SVQ3 P/I paths on
//! arbitrary planes: the spec/09 edge filter (`filter_plane` at every
//! limit over fuzz-derived geometries) and motion compensation
//! (`motion_compensate_block` / `motion_compensate_chroma_block` with
//! hostile vectors at every phase, including the i32 extremes) — no
//! panics, no overflow, every output sample in range, filtered planes
//! bounded by twice the limit (one vertical and one horizontal edge
//! per sample).

#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_svq::svq3_filter::filter_plane;
use oxideav_svq::svq3_mc::{
    motion_compensate_block, motion_compensate_chroma_block, ReferencePlane,
};

fuzz_target!(|data: &[u8]| {
    if data.len() < 12 {
        return;
    }
    let width = 1 + (data[0] as usize % 40);
    let height = 1 + (data[1] as usize % 40);
    let limit = data[2] % 8;
    let plane_len = width * height;
    let mut samples: Vec<u8> = data[12..].iter().copied().take(plane_len).collect();
    samples.resize(plane_len, data[3]);

    // Edge filter: a sample sits on at most one vertical and one
    // horizontal edge, so the two sweeps move it by at most 2·limit.
    let mut filtered = samples.clone();
    filter_plane(&mut filtered, width, height, limit);
    for (a, b) in samples.iter().zip(&filtered) {
        assert!(
            a.abs_diff(*b) <= 2 * limit,
            "filter exceeded twice its limit"
        );
    }

    // Motion compensation with arbitrary vectors and block geometry.
    let plane = ReferencePlane::new(&samples, width, height).expect("sized plane");
    let mv_x = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    let mv_y = i32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let x = (data[3] as i32 % 64) - 32;
    let y = (data[2] as i32 % 64) - 32;
    let (w, h) = match data[0] % 7 {
        0 => (16, 16),
        1 => (8, 16),
        2 => (16, 8),
        3 => (8, 8),
        4 => (4, 8),
        5 => (8, 4),
        _ => (4, 4),
    };
    let block = motion_compensate_block(&plane, x, y, w, h, mv_x, mv_y);
    assert_eq!(block.len(), w * h);
    let chroma = motion_compensate_chroma_block(&plane, x, y, w, h, mv_x, mv_y);
    assert_eq!(chroma.len(), (w / 2) * (h / 2));
    // Extreme vectors resolve to edge replication, never a panic.
    let _ = motion_compensate_block(&plane, x, y, w, h, i32::MAX, i32::MIN);
    let _ = motion_compensate_block(&plane, i32::MAX, i32::MIN, w, h, mv_x, mv_y);
});
