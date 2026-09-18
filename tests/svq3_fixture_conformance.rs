//! SVQ3 end-to-end conformance against the real-stream fixtures staged
//! under `docs/video/svq3/fixtures/` (`real-sample-240x128`,
//! `real-sample-320x240-short-seqh`): every access unit is decoded in
//! order and compared byte for byte with the black-box reference decode
//! `expected.yuv`.
//!
//! The fixtures are excerpts of published sample files kept in the
//! private docs staging, so they are not vendored into this crate: the
//! harness looks for them at `$OXIDEAV_SVQ3_FIXTURES` or at
//! `../../docs/video/svq3/fixtures` relative to the crate and reports a
//! skip when neither exists (CI has no docs checkout; the local gate
//! does).
//!
//! `expected.yuv` is the reconstruction **without** the spec/09
//! intra-picture edge filter (`fixtures/README.md`, "Reference-decode
//! caveat"), so the comparison runs the decoder with the filter off;
//! the filtered pictures of the 320×240 stream's access units 2 and 3
//! are pinned separately by their SHA-256 (`spec/09` §6).

use std::path::PathBuf;

use oxideav_svq::svq3::{parse_extradata_flexible, Svq3FrameType, Svq3SequenceHeader};
use oxideav_svq::svq3_frame::{Svq3DecodeOptions, Svq3PictureDecoder};
use oxideav_svq::svq3_picture::Svq3Picture;

/// One staged fixture.
struct Fixture {
    name: &'static str,
    seqh: Svq3SequenceHeader,
    /// `(access unit bytes, sync flag)` in decode order.
    access_units: Vec<(Vec<u8>, bool)>,
    /// `expected.yuv`, one planar 4:2:0 frame per access unit.
    expected: Vec<u8>,
}

impl Fixture {
    fn frame_size(&self) -> usize {
        let (w, h) = (usize::from(self.seqh.width), usize::from(self.seqh.height));
        w * h + 2 * w.div_ceil(2) * h.div_ceil(2)
    }

    fn expected_frame(&self, index: usize) -> &[u8] {
        let n = self.frame_size();
        &self.expected[index * n..(index + 1) * n]
    }
}

fn fixtures_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("OXIDEAV_SVQ3_FIXTURES") {
        let p = PathBuf::from(dir);
        return p.is_dir().then_some(p);
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/video/svq3/fixtures");
    p.is_dir().then_some(p)
}

fn load_fixture(name: &'static str) -> Option<Fixture> {
    let dir = fixtures_dir()?.join(name);
    let extradata = std::fs::read(dir.join("extradata.bin")).ok()?;
    let samples = std::fs::read(dir.join("samples.bin")).ok()?;
    let index = std::fs::read_to_string(dir.join("samples-index.csv")).ok()?;
    let expected = std::fs::read(dir.join("expected.yuv")).ok()?;
    let seqh = parse_extradata_flexible(&extradata).expect("fixture SEQH parses");
    let mut access_units = Vec::new();
    for (lineno, line) in index.lines().enumerate() {
        if lineno == 0 || line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        assert_eq!(cols.len(), 4, "samples-index.csv row: {line}");
        let offset: usize = cols[1].parse().unwrap();
        let size: usize = cols[2].parse().unwrap();
        let sync = cols[3].trim() == "1";
        access_units.push((samples[offset..offset + size].to_vec(), sync));
    }
    let f = Fixture {
        name,
        seqh,
        access_units,
        expected,
    };
    assert_eq!(
        f.expected.len(),
        f.frame_size() * f.access_units.len(),
        "{name}: expected.yuv holds one frame per access unit"
    );
    Some(f)
}

/// The visible `width × height` planes of a picture in the
/// `expected.yuv` layout (Y, then Cb, then Cr, tightly packed).
fn cropped_yuv(picture: &Svq3Picture, width: usize, height: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 3 / 2);
    let lw = picture.luma_width();
    for y in 0..height {
        out.extend_from_slice(&picture.luma()[y * lw..y * lw + width]);
    }
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let stride = picture.chroma_width();
    for plane in [picture.cb(), picture.cr()] {
        for y in 0..ch {
            out.extend_from_slice(&plane[y * stride..y * stride + cw]);
        }
    }
    out
}

/// `(mismatching samples, largest absolute difference)`.
fn compare(a: &[u8], b: &[u8]) -> (usize, u8) {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).fold((0, 0), |(n, m), (&x, &y)| {
        let d = x.abs_diff(y);
        (n + usize::from(d != 0), m.max(d))
    })
}

/// Decode every access unit of `f` (filter per `options`), returning
/// per access unit either the cropped YUV or the decode error message.
fn decode_all(f: &Fixture, options: Svq3DecodeOptions) -> Vec<Result<Vec<u8>, String>> {
    let mut dec = Svq3PictureDecoder::new(f.seqh.clone())
        .unwrap()
        .with_options(options);
    let (w, h) = (usize::from(f.seqh.width), usize::from(f.seqh.height));
    f.access_units
        .iter()
        .map(|(au, _)| {
            dec.decode_access_unit(au)
                .map(|d| cropped_yuv(&d.picture, w, h))
                .map_err(|e| e.to_string())
        })
        .collect()
}

const FIXTURES: [&str; 2] = ["real-sample-240x128", "real-sample-320x240-short-seqh"];

/// Print the per-access-unit scorecard of `decoded` against
/// `expected.yuv` and return whether every access unit selected by
/// `select(sync)` decoded byte-exact.
fn scorecard(
    f: &Fixture,
    decoded: &[Result<Vec<u8>, String>],
    select: impl Fn(bool) -> bool,
) -> bool {
    let mut all_exact = true;
    for (i, ((_, sync), result)) in f.access_units.iter().zip(decoded).enumerate() {
        let kind = if *sync { "I" } else { "P" };
        let counted = select(*sync);
        match result {
            Ok(frame) => {
                let (mismatches, max_diff) = compare(frame, f.expected_frame(i));
                eprintln!(
                    "{} AU{i} ({kind}): {} ({mismatches} samples differ, max |Δ| {max_diff})",
                    f.name,
                    if mismatches == 0 {
                        "byte-exact"
                    } else {
                        "MISMATCH"
                    }
                );
                if counted {
                    all_exact &= mismatches == 0;
                }
            }
            Err(e) => {
                eprintln!("{} AU{i} ({kind}): decode error: {e}", f.name);
                if counted {
                    all_exact = false;
                }
            }
        }
    }
    all_exact
}

#[test]
fn sync_access_units_decode_byte_exact_against_the_unfiltered_reference() {
    let Some(_) = fixtures_dir() else {
        eprintln!("svq3 fixtures not found (set OXIDEAV_SVQ3_FIXTURES) — skipping");
        return;
    };
    for name in FIXTURES {
        let f = load_fixture(name).expect("fixture files present");
        let decoded = decode_all(
            &f,
            Svq3DecodeOptions {
                intra_edge_filter: false,
            },
        );
        assert!(
            scorecard(&f, &decoded, |sync| sync),
            "{name}: every I access unit must decode byte-exact"
        );
    }
}

#[test]
fn fixture_sync_frames_decode_as_intra() {
    let Some(_) = fixtures_dir() else {
        return;
    };
    for name in FIXTURES {
        let f = load_fixture(name).unwrap();
        for (i, (au, sync)) in f.access_units.iter().enumerate() {
            if !*sync {
                continue;
            }
            // Each sync frame is a random-access point: a fresh decoder
            // must accept it without any reference.
            let mut dec = Svq3PictureDecoder::new(f.seqh.clone()).unwrap();
            let d = dec
                .decode_access_unit(au)
                .unwrap_or_else(|e| panic!("{name} AU{i}: {e}"));
            assert_eq!(d.frame_type, Svq3FrameType::Intra, "{name} AU{i}");
        }
    }
}
