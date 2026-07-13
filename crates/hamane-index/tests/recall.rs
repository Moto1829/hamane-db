//! 再現率テスト (todos/303): 既定パラメータの HNSW が recall@10 ≥ 0.95 を
//! 満たすことを CI で担保する。seed 完全固定で決定的。
//!
//! CI 時間の都合で n は設計 (10k) より小さくしている。大規模な実測は
//! todos/307 のベンチハーネスで行う。

use hamane_core::Metric;
use hamane_index::{search_hnsw, HnswBuilder, HnswParams, SliceSource};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn uniform(n: usize, dim: usize, rng: &mut StdRng) -> Vec<Vec<f32>> {
    (0..n)
        .map(|_| (0..dim).map(|_| rng.random::<f32>() * 2.0 - 1.0).collect())
        .collect()
}

/// ガウス混合クラスタ (近傍が固まる、より現実的な分布)。
fn clustered(n: usize, dim: usize, clusters: usize, rng: &mut StdRng) -> Vec<Vec<f32>> {
    let centers: Vec<Vec<f32>> = (0..clusters)
        .map(|_| (0..dim).map(|_| rng.random::<f32>() * 10.0 - 5.0).collect())
        .collect();
    (0..n)
        .map(|i| {
            let c = &centers[i % clusters];
            // Box-Muller 正規乱数
            (0..dim)
                .map(|d| {
                    let u1: f32 = rng.random::<f32>().max(1e-7);
                    let u2: f32 = rng.random::<f32>();
                    c[d] + (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos() * 0.5
                })
                .collect()
        })
        .collect()
}

fn flat_topk(vecs: &[Vec<f32>], query: &[f32], k: usize, metric: Metric) -> Vec<u32> {
    let mut all: Vec<(f32, u32)> = vecs
        .iter()
        .enumerate()
        .map(|(i, v)| (metric.distance_key(query, v), i as u32))
        .collect();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    all.into_iter().take(k).map(|(_, i)| i).collect()
}

fn measure_recall(
    vecs: &[Vec<f32>],
    queries: &[Vec<f32>],
    metric: Metric,
    params: HnswParams,
    ef: usize,
    k: usize,
) -> f64 {
    let src = SliceSource(vecs);
    let graph = HnswBuilder::build(&src, metric, params);
    let mut hit = 0usize;
    for q in queries {
        let truth = flat_topk(vecs, q, k, metric);
        let approx: Vec<u32> = search_hnsw(&graph, &src, metric, q, k, ef, None)
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        hit += approx.iter().filter(|r| truth.contains(r)).count();
    }
    hit as f64 / (queries.len() * k) as f64
}

#[test]
fn recall_at_10_uniform() {
    let mut rng = StdRng::seed_from_u64(303);
    for dim in [16, 64] {
        let vecs = uniform(4000, dim, &mut rng);
        let queries = uniform(100, dim, &mut rng);
        let recall = measure_recall(&vecs, &queries, Metric::L2, HnswParams::default(), 64, 10);
        assert!(recall >= 0.95, "uniform dim={dim}: recall@10 = {recall:.3}");
    }
}

#[test]
fn recall_at_10_clustered() {
    let mut rng = StdRng::seed_from_u64(1303);
    let vecs = clustered(4000, 32, 20, &mut rng);
    let queries = clustered(100, 32, 20, &mut rng);
    let recall = measure_recall(&vecs, &queries, Metric::L2, HnswParams::default(), 64, 10);
    assert!(recall >= 0.95, "clustered: recall@10 = {recall:.3}");
}

/// Cosine (正規化済みベクトル) と Dot でも recall が出ること (todo 507)。
#[test]
fn recall_at_10_cosine_and_dot() {
    let mut rng = StdRng::seed_from_u64(3307);
    // Cosine 相当: L2 正規化したベクトルの Dot (挿入時正規化と同じ状態)
    let mut vecs = uniform(4000, 32, &mut rng);
    let mut queries = uniform(100, 32, &mut rng);
    for v in vecs.iter_mut().chain(queries.iter_mut()) {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    let recall = measure_recall(&vecs, &queries, Metric::Dot, HnswParams::default(), 64, 10);
    assert!(
        recall >= 0.95,
        "cosine(normalized dot): recall@10 = {recall:.3}"
    );

    // 生の Dot (MIPS)。HNSW は内積では三角不等式が成り立たず本質的に難しいが、
    // 一様データでは実用的な再現率が出ることを確認する
    let mut rng = StdRng::seed_from_u64(4307);
    let vecs = uniform(4000, 32, &mut rng);
    let queries = uniform(100, 32, &mut rng);
    let recall = measure_recall(&vecs, &queries, Metric::Dot, HnswParams::default(), 128, 10);
    assert!(recall >= 0.85, "raw dot (MIPS): recall@10 = {recall:.3}");
}

#[test]
fn recall_improves_with_ef() {
    let mut rng = StdRng::seed_from_u64(2303);
    let vecs = uniform(3000, 32, &mut rng);
    let queries = uniform(60, 32, &mut rng);
    let src = SliceSource(&vecs);
    let graph = HnswBuilder::build(&src, Metric::L2, HnswParams::default());

    let recall_at = |ef: usize| -> f64 {
        let mut hit = 0usize;
        for q in &queries {
            let truth = flat_topk(&vecs, q, 10, Metric::L2);
            let approx: Vec<u32> = search_hnsw(&graph, &src, Metric::L2, q, 10, ef, None)
                .into_iter()
                .map(|(r, _)| r)
                .collect();
            hit += approx.iter().filter(|r| truth.contains(r)).count();
        }
        hit as f64 / (queries.len() * 10) as f64
    };

    let recalls: Vec<f64> = [16, 64, 256].iter().map(|&ef| recall_at(ef)).collect();
    // ef を上げて再現率が下がらないこと (探索実装の破れの検出)
    assert!(
        recalls.windows(2).all(|w| w[1] >= w[0] - 0.01),
        "recall must not degrade with larger ef: {recalls:?}"
    );
    assert!(
        recalls[2] >= 0.99,
        "ef=256 should be near-exact: {recalls:?}"
    );
}
