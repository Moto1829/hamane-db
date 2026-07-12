use std::cmp::Ordering;
use std::collections::BinaryHeap;

use hamane_core::{Filter, Id, Metadata, Metric};

use crate::Hit;

/// 最大ヒープ用エントリ。distance_key が大きい (= 遠い) ものがヒープの頂点に来る。
struct HeapEntry {
    key: f32,
    id: Id,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.id == other.id
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // distance_key は NaN を含まない前提 (挿入時に検証済み)。
        // 同キーは id で順序を安定させる。
        self.key
            .partial_cmp(&other.key)
            .unwrap_or(Ordering::Equal)
            .then_with(|| self.id.cmp(&other.id))
    }
}

/// ブルートフォースの正確検索。
///
/// 候補イテレータを全走査し、フィルタを満たすものから上位 k 件を
/// サイズ k の最大ヒープで選抜する。O(n log k)。
/// 結果は近い順にソートして返す。
pub fn search_flat<'a>(
    candidates: impl Iterator<Item = (Id, &'a [f32], &'a Metadata)>,
    query: &[f32],
    k: usize,
    metric: Metric,
    filter: Option<&Filter>,
) -> Vec<Hit> {
    if k == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(k + 1);
    for (id, vector, meta) in candidates {
        if let Some(f) = filter {
            if !f.matches(meta) {
                continue;
            }
        }
        let key = metric.distance_key(query, vector);
        if heap.len() < k {
            heap.push(HeapEntry { key, id });
        } else {
            // 同キーは id 昇順を優先し、走査順に依らず決定的な結果にする
            let top = heap.peek().expect("heap is non-empty");
            if key < top.key || (key == top.key && id < top.id) {
                heap.pop();
                heap.push(HeapEntry { key, id });
            }
        }
    }
    let mut entries = heap.into_sorted_vec();
    entries
        .drain(..)
        .map(|e| Hit {
            id: e.id,
            score: metric.score_from_key(e.key),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hamane_core::Filter;

    fn dataset() -> Vec<(Id, Vec<f32>, Metadata)> {
        // 1 次元に並べた点: id i は座標 i
        (0..10u64)
            .map(|i| {
                let mut meta = Metadata::new();
                meta.insert("even".into(), (i % 2 == 0).into());
                (i, vec![i as f32, 0.0], meta)
            })
            .collect()
    }

    fn run(
        data: &[(Id, Vec<f32>, Metadata)],
        query: &[f32],
        k: usize,
        metric: Metric,
        filter: Option<&Filter>,
    ) -> Vec<Hit> {
        search_flat(
            data.iter().map(|(id, v, m)| (*id, v.as_slice(), m)),
            query,
            k,
            metric,
            filter,
        )
    }

    #[test]
    fn top_k_l2_sorted_by_distance() {
        let data = dataset();
        let hits = run(&data, &[3.2, 0.0], 3, Metric::L2, None);
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![3, 4, 2]);
        // score は近い順に単調非減少 (L2)
        assert!(hits[0].score <= hits[1].score && hits[1].score <= hits[2].score);
        assert!((hits[0].score - 0.2).abs() < 1e-5);
    }

    #[test]
    fn filter_restricts_candidates() {
        let data = dataset();
        let filter = Filter::eq("even", true);
        let hits = run(&data, &[3.2, 0.0], 3, Metric::L2, Some(&filter));
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![4, 2, 6]);
    }

    #[test]
    fn k_larger_than_matches() {
        let data = dataset();
        let hits = run(&data, &[0.0, 0.0], 100, Metric::L2, None);
        assert_eq!(hits.len(), 10);
        assert_eq!(hits[0].id, 0);
    }

    #[test]
    fn k_zero_returns_empty() {
        let data = dataset();
        assert!(run(&data, &[0.0, 0.0], 0, Metric::L2, None).is_empty());
    }

    #[test]
    fn dot_prefers_larger_inner_product() {
        let data = vec![
            (1u64, vec![1.0, 0.0], Metadata::new()),
            (2u64, vec![10.0, 0.0], Metadata::new()),
            (3u64, vec![-5.0, 0.0], Metadata::new()),
        ];
        let hits = run(&data, &[1.0, 0.0], 2, Metric::Dot, None);
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![2, 1]);
        assert!((hits[0].score - 10.0).abs() < 1e-5);
    }
}
