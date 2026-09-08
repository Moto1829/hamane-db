//! 大量データを投入するときの定石 (todo 1302)。
//!
//! 実行: `cargo run --release --example bulk_load [件数]`  (既定 20 万件)
//!
//! ポイント:
//! 1. `upsert_batch` でまとめて入れる (WAL の書き込みと fsync がまとまる)
//! 2. `SyncPolicy` を用途に合わせる。初期ロードは `Batch` か `EveryN`
//! 3. `flush_threshold_bytes` でセグメントの粒度を決める
//! 4. 投入後に `flush` + `compact` で読み取りに最適な形にする
//!
//! 途中でプロセスが落ちても WAL から復旧できるので、
//! 「全部入れ終わるまでデータが無い」ということはありません。

use std::time::Instant;

use hamane::{CollectionConfig, Database, Metric, Record, StoreOptions, SyncPolicy};

const DIM: usize = 128;
const BATCH: usize = 5_000;

fn main() -> hamane::Result<()> {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200_000);
    let dir = std::env::temp_dir().join(format!("hamane-example-bulk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let db = Database::open_with_options(
        &dir,
        StoreOptions {
            // 初期ロードでは 1 件ごとの fsync は割に合わない。
            // Batch は「並行する書き込みの fsync を 1 回に相乗り」させる (耐久性は同じ)。
            // ここは単一スレッドなので EveryN で間引く (クラッシュで直近を失い得る)
            sync: SyncPolicy::EveryN(1000),
            // 64 MiB ごとにセグメント化。大きくすると 1 セグメントが大きくなり
            // 検索は速いが、フラッシュ 1 回の停止時間とメモリが増える
            flush_threshold_bytes: 64 * 1024 * 1024,
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

    println!("{n} 件 × dim={DIM} を投入します (バッチ {BATCH} 件)");
    let started = Instant::now();
    let mut batch = Vec::with_capacity(BATCH);
    for i in 0..n {
        batch.push(Record::new(i, vector(i)));
        if batch.len() == BATCH {
            col.upsert_batch(std::mem::take(&mut batch))?;
            batch.reserve(BATCH);
            if (i + 1) % 50_000 == 0 {
                println!(
                    "  {:>7} 件 ({:.0} rec/s)",
                    i + 1,
                    (i + 1) as f64 / started.elapsed().as_secs_f64()
                );
            }
        }
    }
    if !batch.is_empty() {
        col.upsert_batch(batch)?;
    }
    let insert = started.elapsed();

    // 残りの memtable をセグメントにする (HNSW 構築もここで走る)
    let t = Instant::now();
    col.flush()?;
    let flush = t.elapsed();

    // セグメントを統合すると検索時のマージが減る (任意)
    let t = Instant::now();
    db.compact()?;
    let compact = t.elapsed();

    println!(
        "\n投入 {:.1?} ({:.0} rec/s) / フラッシュ {:.1?} / コンパクション {:.1?}",
        insert,
        n as f64 / insert.as_secs_f64(),
        flush,
        compact
    );
    let stats = col.segment_stats()?;
    println!("セグメント数: {} / 件数: {}", stats.len(), col.len());
    for s in stats.iter().take(5) {
        println!(
            "  seg {} rows={} hnsw={}",
            s.seg_id, s.record_count, s.has_hnsw
        );
    }

    let t = Instant::now();
    let hits = col.search(&vector(n / 2)).k(10).run()?;
    println!(
        "\n検索 {:.1?}: 先頭 id={} score={:.4}",
        t.elapsed(),
        hits[0].id,
        hits[0].score
    );

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

fn vector(i: u64) -> Vec<f32> {
    (0..DIM)
        .map(|d| {
            let x = (i * 31 + d as u64)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((x >> 33) as f32) / (u32::MAX as f32 / 2.0) - 1.0
        })
        .collect()
}
