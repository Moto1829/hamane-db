//! バックグラウンドフラッシュ中の書き込みレイテンシ計測 (todo 504 の検証)。
//!
//! flush_threshold を低くして自動フラッシュ (HNSW 構築込み) を誘発しながら
//! upsert し続け、レイテンシ分布を出す。
//!
//! 実行: `cargo run --release -p hamane --example write_latency`

use std::time::Instant;

use hamane::{CollectionConfig, Database, HnswParams, Metric, Record, StoreOptions, SyncPolicy};

fn main() -> hamane::Result<()> {
    let dir = std::env::temp_dir().join(format!("hamane-latency-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Database::open_with_options(
        &dir,
        StoreOptions {
            sync: SyncPolicy::EveryN(u32::MAX), // fsync を除外して純粋なブロッキングを測る
            flush_threshold_bytes: 16 * 1024 * 1024, // 16MiB ごとに自動フラッシュ
            hnsw: HnswParams::default(),
            ..Default::default()
        },
    )?;
    let col = db.create_collection(
        "bench",
        CollectionConfig {
            dim: 128,
            metric: Metric::L2,
        },
    )?;

    const N: u64 = 200_000; // 16MiB 閾値で約 6 回の自動フラッシュが走る
    let mut latencies = Vec::with_capacity(N as usize);
    let vector: Vec<f32> = (0..128).map(|i| i as f32 * 0.01).collect();
    let started = Instant::now();
    for i in 0..N {
        let t = Instant::now();
        col.upsert(Record::new(i, vector.clone()))?;
        latencies.push(t.elapsed());
    }
    let total = started.elapsed();

    latencies.sort_unstable();
    let pct = |p: f64| latencies[((latencies.len() as f64 * p) as usize).min(latencies.len() - 1)];
    let flushes = col.segment_stats()?.len();
    println!(
        "upserts: {N} in {:.1}s ({:.0}/s)",
        total.as_secs_f64(),
        N as f64 / total.as_secs_f64()
    );
    println!("auto flushes (segments): {flushes}");
    println!(
        "latency p50={:?} p99={:?} p99.9={:?} max={:?}",
        pct(0.50),
        pct(0.99),
        pct(0.999),
        latencies[latencies.len() - 1]
    );
    db.flush()?;
    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
