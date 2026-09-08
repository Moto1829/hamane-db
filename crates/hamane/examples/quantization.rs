//! 量子化・索引構成の比較 (todo 1302)。
//!
//! 実行: `cargo run --release --example quantization [件数]`
//!
//! **同じデータを構成ごとに作り直して**、再現率・検索時間・構築時間・
//! ディスクサイズを表にします。「どれを選ぶか」を自分のデータで判断するための
//! ひな形として使ってください (SIFT での実測は docs/benchmarks.md)。
//!
//! 許可される組み合わせは docs/spec の「索引と量子化の組み合わせ」を参照。

use std::time::Instant;

use hamane::{
    CollectionConfig, Database, IndexKind, Metric, Quantization, Record, StoreOptions, SyncPolicy,
};

const DIM: usize = 64;

struct Setup {
    label: &'static str,
    index: IndexKind,
    quantization: Option<Quantization>,
    pq_nbits: u8,
    opq: bool,
    note: &'static str,
}

fn setups() -> Vec<Setup> {
    vec![
        Setup {
            label: "f32 (既定)",
            index: IndexKind::Hnsw,
            quantization: None,
            pq_nbits: 8,
            opq: false,
            note: "基準。メモリは一番使う",
        },
        Setup {
            label: "SQ8",
            index: IndexKind::Hnsw,
            quantization: Some(Quantization::Sq8),
            pq_nbits: 8,
            opq: false,
            note: "1/4 圧縮。中規模ならまずこれ",
        },
        Setup {
            label: "PQ 8bit",
            index: IndexKind::Hnsw,
            quantization: Some(Quantization::Pq { m: None }),
            pq_nbits: 8,
            opq: false,
            note: "強い圧縮。億件規模のメモリ削減向け",
        },
        Setup {
            label: "PQ 4bit (m 2倍)",
            index: IndexKind::Hnsw,
            quantization: Some(Quantization::Pq { m: Some(DIM / 2) }),
            pq_nbits: 4,
            opq: false,
            note: "同じコード長で構築が速い",
        },
        Setup {
            label: "OPQ (回転付き PQ)",
            index: IndexKind::Hnsw,
            quantization: Some(Quantization::Pq { m: None }),
            pq_nbits: 8,
            opq: true,
            note: "次元の並びに意味がないデータで効く",
        },
        Setup {
            label: "IVF",
            index: IndexKind::Ivf,
            quantization: None,
            pq_nbits: 8,
            opq: false,
            note: "HNSW グラフを書かない。ディスク最小",
        },
        Setup {
            label: "IVF-PQ",
            index: IndexKind::IvfPq,
            quantization: Some(Quantization::Pq { m: None }),
            pq_nbits: 8,
            opq: false,
            note: "枝刈り + 強圧縮",
        },
    ]
}

fn main() -> hamane::Result<()> {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(10_000);

    println!("{n} 件 × dim={DIM} で構成を比較します\n");
    let data = clustered_data(n, DIM);
    let queries: Vec<Vec<f32>> = (0..20)
        .map(|i| data[(i * 397) % data.len()].1.clone())
        .collect();

    // 正解 (総当たり)
    let truth: Vec<Vec<u64>> = queries.iter().map(|q| flat_topk(&data, q, 10)).collect();

    println!(
        "{:<20} {:>8} {:>10} {:>10} {:>9}  メモ",
        "構成", "recall@10", "検索(µs)", "構築(ms)", "ディスク"
    );
    println!("{}", "-".repeat(88));

    for setup in setups() {
        let dir = std::env::temp_dir().join(format!(
            "hamane-example-quant-{}-{}",
            std::process::id(),
            setup.label.replace([' ', '(', ')', '/'], "_")
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let db = Database::open_with_options(
            &dir,
            StoreOptions {
                sync: SyncPolicy::EveryN(u32::MAX), // 比較なので fsync は外す
                index: setup.index,
                quantization: setup.quantization,
                pq_nbits: setup.pq_nbits,
                opq: setup.opq,
                ..Default::default()
            },
        )?;
        let col = db.create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )?;

        let build = Instant::now();
        col.upsert_batch(
            data.iter()
                .map(|(id, v)| Record::new(*id, v.clone()))
                .collect(),
        )?;
        // フラッシュ時に索引と量子化コードブックが作られる
        col.flush()?;
        let build_ms = build.elapsed().as_millis();

        // 計測前にウォームアップ (mmap ページフォルトを除く)
        for q in queries.iter() {
            col.search(q).k(10).run()?;
        }
        let search = Instant::now();
        let mut hit_rate = 0.0;
        for (q, want) in queries.iter().zip(&truth) {
            let got: Vec<u64> = col.search(q).k(10).run()?.iter().map(|h| h.id).collect();
            hit_rate +=
                want.iter().filter(|id| got.contains(id)).count() as f64 / want.len() as f64;
        }
        let per_query_us = search.elapsed().as_micros() / queries.len() as u128;
        let recall = hit_rate / queries.len() as f64;

        println!(
            "{:<20} {:>8.3} {:>10} {:>10} {:>8} MB  {}",
            setup.label,
            recall,
            per_query_us,
            build_ms,
            dir_size(&dir) / 1024 / 1024,
            setup.note
        );

        drop(col);
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    println!(
        "\n注意:
- ディスクは再ランク用の vectors.bin を常に含むので、量子化を足すと**増えます**。
  圧縮が効くのは「探索時に走査する作業集合」で、億件規模で意味を持ちます
- IVF 系は nprobe (既定 8) 次第で recall が大きく変わります。
  `search(q).nprobe(32)` のように検索ごとに上書きできます
- recall は再ランク後の値です。score は常に正確な f32 距離が返ります
- OPQ の構築時間は件数が少ないほど割高です (回転学習のコストが固定で乗る)。
  SIFT 20 万件では PQ の 1.5 倍程度でした"
    );
    Ok(())
}

/// クラスタ構造のある決定的なデータ (実際の埋め込みに近い分布)。
fn clustered_data(n: u64, dim: usize) -> Vec<(u64, Vec<f32>)> {
    let clusters = ((n as f64).sqrt() as u64).clamp(8, 256);
    (0..n)
        .map(|i| {
            let c = i % clusters;
            let v = (0..dim)
                .map(|d| {
                    let center = pseudo_random(c * 1000 + d as u64) * 5.0;
                    center + pseudo_random(i * 31 + d as u64) * 0.8
                })
                .collect();
            (i, v)
        })
        .collect()
}

fn flat_topk(data: &[(u64, Vec<f32>)], query: &[f32], k: usize) -> Vec<u64> {
    let mut all: Vec<(f32, u64)> = data
        .iter()
        .map(|(id, v)| (Metric::L2.distance_key(query, v), *id))
        .collect();
    all.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    all.into_iter().take(k).map(|(_, id)| id).collect()
}

fn dir_size(path: &std::path::Path) -> u64 {
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

fn pseudo_random(seed: u64) -> f32 {
    let x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((x >> 33) as f32) / (u32::MAX as f32 / 2.0) - 1.0
}
