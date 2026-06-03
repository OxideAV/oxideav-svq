//! Build script — parses the clean-room SVQ1 codebook + helper-LUT
//! tables mirrored under `tables/` (copied bit-exact from
//! `docs/video/svq1/tables/codebook-l0l3.csv` +
//! `tables/codebook-descriptor.csv` + `tables/clip_lut.csv` +
//! `tables/svc_bitmask_lut.csv`) and emits compile-time Rust constants
//! in `$OUT_DIR/svq1_codebook_data.rs`. **The Implementer must NOT
//! retype these numbers manually**; they come straight from the CSVs
//! produced by the docs collaborator (Extractor 02,
//! `docs/video/svq1/provenance/02-codebook-extraction.md`) from
//! `reference/binaries/quicktimethirdparty.qtx` SHA-256
//! `ac3509bf22aa1458dfc6e1af980956c0153b4c287af452ae5b9cac6f923be169`.
//!
//! Emitted constants:
//!
//! * `pub static SVQ1_CODEBOOK_L0L3_BYTES: [i8; 23004]` — the
//!   contiguous mean-removed-VQ vector payload at file offset
//!   `0x5d224..0x62c00` (VMA `0x67dcd224..0x67dd2c00`) of the
//!   reference binary. Holds the L=0..L=3 codebooks (intra+inter
//!   halves), per `docs/video/svq1/tables/codebook-l0l3.meta`:
//!   `2 × (768 + 1536 + 3072 + 6144) = 23040 B` minus a 36-byte
//!   descriptor/LUT prefix = 23004 B vector payload.
//! * `pub static SVQ1_CODEBOOK_DESCRIPTOR: [u8; 36]` — the 36-byte
//!   level-descriptor + block-shape prefix at file offset
//!   `0x5d200..0x5d224`.
//! * `pub static SVQ1_BLOCK_SHAPE_LUT: [u8; 16]` — the 16-entry
//!   block-shape lookup (values 1..=4) that lives at the tail of the
//!   descriptor (file offset `0x5d214..0x5d224`); see
//!   `docs/video/svq1/tables/codebook-descriptor.meta` line 22.
//! * `pub static SVQ1_U16_PARAM_TABLE: [u16; 512]` — the 1024-byte
//!   u16-LE parameter table at file offset `0x59d00..0x5a100` (VMA
//!   `0x67dc9d00..0x67dca100`, section `.rdata`), mirrored bit-exact
//!   from `tables/u16_param_table.csv` (itself a bit-exact mirror of
//!   `docs/video/svq1/tables/u16_param_table.csv`). Per its
//!   companion `.meta`, the table holds u16 values drawn from the
//!   set `{0x0000, 0x0001, 0x0002, 0x0010, 0x0014, 0x0020, 0x0028,
//!   0x0048, 0x0068, 0x0081, 0x0082, 0x0084, 0x0101, 0x0102, 0x0181,
//!   0x0182}` arranged in grouped runs adjacent to the clip LUT;
//!   no SVQ1 pixel-decode path consumes it yet.
//! * `pub const SVQ1_L4_ABSENCE: Svq1AbsentLevelRecord` and
//!   `pub const SVQ1_L5_ABSENCE: Svq1AbsentLevelRecord` — typed records
//!   mirrored bit-for-bit from `tables/codebook-l4.meta` and
//!   `tables/codebook-l5.meta` (themselves a bit-exact mirror of
//!   `docs/video/svq1/tables/codebook-l{4,5}.meta`). Each carries the
//!   canonical vector length and the would-be per-half codebook size
//!   that the docs collaborator's Extractor 02 pass ruled out as
//!   ABSENT in the reference binary. The build script parses the meta
//!   files and asserts `status: ABSENT` + matches against the
//!   `Svq1Level` derivation so a future docs revision that quietly
//!   flips a level back to "present" fails the build.
//!
//! No FFmpeg / libav* / Sorenson-SDK source is read at any step. The
//! CSV column order is `byte_index,file_offset_hex,vma_hex,value_signed,value_hex`;
//! we consume `value_signed` only. The two `codebook-l{4,5}.meta`
//! records are simple `key: value` lines (with `|` introducing
//! multi-line YAML-block scalars that we skip past — only the scalar
//! `level`, `block_size`, `canonical_vector_len_bytes`,
//! `canonical_6stage_intra_or_inter_bytes`, and `status` keys are
//! consumed).
//!
//! The two byte-wide helper-LUT CSVs (`clip_lut.csv`,
//! `svc_bitmask_lut.csv`) ship the `value_unsigned` column instead of
//! the signed-byte column the codebook CSVs use; the
//! `parse_unsigned_csv` helper consumes column 3 as a `u8` and
//! verifies the `byte_index` column runs `0..expected_len`. Their
//! `.meta` companions document the source region (file offset,
//! length, byte structure) and are NOT consumed at build time —
//! their content is documented in the dedicated module docs in
//! `src/svq1_helper_luts.rs`.
//!
//! The 16-bit-wide `u16_param_table.csv` ships a
//! `word_index,file_offset_hex,vma_hex,value_u16,value_hex` row
//! layout (note the `word_index` column instead of `byte_index`); the
//! `parse_u16_csv` helper consumes column 3 as a `u16` and verifies
//! the `word_index` column runs `0..expected_len`. Its `.meta`
//! companion is NOT consumed at build time; the surface invariants
//! it documents (length 512, allowed value set, group structure)
//! are encoded directly in the dedicated module docs in
//! `src/svq1_helper_luts.rs` + lib tests.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const CODEBOOK_L0L3_BYTES: usize = 23004;
const CODEBOOK_DESCRIPTOR_BYTES: usize = 36;
const BLOCK_SHAPE_LUT_LEN: usize = 16;
const BLOCK_SHAPE_LUT_OFFSET: usize = 0x14; // 20

/// Length of the saturating-clip helper LUT — 768 bytes, per
/// `tables/clip_lut.meta` (`byte_length: 768`).
const CLIP_LUT_BYTES: usize = 768;

/// Length of the bit-position / bit-mask helper LUT — 16 bytes, per
/// `tables/svc_bitmask_lut.meta` (`byte_length: 16`).
const BITMASK_LUT_BYTES: usize = 16;

/// Length of the u16-LE parameter table — 512 records of 2 bytes
/// each = 1024 bytes total, per `tables/u16_param_table.meta`
/// (`byte_length: 1024`, `record_count: 512`, `record_size_bytes: 2`).
const U16_PARAM_TABLE_WORDS: usize = 512;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let tables_dir = Path::new(&manifest_dir).join("tables");

    let codebook_csv = tables_dir.join("codebook-l0l3.csv");
    let descriptor_csv = tables_dir.join("codebook-descriptor.csv");
    let l4_meta = tables_dir.join("codebook-l4.meta");
    let l5_meta = tables_dir.join("codebook-l5.meta");
    let clip_csv = tables_dir.join("clip_lut.csv");
    let bitmask_csv = tables_dir.join("svc_bitmask_lut.csv");
    let u16_param_csv = tables_dir.join("u16_param_table.csv");

    println!("cargo:rerun-if-changed={}", codebook_csv.display());
    println!("cargo:rerun-if-changed={}", descriptor_csv.display());
    println!("cargo:rerun-if-changed={}", l4_meta.display());
    println!("cargo:rerun-if-changed={}", l5_meta.display());
    println!("cargo:rerun-if-changed={}", clip_csv.display());
    println!("cargo:rerun-if-changed={}", bitmask_csv.display());
    println!("cargo:rerun-if-changed={}", u16_param_csv.display());

    let l0l3 = parse_signed_csv(&codebook_csv, CODEBOOK_L0L3_BYTES);
    let descriptor = parse_signed_csv(&descriptor_csv, CODEBOOK_DESCRIPTOR_BYTES);
    let absent_l4 = parse_absent_meta(&l4_meta, 4, "16x8", 128, 12288);
    let absent_l5 = parse_absent_meta(&l5_meta, 5, "16x16", 256, 24576);
    let clip = parse_unsigned_csv(&clip_csv, CLIP_LUT_BYTES);
    let bitmask = parse_unsigned_csv(&bitmask_csv, BITMASK_LUT_BYTES);
    let u16_param = parse_u16_csv(&u16_param_csv, U16_PARAM_TABLE_WORDS);

    let out_dir: PathBuf = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .expect("OUT_DIR set by cargo");
    let out_path = out_dir.join("svq1_codebook_data.rs");
    let mut f = fs::File::create(&out_path)
        .unwrap_or_else(|e| panic!("create {}: {e}", out_path.display()));

    writeln!(
        f,
        "// AUTO-GENERATED by build.rs from tables/codebook-l0l3.csv +\n\
         // tables/codebook-descriptor.csv +\n\
         // tables/codebook-l{{4,5}}.meta (mirrored from docs/video/svq1/tables/).\n\
         // DO NOT EDIT — re-run cargo build to regenerate."
    )
    .unwrap();

    emit_i8_array(&mut f, "SVQ1_CODEBOOK_L0L3_BYTES", &l0l3);
    emit_descriptor(&mut f, &descriptor);
    emit_absent_record(&mut f, "SVQ1_L4_ABSENCE", &absent_l4);
    emit_absent_record(&mut f, "SVQ1_L5_ABSENCE", &absent_l5);
    emit_clip_lut(&mut f, &clip);
    emit_bitmask_lut(&mut f, &bitmask);
    emit_u16_param_table(&mut f, &u16_param);
}

/// Emit `SVQ1_CLIP_LUT: [u8; 768]` — the saturating-clip helper LUT
/// from `tables/clip_lut.csv`. Source region per `tables/clip_lut.meta`:
/// file offset `0x5a100..0x5a400`, VMA `0x67dca100..0x67dca400`,
/// section `.rdata`. Used by the codec's interpolation /
/// overflow-saturation paths; NOT a VQ codebook.
fn emit_clip_lut(f: &mut fs::File, data: &[u8]) {
    assert_eq!(data.len(), CLIP_LUT_BYTES);
    writeln!(
        f,
        "\n/// SVQ1_CLIP_LUT — 768-byte saturating-clip helper LUT\n\
         /// from tables/clip_lut.csv. Source region per\n\
         /// tables/clip_lut.meta: file offset 0x5a100..0x5a400, VMA\n\
         /// 0x67dca100..0x67dca400, section .rdata. NOT a VQ codebook.\n\
         pub static SVQ1_CLIP_LUT: [u8; {len}] = [",
        len = data.len()
    )
    .unwrap();
    let mut buf = String::new();
    for (i, b) in data.iter().enumerate() {
        buf.push_str(&format!("0x{:02x}, ", b));
        if (i + 1) % 12 == 0 {
            writeln!(f, "    {}", buf.trim_end()).unwrap();
            buf.clear();
        }
    }
    if !buf.is_empty() {
        writeln!(f, "    {}", buf.trim_end()).unwrap();
    }
    writeln!(f, "];").unwrap();
}

/// Emit `SVQ1_BITMASK_LUT: [u8; 16]` — bit-position / bit-mask helper
/// LUT from `tables/svc_bitmask_lut.csv`. Source region per
/// `tables/svc_bitmask_lut.meta`: file offset `0x5c1c4..0x5c1d4`, VMA
/// `0x67dcc1c4..0x67dcc1d4`, section `.rdata`. First 8 entries are
/// bit masks (`0x80, 0x40, ..., 0x01`); last 8 are their one's
/// complements (`0x7f, 0xbf, ..., 0xfe`).
fn emit_bitmask_lut(f: &mut fs::File, data: &[u8]) {
    assert_eq!(data.len(), BITMASK_LUT_BYTES);
    writeln!(
        f,
        "\n/// SVQ1_BITMASK_LUT — 16-byte bit-position / bit-mask helper\n\
         /// LUT from tables/svc_bitmask_lut.csv. Source region per\n\
         /// tables/svc_bitmask_lut.meta: file offset 0x5c1c4..0x5c1d4,\n\
         /// VMA 0x67dcc1c4..0x67dcc1d4, section .rdata. First 8 entries\n\
         /// are bit masks (0x80, 0x40, ..., 0x01); last 8 are their\n\
         /// one's complements (0x7f, 0xbf, ..., 0xfe).\n\
         pub static SVQ1_BITMASK_LUT: [u8; {len}] = [",
        len = data.len()
    )
    .unwrap();
    let mut buf = String::new();
    for (i, b) in data.iter().enumerate() {
        buf.push_str(&format!("0x{:02x}, ", b));
        if (i + 1) % 8 == 0 {
            writeln!(f, "    {}", buf.trim_end()).unwrap();
            buf.clear();
        }
    }
    if !buf.is_empty() {
        writeln!(f, "    {}", buf.trim_end()).unwrap();
    }
    writeln!(f, "];").unwrap();
}

/// Emit `SVQ1_U16_PARAM_TABLE: [u16; 512]` — the 1024-byte u16-LE
/// parameter table from `tables/u16_param_table.csv`. Source region
/// per `tables/u16_param_table.meta`: file offset `0x59d00..0x5a100`,
/// VMA `0x67dc9d00..0x67dca100`, section `.rdata`. Sits immediately
/// below the saturating-clip LUT (clip starts at `0x5a100`).
fn emit_u16_param_table(f: &mut fs::File, data: &[u16]) {
    assert_eq!(data.len(), U16_PARAM_TABLE_WORDS);
    writeln!(
        f,
        "\n/// SVQ1_U16_PARAM_TABLE — 1024-byte u16-LE parameter table\n\
         /// from tables/u16_param_table.csv. Source region per\n\
         /// tables/u16_param_table.meta: file offset 0x59d00..0x5a100,\n\
         /// VMA 0x67dc9d00..0x67dca100, section .rdata. 512 records,\n\
         /// 2 bytes each, u16 LE. Adjacent to (immediately below) the\n\
         /// saturating-clip LUT at 0x5a100. NOT a VQ codebook.\n\
         pub static SVQ1_U16_PARAM_TABLE: [u16; {len}] = [",
        len = data.len()
    )
    .unwrap();
    let mut buf = String::new();
    for (i, w) in data.iter().enumerate() {
        buf.push_str(&format!("0x{:04x}, ", w));
        if (i + 1) % 8 == 0 {
            writeln!(f, "    {}", buf.trim_end()).unwrap();
            buf.clear();
        }
    }
    if !buf.is_empty() {
        writeln!(f, "    {}", buf.trim_end()).unwrap();
    }
    writeln!(f, "];").unwrap();
}

/// Parse a `word_index,file_offset_hex,vma_hex,value_u16,value_hex`
/// CSV and return the `value_u16` column as a `Vec<u16>` of exactly
/// `expected_len` entries. The `word_index` column must run
/// `0..expected_len` with no gaps. Sister of `parse_unsigned_csv`
/// for the 16-bit-wide u16-LE parameter table.
fn parse_u16_csv(path: &Path, expected_len: usize) -> Vec<u16> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out: Vec<u16> = Vec::with_capacity(expected_len);
    for (lineno, line) in text.lines().enumerate() {
        if lineno == 0 {
            // header row: word_index,file_offset_hex,vma_hex,value_u16,value_hex
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        assert!(
            cols.len() == 5,
            "{}:{} expected 5 columns, got {} ({:?})",
            path.display(),
            lineno + 1,
            cols.len(),
            line
        );
        let word_index: usize = cols[0].parse().unwrap_or_else(|e| {
            panic!(
                "{}:{} parse word_index {:?}: {e}",
                path.display(),
                lineno + 1,
                cols[0]
            )
        });
        assert_eq!(
            word_index,
            out.len(),
            "{}:{} word_index gap (expected {}, got {})",
            path.display(),
            lineno + 1,
            out.len(),
            word_index
        );
        let value_u16: u32 = cols[3].parse().unwrap_or_else(|e| {
            panic!(
                "{}:{} parse value_u16 {:?}: {e}",
                path.display(),
                lineno + 1,
                cols[3]
            )
        });
        assert!(
            value_u16 <= 0xffff,
            "{}:{} value_u16 {} out of u16 range",
            path.display(),
            lineno + 1,
            value_u16
        );
        out.push(value_u16 as u16);
    }
    assert_eq!(
        out.len(),
        expected_len,
        "{} produced {} words; expected {} per the docs/video/svq1/tables/u16_param_table.meta size arithmetic",
        path.display(),
        out.len(),
        expected_len
    );
    out
}

/// Parse a `byte_index,file_offset_hex,vma_hex,value_unsigned,value_hex`
/// CSV and return the `value_unsigned` column as a `Vec<u8>` of exactly
/// `expected_len` entries. The `byte_index` column must run
/// `0..expected_len` with no gaps. Sister of `parse_signed_csv` for the
/// helper-LUT CSVs.
fn parse_unsigned_csv(path: &Path, expected_len: usize) -> Vec<u8> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out: Vec<u8> = Vec::with_capacity(expected_len);
    for (lineno, line) in text.lines().enumerate() {
        if lineno == 0 {
            // header row: byte_index,file_offset_hex,vma_hex,value_unsigned,value_hex
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        assert!(
            cols.len() == 5,
            "{}:{} expected 5 columns, got {} ({:?})",
            path.display(),
            lineno + 1,
            cols.len(),
            line
        );
        let byte_index: usize = cols[0].parse().unwrap_or_else(|e| {
            panic!(
                "{}:{} parse byte_index {:?}: {e}",
                path.display(),
                lineno + 1,
                cols[0]
            )
        });
        assert_eq!(
            byte_index,
            out.len(),
            "{}:{} byte_index gap (expected {}, got {})",
            path.display(),
            lineno + 1,
            out.len(),
            byte_index
        );
        let value_unsigned: u16 = cols[3].parse().unwrap_or_else(|e| {
            panic!(
                "{}:{} parse value_unsigned {:?}: {e}",
                path.display(),
                lineno + 1,
                cols[3]
            )
        });
        assert!(
            value_unsigned <= 0xff,
            "{}:{} value_unsigned {} out of u8 range",
            path.display(),
            lineno + 1,
            value_unsigned
        );
        out.push(value_unsigned as u8);
    }
    assert_eq!(
        out.len(),
        expected_len,
        "{} produced {} bytes; expected {} per the docs/video/svq1/tables/*.meta size arithmetic",
        path.display(),
        out.len(),
        expected_len
    );
    out
}

/// Parsed view of a `codebook-lN.meta` ABSENT record. Only the scalar
/// numeric / status keys are kept; the multi-line `resolution` /
/// `evidence_rvas` YAML-block scalars are skipped at parse time and are
/// documented in their full form in `docs/video/svq1/tables/`.
#[derive(Debug)]
struct AbsentLevelMeta {
    level: u8,
    block_size: String,
    canonical_vector_len_bytes: u32,
    canonical_6stage_intra_or_inter_bytes: u32,
}

/// Parse the `codebook-lN.meta` YAML-lite record at `path` and verify
/// it matches the per-level invariants we expect at this build (the
/// `level` integer, the `block_size` string, the canonical per-vector
/// and per-half byte counts) AND that `status: ABSENT`. A future docs
/// revision that quietly changes any of these — say, by flipping
/// `status` to `present` or changing the canonical vector length —
/// fails the build before any code that depends on the ABSENT
/// guarantee can run.
fn parse_absent_meta(
    path: &Path,
    expected_level: u8,
    expected_block_size: &str,
    expected_vector_len: u32,
    expected_per_half: u32,
) -> AbsentLevelMeta {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut level: Option<u8> = None;
    let mut block_size: Option<String> = None;
    let mut vec_len: Option<u32> = None;
    let mut per_half: Option<u32> = None;
    let mut status: Option<String> = None;
    // Skip-mode flag: once a `key: |` line is seen, every subsequent
    // line until the next dedented top-level key is part of a YAML
    // block scalar and must not be parsed as a scalar key/value.
    let mut in_block_scalar = false;
    for (lineno, raw) in text.lines().enumerate() {
        // A line whose first character is whitespace is body of an
        // in-progress block scalar (or a blank line in one); skip.
        if in_block_scalar {
            if raw.starts_with(' ') || raw.starts_with('\t') || raw.is_empty() {
                continue;
            }
            in_block_scalar = false;
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => panic!(
                "{}:{} expected `key: value`, got {:?}",
                path.display(),
                lineno + 1,
                line
            ),
        };
        if value == "|" {
            // Multi-line block scalar follows; skip its body lines.
            in_block_scalar = true;
            continue;
        }
        match key {
            "level" => {
                level = Some(value.parse().unwrap_or_else(|e| {
                    panic!(
                        "{}:{} parse level {:?}: {e}",
                        path.display(),
                        lineno + 1,
                        value
                    )
                }));
            }
            "block_size" => {
                block_size = Some(value.to_string());
            }
            "canonical_vector_len_bytes" => {
                vec_len = Some(value.parse().unwrap_or_else(|e| {
                    panic!(
                        "{}:{} parse canonical_vector_len_bytes {:?}: {e}",
                        path.display(),
                        lineno + 1,
                        value
                    )
                }));
            }
            "canonical_6stage_intra_or_inter_bytes" => {
                per_half = Some(value.parse().unwrap_or_else(|e| {
                    panic!(
                        "{}:{} parse canonical_6stage_intra_or_inter_bytes {:?}: {e}",
                        path.display(),
                        lineno + 1,
                        value
                    )
                }));
            }
            "status" => {
                status = Some(value.to_string());
            }
            // Other documented keys (source, source_sha256, size_bytes,
            // image_base_hex) are present in the meta but are not
            // consumed at build time; the docs collaborator owns their
            // provenance documentation.
            _ => {}
        }
    }
    let level = level.unwrap_or_else(|| panic!("{}: missing `level`", path.display()));
    let block_size =
        block_size.unwrap_or_else(|| panic!("{}: missing `block_size`", path.display()));
    let vec_len = vec_len
        .unwrap_or_else(|| panic!("{}: missing `canonical_vector_len_bytes`", path.display()));
    let per_half = per_half.unwrap_or_else(|| {
        panic!(
            "{}: missing `canonical_6stage_intra_or_inter_bytes`",
            path.display()
        )
    });
    let status = status.unwrap_or_else(|| panic!("{}: missing `status`", path.display()));
    assert_eq!(
        level,
        expected_level,
        "{}: level mismatch (got {}, expected {})",
        path.display(),
        level,
        expected_level
    );
    assert_eq!(
        block_size,
        expected_block_size,
        "{}: block_size mismatch (got {:?}, expected {:?})",
        path.display(),
        block_size,
        expected_block_size
    );
    assert_eq!(
        vec_len,
        expected_vector_len,
        "{}: canonical_vector_len_bytes mismatch (got {}, expected {})",
        path.display(),
        vec_len,
        expected_vector_len
    );
    assert_eq!(
        per_half,
        expected_per_half,
        "{}: canonical_6stage_intra_or_inter_bytes mismatch (got {}, expected {})",
        path.display(),
        per_half,
        expected_per_half
    );
    assert_eq!(
        status,
        "ABSENT",
        "{}: status must be ABSENT (got {:?}); the Svq1Level::codebook_bytes_per_half None branch \
         depends on this invariant",
        path.display(),
        status
    );
    AbsentLevelMeta {
        level,
        block_size,
        canonical_vector_len_bytes: vec_len,
        canonical_6stage_intra_or_inter_bytes: per_half,
    }
}

fn emit_absent_record(f: &mut fs::File, name: &str, m: &AbsentLevelMeta) {
    writeln!(
        f,
        "\n/// {name} — typed mirror of tables/codebook-l{level}.meta\n\
         /// status=ABSENT record. The value is asserted at build time\n\
         /// against the expected level / block_size / vector-length /\n\
         /// per-half byte count; consumers can rely on the absence\n\
         /// invariant without re-parsing the meta themselves.\n\
         pub const {name}: crate::svq1_codebook::Svq1AbsentLevelRecord =\n\
             crate::svq1_codebook::Svq1AbsentLevelRecord {{\n\
                 level: {level},\n\
                 block_size: {block_size:?},\n\
                 canonical_vector_len_bytes: {vec_len},\n\
                 canonical_6stage_intra_or_inter_bytes: {per_half},\n\
             }};",
        name = name,
        level = m.level,
        block_size = m.block_size,
        vec_len = m.canonical_vector_len_bytes,
        per_half = m.canonical_6stage_intra_or_inter_bytes,
    )
    .unwrap();
}

/// Parse a `byte_index,...,value_signed,value_hex` CSV and return the
/// `value_signed` column as a `Vec<i8>` of exactly `expected_len`
/// entries. The `byte_index` column must run `0..expected_len` with
/// no gaps.
fn parse_signed_csv(path: &Path, expected_len: usize) -> Vec<i8> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut out: Vec<i8> = Vec::with_capacity(expected_len);
    for (lineno, line) in text.lines().enumerate() {
        if lineno == 0 {
            // header row: byte_index,file_offset_hex,vma_hex,value_signed,value_hex
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        assert!(
            cols.len() == 5,
            "{}:{} expected 5 columns, got {} ({:?})",
            path.display(),
            lineno + 1,
            cols.len(),
            line
        );
        let byte_index: usize = cols[0].parse().unwrap_or_else(|e| {
            panic!(
                "{}:{} parse byte_index {:?}: {e}",
                path.display(),
                lineno + 1,
                cols[0]
            )
        });
        assert_eq!(
            byte_index,
            out.len(),
            "{}:{} byte_index gap (expected {}, got {})",
            path.display(),
            lineno + 1,
            out.len(),
            byte_index
        );
        let value_signed: i16 = cols[3].parse().unwrap_or_else(|e| {
            panic!(
                "{}:{} parse value_signed {:?}: {e}",
                path.display(),
                lineno + 1,
                cols[3]
            )
        });
        assert!(
            (-128..=127).contains(&value_signed),
            "{}:{} value_signed {} out of i8 range",
            path.display(),
            lineno + 1,
            value_signed
        );
        out.push(value_signed as i8);
    }
    assert_eq!(
        out.len(),
        expected_len,
        "{} produced {} bytes; expected {} per the docs/video/svq1/tables/*.meta size arithmetic",
        path.display(),
        out.len(),
        expected_len
    );
    out
}

fn emit_i8_array(f: &mut fs::File, name: &str, data: &[i8]) {
    writeln!(
        f,
        "\n/// {name} — bit-exact payload from tables/codebook-l0l3.csv\n\
         /// ({} bytes).\n\
         #[allow(clippy::approx_constant)]\n\
         pub static {name}: [i8; {len}] = [",
        data.len(),
        name = name,
        len = data.len()
    )
    .unwrap();
    let mut buf = String::new();
    for (i, b) in data.iter().enumerate() {
        if i % 16 == 0 && !buf.is_empty() {
            writeln!(f, "    {}", buf.trim_end()).unwrap();
            buf.clear();
        }
        buf.push_str(&format!("{}, ", b));
    }
    if !buf.is_empty() {
        writeln!(f, "    {}", buf.trim_end()).unwrap();
    }
    writeln!(f, "];").unwrap();
}

fn emit_descriptor(f: &mut fs::File, descriptor: &[i8]) {
    assert_eq!(descriptor.len(), CODEBOOK_DESCRIPTOR_BYTES);
    // Re-export as unsigned u8 (level indices + block-shape entries
    // are byte values without a sign interpretation; the source CSV
    // ships them in `value_signed` because that column shape is
    // shared with the L0..L3 codebook export).
    writeln!(
        f,
        "\n/// SVQ1_CODEBOOK_DESCRIPTOR — 36-byte level-descriptor + \n\
         /// block-shape prefix at file offset 0x5d200..0x5d224 of the\n\
         /// reference binary. The block-shape LUT lives at +0x14.\n\
         pub static SVQ1_CODEBOOK_DESCRIPTOR: [u8; {len}] = [",
        len = descriptor.len()
    )
    .unwrap();
    let mut buf = String::new();
    for (i, b) in descriptor.iter().enumerate() {
        if i % 9 == 0 && !buf.is_empty() {
            writeln!(f, "    {}", buf.trim_end()).unwrap();
            buf.clear();
        }
        // descriptor bytes are unsigned in interpretation; cast through
        // u8 mask to preserve raw byte value
        buf.push_str(&format!("0x{:02x}, ", (*b as u8)));
    }
    if !buf.is_empty() {
        writeln!(f, "    {}", buf.trim_end()).unwrap();
    }
    writeln!(f, "];").unwrap();

    // Slice the block-shape LUT (16 bytes at +0x14) into a stand-alone
    // const for ergonomic access at runtime.
    writeln!(
        f,
        "\n/// SVQ1_BLOCK_SHAPE_LUT — the 16-entry block-shape lookup\n\
         /// (values 1..=4) at descriptor offset +0x14. Caps the\n\
         /// quantised block sizes to L=0..L=3 (no entry exceeds 4),\n\
         /// corroborating docs/video/svq1/spec/14.10-codebook-L4.md +\n\
         /// docs/video/svq1/spec/14.11-codebook-L5.md ABSENT findings.\n\
         pub static SVQ1_BLOCK_SHAPE_LUT: [u8; {lut_len}] = [",
        lut_len = BLOCK_SHAPE_LUT_LEN
    )
    .unwrap();
    let mut buf = String::new();
    for (i, b) in descriptor[BLOCK_SHAPE_LUT_OFFSET..].iter().enumerate() {
        buf.push_str(&format!("0x{:02x}, ", (*b as u8)));
        if (i + 1) % 8 == 0 {
            writeln!(f, "    {}", buf.trim_end()).unwrap();
            buf.clear();
        }
    }
    if !buf.is_empty() {
        writeln!(f, "    {}", buf.trim_end()).unwrap();
    }
    writeln!(f, "];").unwrap();
}
