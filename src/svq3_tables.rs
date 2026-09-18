//! SVQ3 data tables mirrored bit-exact from `docs/video/svq3/tables/`
//! (`tables/svq3-07..09-*.csv`, SHA-256s in `tables/MANIFEST-svq3.sha256`)
//! and emitted at build time by `build.rs`:
//!
//! * [`SVQ3_INTRA4X4_PRED_MODE_PAIRS`] — tables/07, the 25
//!   `(rank_first, rank_second)` records an intra 4×4 macroblock's
//!   prediction-mode pair code selects (spec/07 §6 item 1);
//! * [`SVQ3_INTRA4X4_PRED_MODE_CONTEXT`] — tables/08, the
//!   `[top_context][left_context][rank]` resolution of a rank into a
//!   prediction mode (spec/07 §10.2), `9` = illegal;
//! * [`SVQ3_INTRA_EDGE_FILTER_LIMIT`] — tables/09, the per-quantiser
//!   strength of the intra-picture edge filter (spec/09 §2).
//!
//! The numbers are never retyped by hand: they come straight from the
//! CSVs the docs collaborator staged from the decompressor component.

include!(concat!(env!("OUT_DIR"), "/svq3_tables_data.rs"));

/// The `pred_mode` value tables/08 uses for an illegal
/// `(context, rank)` combination — a bitstream error when reached.
pub const SVQ3_INTRA4X4_PRED_MODE_ILLEGAL: u8 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_enumerate_all_rank_pairs_in_cost_order() {
        // tables/07 validation: all 25 pairs distinct, ranks 0..=4,
        // (0, 0) first, then increasing rank sum.
        let mut seen = [[false; 5]; 5];
        let mut last_sum = 0u8;
        for &(a, b) in SVQ3_INTRA4X4_PRED_MODE_PAIRS.iter() {
            assert!(a <= 4 && b <= 4);
            assert!(!seen[a as usize][b as usize]);
            seen[a as usize][b as usize] = true;
            assert!(a + b >= last_sum);
            last_sum = a + b;
        }
        assert_eq!(SVQ3_INTRA4X4_PRED_MODE_PAIRS[0], (0, 0));
        assert_eq!(SVQ3_INTRA4X4_PRED_MODE_PAIRS[24], (4, 4));
    }

    #[test]
    fn context_rows_are_permutation_prefixes() {
        // tables/08 validation: legal entries distinct in 0..=4, illegal
        // (9) entries trailing; exactly the rows with a 0 context expose
        // fewer than five ranks; row (0, 0) admits only rank 0 → DC.
        for (top, rows) in SVQ3_INTRA4X4_PRED_MODE_CONTEXT.iter().enumerate() {
            for (left, row) in rows.iter().enumerate() {
                let legal = row.iter().take_while(|&&m| m != 9).count();
                assert!(row[legal..].iter().all(|&m| m == 9));
                let mut seen = [false; 5];
                for &m in &row[..legal] {
                    assert!(m <= 4);
                    assert!(!seen[m as usize]);
                    seen[m as usize] = true;
                }
                assert_eq!(legal < 5, top == 0 || left == 0, "row ({top},{left})");
            }
        }
        assert_eq!(SVQ3_INTRA4X4_PRED_MODE_CONTEXT[0][0], [0, 9, 9, 9, 9]);
    }

    #[test]
    fn edge_filter_limit_matches_spec09_table() {
        // spec/09 §2: 0…10 → 0, 11…15 → 1, 16…20 → 2, 21…24 → 3,
        // 25…28 → 4, 29…31 → 5.
        for (q, &limit) in SVQ3_INTRA_EDGE_FILTER_LIMIT.iter().enumerate() {
            let want = match q {
                0..=10 => 0,
                11..=15 => 1,
                16..=20 => 2,
                21..=24 => 3,
                25..=28 => 4,
                _ => 5,
            };
            assert_eq!(limit, want, "quantiser {q}");
        }
    }
}
