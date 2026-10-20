//! Reciprocal rank fusion: one ranking from several, for hybrid search.
//!
//! Ranks as the `.knode` format recommends (spec section 19.6): an item scores `1 / (60 + rank)`
//! in every list it is in, and equal scores go to the better rank, then the lower item. Hybrid
//! search over a space is [`knode_format::Space::hybrid_search`]; this is for fusing other
//! lists, such as results from several spaces.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// The constant of reciprocal rank fusion the format recommends. Larger values flatten the
/// weight of top ranks.
pub const RRF_K: usize = 60;

/// Fuses a full-text ranking and a vector ranking, each best first, with reciprocal rank fusion:
/// an item scores `1 / (k + rank)` in every list it appears in (rank from 1), summed. Returns
/// every item once with its score, best first; equal scores go to the item with the better rank
/// in any list, then to the lower item (for node ids, the older node).
pub fn rrf<K: Eq + Hash + Clone + Ord>(
    fts_ranks: &[K],
    vector_ranks: &[K],
    k: usize,
) -> Vec<(K, f64)> {
    rrf_many(&[fts_ranks, vector_ranks], k)
}

/// [`rrf`] over any number of rankings. An item repeated within one list counts at its best
/// rank only.
pub fn rrf_many<K: Eq + Hash + Clone + Ord>(rankings: &[&[K]], k: usize) -> Vec<(K, f64)> {
    // item -> (score, best rank)
    let mut scores: HashMap<&K, (f64, usize)> = HashMap::new();
    for ranking in rankings {
        let mut in_this: HashSet<&K> = HashSet::new();
        for (index, item) in ranking.iter().enumerate() {
            if !in_this.insert(item) {
                continue;
            }
            let rank = index + 1;
            let entry = scores.entry(item).or_insert((0.0, usize::MAX));
            entry.0 += 1.0 / (k + rank) as f64;
            entry.1 = entry.1.min(rank);
        }
    }
    let mut fused: Vec<_> = scores.into_iter().collect();
    fused.sort_by(|a, b| {
        b.1.0
            .total_cmp(&a.1.0)
            .then(a.1.1.cmp(&b.1.1))
            .then(a.0.cmp(b.0))
    });
    fused
        .into_iter()
        .map(|(item, (score, _))| (item.clone(), score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_in_both_lists_rise() {
        let fts = ["a", "b", "c"];
        let vec = ["c", "d", "a"];
        let fused = rrf(&fts, &vec, RRF_K);
        let order: Vec<_> = fused.iter().map(|(k, _)| *k).collect();
        assert_eq!(order, vec!["a", "c", "b", "d"]);
        let a = 1.0 / 61.0 + 1.0 / 63.0;
        assert!((fused[0].1 - a).abs() < 1e-12);
    }

    #[test]
    fn matches_the_example_of_the_specification() {
        // Words (A, B, C) and meaning (C, A, D), L = 3: A, C, B.
        let fused = rrf(&["A", "B", "C"], &["C", "A", "D"], RRF_K);
        let order: Vec<_> = fused.iter().map(|(k, _)| *k).collect();
        assert_eq!(order, ["A", "C", "B", "D"]);
        let rounded: Vec<f64> = fused.iter().map(|(_, s)| (s * 1e5).round() / 1e5).collect();
        assert_eq!(rounded, [0.03252, 0.03227, 0.01613, 0.01587]);
    }

    #[test]
    fn ties_follow_best_rank_then_the_lower_item() {
        let fused = rrf(&["y", "x"], &["x", "y"], 60);
        assert_eq!(fused[0].1, fused[1].1);
        assert_eq!(fused[0].0, "x");
        let fused = rrf(&["q"], &["p"], 60);
        assert_eq!(fused[0].0, "p");
        assert_eq!(fused[0].1, fused[1].1);
    }

    #[test]
    fn one_list_empty_keeps_the_other_order() {
        let fused = rrf::<u32>(&[], &[3, 1, 2], 60);
        assert_eq!(
            fused.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![3, 1, 2]
        );
        assert!(rrf::<u32>(&[], &[], 60).is_empty());
    }

    #[test]
    fn duplicates_count_once_per_list() {
        let fused = rrf(&["a", "a", "b"], &[], 60);
        assert_eq!(fused.len(), 2);
        assert!((fused[0].1 - 1.0 / 61.0).abs() < 1e-12);
        assert!((fused[1].1 - 1.0 / 63.0).abs() < 1e-12);
    }
}
