//! 複数スレッドからの読み書き (todo 1302)。
//!
//! 実行: `cargo run --release --example concurrent`
//!
//! hamane-db は**単一ライタ・複数リーダ**です。
//! `Database` / `Collection` は `Send + Sync` なので `Arc` で共有し、
//! 書き込みは 1 スレッドに集約、読み取りは何スレッドからでも投げられます
//! (書き込み自体も内部でロックされるので複数スレッドから呼んでも安全ですが、
//!  スループットは上がりません)。
//!
//! 検索中にバックグラウンドのフラッシュ・コンパクションが走っても、
//! 検索は開始時点のスナップショット (LiveView) を見るので壊れません。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use hamane::{CollectionConfig, Database, Metric, Record, StoreOptions, SyncPolicy};

const DIM: usize = 32;

fn main() -> hamane::Result<()> {
    let dir =
        std::env::temp_dir().join(format!("hamane-example-concurrent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let db = Arc::new(Database::open_with_options(
        &dir,
        StoreOptions {
            sync: SyncPolicy::Batch,                // 並行書き込みの fsync を相乗りさせる
            flush_threshold_bytes: 4 * 1024 * 1024, // 小さくしてフラッシュを誘発する
            ..Default::default()
        },
    )?);
    // create_collection / collection は Arc<Collection> を返すのでそのまま共有できる
    let col = db.create_collection(
        "docs",
        CollectionConfig {
            dim: DIM,
            metric: Metric::L2,
        },
    )?;

    for i in 0..1_000u64 {
        col.upsert(Record::new(i, vector(i)))?;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let searches = Arc::new(AtomicU64::new(0));

    std::thread::scope(|scope| {
        // 読み取りスレッド 4 本
        for t in 0..4 {
            let col = Arc::clone(&col);
            let stop = Arc::clone(&stop);
            let searches = Arc::clone(&searches);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let hits = col.search(&vector(t * 977)).k(10).run().expect("search");
                    assert!(!hits.is_empty());
                    searches.fetch_add(1, Ordering::Relaxed);
                }
            });
        }

        // 書き込みは 1 スレッドに集約する
        for i in 1_000..30_000u64 {
            col.upsert(Record::new(i, vector(i))).expect("upsert");
        }
        stop.store(true, Ordering::Relaxed);
    });

    col.flush()?;
    println!(
        "書き込み 29,000 件の裏で {} 回検索しました (件数 {})",
        searches.load(Ordering::Relaxed),
        col.len()
    );
    println!("セグメント数: {}", col.segment_stats()?.len());

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
