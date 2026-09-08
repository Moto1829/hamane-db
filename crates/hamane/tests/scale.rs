//! 大量データの読み書き E2E (todo 1301 B)。
//!
//! 全て `#[ignore]` なので通常の `cargo test` では走らない。
//!
//! ```text
//! cargo test --release -p hamane --test scale -- --ignored --nocapture
//! HAMANE_SCALE_N=1000000 cargo test --release -p hamane --test scale -- --ignored
//! ```
//!
//! 既定は 10 万件 (`common::scale_n`)。nightly ワークフローが 100 万件を指定する。
//! **必ず `--release` で実行すること** (debug の HNSW 構築は桁違いに遅い)。

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use common::{
    clustered_dataset, dataset, dir_size, flat_topk, random_vec, recall, records, scale_n, Phase,
};
use hamane::{CollectionConfig, Database, Metric, Quantization, Record, StoreOptions, SyncPolicy};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

const DIM: usize = 128;

fn config() -> CollectionConfig {
    CollectionConfig {
        dim: DIM,
        metric: Metric::L2,
    }
}

/// 大量投入向けの既定オプション。フラッシュ閾値を下げて**複数セグメントを強制**し、
/// 自動コンパクションも通るようにする。
fn bulk_options() -> StoreOptions {
    StoreOptions {
        // fsync はここでの計測対象ではない (耐久性は crash.rs で見ている)
        sync: SyncPolicy::EveryN(u32::MAX),
        flush_threshold_bytes: 8 * 1024 * 1024,
        hnsw_min_rows: 1024,
        ..Default::default()
    }
}

/// 100 万件規模を投入し、複数セグメントに分かれた状態で
/// 件数・点参照・検索結果が正しいこと。
#[test]
#[ignore = "大量データ (nightly / --ignored でのみ実行)"]
fn large_dataset_search_stays_consistent() {
    let n = scale_n();
    let dir = tempfile::tempdir().unwrap();
    // 再現率を見るので実データに近いクラスタ構造のデータを使う
    // (一様乱数の高次元は HNSW の最悪ケース。common::clustered_dataset 参照)
    let data = {
        let _p = Phase::start("generate dataset");
        clustered_dataset(n, DIM, 1301)
    };

    let db = Database::open_with_options(dir.path(), bulk_options()).unwrap();
    let col = db.create_collection("docs", config()).unwrap();
    {
        let _p = Phase::start("upsert");
        for chunk in data.chunks(10_000) {
            col.upsert_batch(records(chunk)).unwrap();
        }
    }
    {
        let _p = Phase::start("flush");
        col.flush().unwrap();
    }

    let stats = col.segment_stats().unwrap();
    eprintln!("[scale] segments = {}", stats.len());
    assert!(
        stats.len() >= 2,
        "フラッシュ閾値を下げたので複数セグメントになるはず (got {})",
        stats.len()
    );
    assert_eq!(col.len(), n as usize, "件数が一致しない");

    // 点参照: 決定的にサンプリングした ID
    {
        let _p = Phase::start("point lookups");
        for i in (0..n).step_by((n / 500).max(1) as usize) {
            let got = col.get(i).unwrap_or_else(|| panic!("id {i} is missing"));
            assert_eq!(got.vector, data[i as usize].1, "id {i} の内容が壊れている");
        }
    }

    // 検索: 総当たり正解との recall@10
    let _p = Phase::start("search + recall");
    let queries = 20;
    let recall_at = |ef: Option<usize>| -> f64 {
        let mut total = 0.0;
        for qi in 0..queries {
            let q = &data[qi * 977 % data.len()].1;
            let mut builder = col.search(q).k(10);
            if let Some(ef) = ef {
                builder = builder.ef(ef);
            }
            let ids: Vec<u64> = builder.run().unwrap().iter().map(|h| h.id).collect();
            let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), q, 10, Metric::L2, |_| {
                true
            });
            total += recall(&ids, &truth);
        }
        total / queries as f64
    };

    let avg = recall_at(None);
    eprintln!("[scale] recall@10 = {avg:.4} over {n} rows (default ef)");
    assert!(avg >= 0.95, "recall@10 = {avg:.4}");

    // ef を上げれば必ず改善する (探索が壊れていないことの独立した確認)。
    // 一様乱数の高次元でも成り立つ性質なので、データ依存で不安定にならない
    let wide = recall_at(Some(256));
    eprintln!("[scale] recall@10 = {wide:.4} (ef=256)");
    assert!(
        wide >= avg - 0.01,
        "ef を上げて recall が落ちた: {avg} → {wide}"
    );
}

/// 書き込み (バックグラウンドフラッシュ込み) と検索を並行させても、
/// 検索が壊れず・スコアが常に正確な f32 距離であること。
#[test]
#[ignore = "大量データ (nightly / --ignored でのみ実行)"]
fn concurrent_writes_and_searches() {
    let n = scale_n() / 2;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open_with_options(dir.path(), bulk_options()).unwrap());
    let col = db.create_collection("docs", config()).unwrap();

    // 検索側が空振りしないよう先に少し入れておく
    let seed_data = dataset(2_000, DIM, 5);
    col.upsert_batch(records(&seed_data)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let searches = Arc::new(AtomicU64::new(0));
    let _p = Phase::start("concurrent write + search");

    std::thread::scope(|s| {
        // 検索スレッド
        for t in 0..3 {
            let col = Arc::clone(&col);
            let stop = Arc::clone(&stop);
            let searches = Arc::clone(&searches);
            s.spawn(move || {
                let mut rng = StdRng::seed_from_u64(100 + t);
                while !stop.load(Ordering::Relaxed) {
                    let q = random_vec(&mut rng, DIM);
                    let hits = col.search(&q).k(10).run().expect("search must not fail");
                    for hit in &hits {
                        // score は常に正確な f32 距離 (量子化なしなのでそのまま)
                        let stored = col.get(hit.id).expect("hit must be readable");
                        let exact =
                            Metric::L2.score_from_key(Metric::L2.distance_key(&q, &stored.vector));
                        assert!(
                            (hit.score - exact).abs() < 1e-3,
                            "score {} != exact {exact}",
                            hit.score
                        );
                    }
                    searches.fetch_add(1, Ordering::Relaxed);
                }
            });
        }

        // 書き込みスレッド (単一ライタ)
        let mut rng = StdRng::seed_from_u64(9);
        for i in 2_000..n {
            col.upsert(Record::new(i, random_vec(&mut rng, DIM)))
                .unwrap();
        }
        stop.store(true, Ordering::Relaxed);
    });

    eprintln!(
        "[scale] {} searches during {} writes",
        searches.load(Ordering::Relaxed),
        n - 2_000
    );
    assert!(
        searches.load(Ordering::Relaxed) > 0,
        "検索が 1 回も走っていない"
    );
    col.flush().unwrap();
    assert_eq!(col.len(), n as usize);
}

/// upsert / delete / search が混ざる長時間ワークロードの結果が
/// 参照モデル (HashMap) と一致すること。
#[test]
#[ignore = "大量データ (nightly / --ignored でのみ実行)"]
fn mixed_workload_matches_reference_model() {
    let n = scale_n() / 2;
    let ops = n * 2;
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(dir.path(), bulk_options()).unwrap();
    let col = db.create_collection("docs", config()).unwrap();

    // 値はクラスタ構造から引く (再現率の検証に耐えるデータにする)
    let pool = clustered_dataset(n, DIM, 555);
    let mut model: HashMap<u64, Vec<f32>> = HashMap::new();
    let mut rng = StdRng::seed_from_u64(1234);
    {
        let _p = Phase::start("mixed workload");
        for _ in 0..ops {
            let id = rng.random_range(0..n);
            match rng.random_range(0..10) {
                // 70% upsert (新規と上書きが混ざる)
                0..=6 => {
                    let v = pool[rng.random_range(0..pool.len())].1.clone();
                    col.upsert(Record::new(id, v.clone())).unwrap();
                    model.insert(id, v);
                }
                // 20% delete
                7..=8 => {
                    col.delete(id).unwrap();
                    model.remove(&id);
                }
                // 10% search (結果は使わないが経路を通す)
                _ => {
                    let q = random_vec(&mut rng, DIM);
                    col.search(&q).k(10).run().unwrap();
                }
            }
        }
    }
    col.flush().unwrap();

    assert_eq!(col.len(), model.len(), "件数がモデルと一致しない");
    let _p = Phase::start("verify against model");
    for (id, vector) in model.iter().take(2_000) {
        let got = col
            .get(*id)
            .unwrap_or_else(|| panic!("id {id} should exist"));
        assert_eq!(&got.vector, vector, "id {id} の内容がモデルと違う");
    }
    // 削除済みが見えないこと
    for id in 0..n.min(2_000) {
        if !model.contains_key(&id) {
            assert!(col.get(id).is_none(), "deleted id {id} is still visible");
        }
    }

    // 検索結果もモデルの総当たりと一致する (recall)
    let model_vec: Vec<(u64, Vec<f32>)> = model.into_iter().collect();
    let mut total = 0.0;
    let queries = 10;
    for i in 0..queries {
        // クエリもクラスタ上から取る (実利用に近い分布)
        let q = pool[i * 7 % pool.len()].1.clone();
        let ids: Vec<u64> = col
            .search(&q)
            .k(10)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        let truth = flat_topk(
            model_vec.iter().map(|(i, v)| (*i, v)),
            &q,
            10,
            Metric::L2,
            |_| true,
        );
        total += recall(&ids, &truth);
    }
    let avg = total / queries as f64;
    eprintln!("[scale] recall@10 after mixed workload = {avg:.4}");
    assert!(avg >= 0.90, "recall@10 = {avg:.4}");
}

/// 上書き中心のワークロードでディスク使用量が発散しないこと (todo 401 の大規模版)。
#[test]
#[ignore = "大量データ (nightly / --ignored でのみ実行)"]
fn compaction_keeps_disk_bounded() {
    let n = (scale_n() / 10).max(5_000);
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            compaction_threshold: 3,
            ..bulk_options()
        },
    )
    .unwrap();
    let col = db.create_collection("docs", config()).unwrap();

    let mut rng = StdRng::seed_from_u64(77);
    let mut sizes = Vec::new();
    let _p = Phase::start("overwrite rounds");
    for round in 0..6 {
        // 同じ ID 空間を毎回上書きする = 論理サイズは一定
        for i in 0..n {
            col.upsert(Record::new(i, random_vec(&mut rng, DIM)))
                .unwrap();
        }
        col.flush().unwrap();
        db.compact().unwrap();
        let size = dir_size(dir.path());
        eprintln!("[scale] round {round}: {} MB", size / 1024 / 1024);
        sizes.push(size);
        assert_eq!(col.len(), n as usize);
    }

    // 論理サイズが一定なので、最終サイズは初回の 2 倍を超えないはず
    let first = sizes[1]; // 1 回目はセグメント構成が安定しないので 2 回目を基準に
    let last = *sizes.last().unwrap();
    assert!(
        last <= first * 2,
        "ディスクが収束していない: {} MB → {} MB",
        first / 1024 / 1024,
        last / 1024 / 1024
    );
}

/// 高次元 (dim=768) を量子化ありで扱えること。
#[test]
#[ignore = "大量データ (nightly / --ignored でのみ実行)"]
fn high_dimension_with_quantization() {
    const HIGH_DIM: usize = 768;
    let n = (scale_n() / 10).max(5_000);
    let data = clustered_dataset(n, HIGH_DIM, 768);

    // PQ は 1 段目が粗いぶん閾値を下げる (再ランク後でも取りこぼしはある)。
    // HNSW の並列構築は非決定なので、実測値からマージンを取った下限にする
    for (label, quantization, pq_nbits, min_recall) in [
        ("f32", None, 8u8, 0.95),
        ("sq8", Some(Quantization::Sq8), 8, 0.95),
        ("pq8", Some(Quantization::Pq { m: None }), 8, 0.85),
        ("pq4", Some(Quantization::Pq { m: Some(192) }), 4, 0.85),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_with_options(
            dir.path(),
            StoreOptions {
                quantization,
                pq_nbits,
                ..bulk_options()
            },
        )
        .unwrap();
        let col = db
            .create_collection(
                "docs",
                CollectionConfig {
                    dim: HIGH_DIM,
                    metric: Metric::L2,
                },
            )
            .unwrap();
        {
            let _p = Phase::start("high-dim upsert + flush");
            for chunk in data.chunks(5_000) {
                col.upsert_batch(records(chunk)).unwrap();
            }
            col.flush().unwrap();
        }

        let mut total = 0.0;
        let queries = 10;
        for qi in 0..queries {
            let q = data[qi * 13 % data.len()].1.clone();
            let ids: Vec<u64> = col
                .search(&q)
                .k(10)
                .run()
                .unwrap()
                .iter()
                .map(|h| h.id)
                .collect();
            let truth = flat_topk(
                data.iter().map(|(i, v)| (*i, v)),
                &q,
                10,
                Metric::L2,
                |_| true,
            );
            total += recall(&ids, &truth);
        }
        let avg = total / queries as f64;
        eprintln!(
            "[scale] dim=768 {label}: recall@10 = {avg:.4}, disk = {} MB",
            dir_size(dir.path()) / 1024 / 1024
        );
        assert!(avg >= min_recall, "dim=768 {label}: recall@10 = {avg:.4}");
    }
}

/// 大きな DB を閉じて開き直しても、件数と検索結果が変わらないこと。
#[test]
#[ignore = "大量データ (nightly / --ignored でのみ実行)"]
fn large_database_reopens_identically() {
    let n = scale_n() / 2;
    let dir = tempfile::tempdir().unwrap();
    let data = dataset(n, DIM, 20);
    let queries: Vec<Vec<f32>> = {
        let mut rng = StdRng::seed_from_u64(3);
        (0..10).map(|_| random_vec(&mut rng, DIM)).collect()
    };

    let before: Vec<Vec<u64>> = {
        let db = Database::open_with_options(dir.path(), bulk_options()).unwrap();
        let col = db.create_collection("docs", config()).unwrap();
        {
            let _p = Phase::start("upsert + flush");
            for chunk in data.chunks(10_000) {
                col.upsert_batch(records(chunk)).unwrap();
            }
            col.flush().unwrap();
        }
        queries
            .iter()
            .map(|q| {
                col.search(q)
                    .k(10)
                    .run()
                    .unwrap()
                    .iter()
                    .map(|h| h.id)
                    .collect()
            })
            .collect()
    };

    let _p = Phase::start("reopen");
    let db = Database::open_with_options(dir.path(), bulk_options()).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), n as usize, "再 open で件数が変わった");
    for (q, want) in queries.iter().zip(&before) {
        let got: Vec<u64> = col
            .search(q)
            .k(10)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(&got, want, "再 open で検索結果が変わった");
    }
}
