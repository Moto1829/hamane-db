//! 統合テスト共通のヘルパ (todo 1301)。
//!
//! 決定的なデータ生成・正解計算・規模の指定をここに集約する。
//! `mod common;` で各テストから読み込む (使わない関数があってもよいので
//! `#![allow(dead_code)]` を付ける)。

#![allow(dead_code)]

use std::time::Instant;

use hamane::{Metric, Record};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// 決定的な乱数ベクトル。
pub fn random_vec(rng: &mut StdRng, dim: usize) -> Vec<f32> {
    (0..dim).map(|_| rng.random::<f32>() * 2.0 - 1.0).collect()
}

/// 決定的なデータセット (id, vector) を作る。
pub fn dataset(n: u64, dim: usize, seed: u64) -> Vec<(u64, Vec<f32>)> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n).map(|i| (i, random_vec(&mut rng, dim))).collect()
}

/// データセットから Record 列を作る (メタデータなし)。
pub fn records(data: &[(u64, Vec<f32>)]) -> Vec<Record> {
    data.iter()
        .map(|(id, v)| Record::new(*id, v.clone()))
        .collect()
}

/// クラスタ構造のある決定的なデータセット。
///
/// **一様乱数の高次元ベクトルは HNSW にとって最悪ケース**で、次元の呪いにより
/// 距離が集中して近似探索が効きにくい (dim=128・3 万件・既定 ef=64 で
/// recall@10 = 0.73、ef=512 まで上げてようやく 0.98)。実際の埋め込みベクトルは
/// クラスタ構造を持つので、再現率の検証にはこちらを使う
/// (SIFT の実測 0.98 と同程度になる)。
///
/// クラスタは**適度に重なる**ようにしてある (中心 ±5、ゆらぎ ±2)。
/// ゆらぎを小さくしてクラスタを固く分離すると、真の近傍同士がほぼ等距離になり
/// 量子化の誤差が識別能力を上回る (dim=768 + PQ で recall が 0.83 まで落ちる)。
/// それは PQ の性質であって実装の不具合ではないが、テストとしては
/// 実データに近い「そこそこ重なるクラスタ」を使う。
pub fn clustered_dataset(n: u64, dim: usize, seed: u64) -> Vec<(u64, Vec<f32>)> {
    let mut rng = StdRng::seed_from_u64(seed);
    let n_clusters = ((n as f64).sqrt() as usize).clamp(8, 512);
    let centers: Vec<Vec<f32>> = (0..n_clusters)
        .map(|_| (0..dim).map(|_| rng.random::<f32>() * 10.0 - 5.0).collect())
        .collect();
    (0..n)
        .map(|i| {
            let c = &centers[rng.random_range(0..n_clusters)];
            let v = c
                .iter()
                .map(|x| x + rng.random::<f32>() * 4.0 - 2.0)
                .collect();
            (i, v)
        })
        .collect()
}

/// 総当たりの正解 top-k (metric に応じたキーの昇順、id でタイブレーク)。
pub fn flat_topk<'a>(
    data: impl Iterator<Item = (u64, &'a Vec<f32>)>,
    query: &[f32],
    k: usize,
    metric: Metric,
    filter: impl Fn(u64) -> bool,
) -> Vec<u64> {
    let mut all: Vec<(f32, u64)> = data
        .filter(|(id, _)| filter(*id))
        .map(|(id, v)| (metric.distance_key(query, v), id))
        .collect();
    all.sort_by(|a, b| a.partial_cmp(b).expect("keys are finite"));
    all.into_iter().take(k).map(|(_, id)| id).collect()
}

/// 実際の結果と正解の一致率 (recall@k)。
pub fn recall(actual: &[u64], truth: &[u64]) -> f64 {
    if truth.is_empty() {
        return 1.0;
    }
    let hit = actual.iter().filter(|id| truth.contains(id)).count();
    hit as f64 / truth.len() as f64
}

/// 大規模テストの件数。`HAMANE_SCALE_N` で上書きできる (既定 100,000)。
///
/// nightly ワークフローは 1,000,000 を指定する。ローカルで軽く回したいときは
/// `HAMANE_SCALE_N=20000 cargo test --release -- --ignored` のように下げる。
pub fn scale_n() -> u64 {
    std::env::var("HAMANE_SCALE_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000)
}

/// 大規模テストの所要時間ログ。`--nocapture` で見える。
pub struct Phase {
    label: &'static str,
    started: Instant,
}

impl Phase {
    pub fn start(label: &'static str) -> Self {
        eprintln!("[scale] {label} ...");
        Self {
            label,
            started: Instant::now(),
        }
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        eprintln!(
            "[scale] {} done in {:.1?}",
            self.label,
            self.started.elapsed()
        );
    }
}

/// ディレクトリ配下のファイル合計サイズ (バイト)。ディスク収束の確認に使う。
pub fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// DB ディレクトリ配下から指定名のファイルを集める。
pub fn segment_files(root: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().map(|f| f == name).unwrap_or(false) {
                out.push(path);
            }
        }
    }
    out
}
