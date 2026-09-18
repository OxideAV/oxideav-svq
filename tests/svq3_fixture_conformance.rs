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

/// SHA-256 (FIPS 180-4), enough to pin the filtered pictures the docs
/// identify by digest (`spec/09` §6).
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    h.iter().map(|v| format!("{v:08x}")).collect()
}

#[test]
fn sha256_known_answer() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn edge_filter_reproduces_the_component_pictures_of_the_320x240_stream() {
    // spec/09 §6 / fixtures/README.md: with the pass in force the
    // component's picture for access unit 2 of the 320×240 stream is
    // `320x240-filter/frame2.buf1.yuv` (SHA-256 7f07e2acc656…), while
    // access units 0 and 1 stay identical to expected.yuv (limit 1 on
    // a flat picture changes nothing) and the 240×128 stream is never
    // filtered (quantisers 0–1 → limit 0).
    let Some(_) = fixtures_dir() else {
        return;
    };
    let f = load_fixture("real-sample-320x240-short-seqh").unwrap();
    let filtered = decode_all(&f, Svq3DecodeOptions::default());
    let au0 = filtered[0].as_ref().expect("AU0 decodes");
    assert_eq!(
        compare(au0, f.expected_frame(0)),
        (0, 0),
        "AU0 unchanged by the filter"
    );
    let au2 = filtered[2].as_ref().expect("AU2 decodes");
    let (mismatches, max_diff) = compare(au2, f.expected_frame(2));
    assert!(
        mismatches > 0 && max_diff <= 2,
        "AU2 differs from the unfiltered reference by at most the limit"
    );
    assert!(
        sha256_hex(au2).starts_with("7f07e2acc656"),
        "AU2 filtered picture digest: {}",
        sha256_hex(au2)
    );

    let f = load_fixture("real-sample-240x128").unwrap();
    let filtered = decode_all(&f, Svq3DecodeOptions::default());
    let au0 = filtered[0].as_ref().expect("AU0 decodes");
    assert_eq!(
        compare(au0, f.expected_frame(0)),
        (0, 0),
        "quantiser 0: limit 0, no pass"
    );
}

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
