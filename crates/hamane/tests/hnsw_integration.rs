//! HNSW 統合テスト (todos/305–306): フラッシュで hnsw.bin が作られ、
//! memtable (Flat) + セグメント (HNSW) のマージ検索が十分な再現率を持つこと。

use hamane::{CollectionConfig, Database, Filter, Metric, Quantization, Record, StoreOptions};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

const DIM: usize = 16;

fn random_vec(rng: &mut StdRng) -> Vec<f32> {
    (0..DIM).map(|_| rng.random::<f32>() * 2.0 - 1.0).collect()
}

/// 正解: 全データの L2 top-k (id タイブレーク付き)。
fn flat_topk<'a>(
    data: impl Iterator<Item = (u64, &'a Vec<f32>)>,
    query: &[f32],
    k: usize,
    filter: impl Fn(u64) -> bool,
) -> Vec<u64> {
    let mut all: Vec<(f32, u64)> = data
        .filter(|(id, _)| filter(*id))
        .map(|(id, v)| (Metric::L2.distance_key(query, v), id))
        .collect();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());
    all.into_iter().take(k).map(|(_, id)| id).collect()
}

struct Fixture {
    db: Database,
    data: Vec<(u64, Vec<f32>)>,
    _dir: tempfile::TempDir,
}

/// セグメント 2 個 (HNSW あり) + memtable に分散した collection を作る。
fn fixture(n: u64) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            hnsw_min_rows: 64,
            ..Default::default()
        },
    )
    .unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();

    let mut rng = StdRng::seed_from_u64(305);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..n)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v).with_meta("mod10", (i % 10) as i64)
        })
        .collect();

    // 前半 40% → セグメント 1、次の 40% → セグメント 2、残り 20% → memtable
    let a = (n as usize) * 4 / 10;
    let b = (n as usize) * 8 / 10;
    let mut records = records.into_iter();
    col.upsert_batch(records.by_ref().take(a).collect())
        .unwrap();
    col.flush().unwrap();
    col.upsert_batch(records.by_ref().take(b - a).collect())
        .unwrap();
    col.flush().unwrap();
    col.upsert_batch(records.collect()).unwrap();

    Fixture {
        db,
        data,
        _dir: dir,
    }
}

fn recall(actual: &[u64], truth: &[u64]) -> f64 {
    actual.iter().filter(|id| truth.contains(id)).count() as f64 / truth.len() as f64
}

#[test]
fn segments_have_hnsw_and_recall_holds() {
    let fx = fixture(2000);
    let col = fx.db.collection("docs").unwrap();

    let mut rng = StdRng::seed_from_u64(1305);
    let mut total = 0.0;
    let queries = 50;
    for _ in 0..queries {
        let q = random_vec(&mut rng);
        let hits: Vec<u64> = col
            .search(&q)
            .k(10)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        let truth = flat_topk(fx.data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
        total += recall(&hits, &truth);
    }
    let avg = total / queries as f64;
    assert!(avg >= 0.95, "merged recall@10 = {avg:.3}");
}

#[test]
fn filtered_search_both_plans() {
    let fx = fixture(2000);
    let col = fx.db.collection("docs").unwrap();
    let mut rng = StdRng::seed_from_u64(2305);

    // 選択率 10% (mod10 == 3) → post-filter 経路
    // 選択率 ~0.1% (id == 42) 相当は eq フィルタでは書けないので
    // 複合 (mod10==3 AND mod10>=3) で post、レアケースは and で作る
    let common = Filter::eq("mod10", 3);
    // 選択率 0 に近い: mod10==3 かつ mod10==4 は空集合 → pre-filter 経路 (s=0)
    let empty = Filter::and([Filter::eq("mod10", 3), Filter::eq("mod10", 4)]);

    let mut total = 0.0;
    let queries = 30;
    for _ in 0..queries {
        let q = random_vec(&mut rng);
        let hits: Vec<u64> = col
            .search(&q)
            .k(10)
            .filter(common.clone())
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        assert!(hits.iter().all(|id| id % 10 == 3), "filter must hold");
        let truth = flat_topk(fx.data.iter().map(|(i, v)| (*i, v)), &q, 10, |id| {
            id % 10 == 3
        });
        total += recall(&hits, &truth);
    }
    let avg = total / queries as f64;
    assert!(avg >= 0.9, "filtered recall@10 = {avg:.3}");

    // 空集合フィルタは 0 件 (panic しない)
    let q = random_vec(&mut rng);
    assert!(col.search(&q).k(10).filter(empty).run().unwrap().is_empty());
}

#[test]
fn ef_improves_recall_via_api() {
    let fx = fixture(3000);
    let col = fx.db.collection("docs").unwrap();
    let mut rng = StdRng::seed_from_u64(3305);

    let recall_with_ef = |ef: usize, rng: &mut StdRng| -> f64 {
        let mut total = 0.0;
        for _ in 0..30 {
            let q = random_vec(rng);
            let hits: Vec<u64> = col
                .search(&q)
                .k(10)
                .ef(ef)
                .run()
                .unwrap()
                .iter()
                .map(|h| h.id)
                .collect();
            let truth = flat_topk(fx.data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
            total += recall(&hits, &truth);
        }
        total / 30.0
    };

    let low = recall_with_ef(8, &mut rng);
    let mut rng = StdRng::seed_from_u64(3305); // 同じクエリ列
    let high = recall_with_ef(512, &mut rng);
    assert!(
        high >= low - 0.01,
        "ef=512 ({high:.3}) must not be worse than ef=8 ({low:.3})"
    );
    assert!(high >= 0.99, "ef=512 should be near exact: {high:.3}");
}

#[test]
fn small_segments_fall_back_to_flat() {
    // hnsw_min_rows 未満のセグメントは hnsw.bin なしでも正しく動く
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            hnsw_min_rows: 1_000_000, // 実質無効化
            ..Default::default()
        },
    )
    .unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: 2,
                metric: Metric::L2,
            },
        )
        .unwrap();
    for i in 0..100u64 {
        col.upsert(Record::new(i, vec![i as f32, 0.0])).unwrap();
    }
    col.flush().unwrap();
    let hits = col.search(&[0.0, 0.0]).k(3).run().unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![0, 1, 2]);
}

/// SQ8 量子化 (todo 602): 2 段階検索 (SQ8 距離 → f32 再ランク) で
/// recall を保ちつつ、量子化ファイルが再 open 後も使われること。
#[test]
fn sq8_two_stage_search_preserves_recall() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            hnsw_min_rows: 64,
            quantization: Some(Quantization::Sq8),
            ..Default::default()
        },
    )
    .unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();

    let mut rng = StdRng::seed_from_u64(602);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..3000u64)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v)
        })
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();

    // sq8 ファイルが存在する (segment_stats では見えないので検索品質で検証)
    let mut total = 0.0;
    let queries = 50;
    for _ in 0..queries {
        let q = random_vec(&mut rng);
        let hits: Vec<u64> = col
            .search(&q)
            .k(10)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
        total += recall(&hits, &truth);
    }
    let avg = total / queries as f64;
    assert!(avg >= 0.95, "sq8 two-stage recall@10 = {avg:.3}");

    // 再 open しても SQ8 経路が生きている
    drop(col);
    drop(db);
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            hnsw_min_rows: 64,
            quantization: Some(Quantization::Sq8),
            ..Default::default()
        },
    )
    .unwrap();
    let col = db.collection("docs").unwrap();
    let q = random_vec(&mut rng);
    let hits = col.search(&q).k(5).run().unwrap();
    assert_eq!(hits.len(), 5);
    // 検索結果が f32 再ランク済み = score が正確な距離であること
    let exact = Metric::L2
        .distance_key(&q, &data[hits[0].id as usize].1)
        .sqrt();
    assert!(
        (hits[0].score - exact).abs() < 1e-4,
        "score must be exact f32 distance"
    );
}

/// PQ 量子化 (todo 1003): 2 段階検索 (ADC 距離 → f32 再ランク) で recall を
/// 保ちつつ、vectors_pq.bin が再 open 後も使われること。
#[test]
fn pq_two_stage_search_preserves_recall() {
    let dir = tempfile::tempdir().unwrap();
    let opts = || StoreOptions {
        hnsw_min_rows: 64,
        // m は None = dim から自動決定 (DIM=16 → m=4, dsub=4)
        quantization: Some(Quantization::Pq { m: None }),
        ..Default::default()
    };
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();

    let mut rng = StdRng::seed_from_u64(1003);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..3000u64)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v)
        })
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();

    let mut total = 0.0;
    let queries = 50;
    for _ in 0..queries {
        let q = random_vec(&mut rng);
        let hits: Vec<u64> = col
            .search(&q)
            .k(10)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
        total += recall(&hits, &truth);
    }
    let avg = total / queries as f64;
    assert!(avg >= 0.95, "pq two-stage recall@10 = {avg:.3}");

    // 再 open しても PQ 経路が生きている
    drop(col);
    drop(db);
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db.collection("docs").unwrap();
    let q = random_vec(&mut rng);
    let hits = col.search(&q).k(5).run().unwrap();
    assert_eq!(hits.len(), 5);
    // f32 再ランク済み = score が正確な距離
    let exact = Metric::L2
        .distance_key(&q, &data[hits[0].id as usize].1)
        .sqrt();
    assert!(
        (hits[0].score - exact).abs() < 1e-4,
        "score must be exact f32 distance"
    );
}

/// IVF (todo 1004): nprobe クラスタだけを走査する枝刈り検索。nprobe を上げると
/// recall が改善し、十分大きな nprobe で全走査 (Flat) の正解に一致すること。
#[test]
fn ivf_nprobe_improves_recall() {
    let dir = tempfile::tempdir().unwrap();
    let opts = || StoreOptions {
        hnsw_min_rows: 64,
        index: hamane::IndexKind::Ivf,
        nprobe: 1,
        ..Default::default()
    };
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();

    let mut rng = StdRng::seed_from_u64(1004);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..3000u64)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v)
        })
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();

    // nprobe を変えて recall@10 を測る。単調に改善し、大きな nprobe で 1.0
    let recall_at = |nprobe: usize, rng: &mut StdRng| -> f64 {
        let queries = 40;
        let mut total = 0.0;
        for _ in 0..queries {
            let q = random_vec(rng);
            let hits: Vec<u64> = col
                .search(&q)
                .k(10)
                .nprobe(nprobe)
                .run()
                .unwrap()
                .iter()
                .map(|h| h.id)
                .collect();
            let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
            total += recall(&hits, &truth);
        }
        total / queries as f64
    };

    let r1 = recall_at(1, &mut rng);
    let r8 = recall_at(8, &mut rng);
    // nlist = sqrt(3000) ≈ 55。全クラスタ走査 = 全走査なので Flat 正解に一致
    let r_full = recall_at(1000, &mut rng);

    assert!(
        r8 >= r1,
        "nprobe=8 recall {r8} should be >= nprobe=1 recall {r1}"
    );
    assert!(
        r_full >= 0.999,
        "full-probe recall {r_full} should match exact Flat"
    );

    // 再 open しても IVF 経路が生きている (score は正確な f32 距離)
    drop(col);
    drop(db);
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db.collection("docs").unwrap();
    let q = random_vec(&mut rng);
    let hits = col.search(&q).k(5).nprobe(1000).run().unwrap();
    assert_eq!(hits.len(), 5);
    let exact = Metric::L2
        .distance_key(&q, &data[hits[0].id as usize].1)
        .sqrt();
    assert!(
        (hits[0].score - exact).abs() < 1e-4,
        "score must be exact f32 distance"
    );
}

/// IVF-PQ (todo 1005): IVF の枝刈り + 残差 PQ 圧縮。nprobe を上げると recall が
/// 改善し、再ランクありで高い recall を保つ。再 open 後も経路が生きること。
#[test]
fn ivfpq_two_stage_search_preserves_recall() {
    let dir = tempfile::tempdir().unwrap();
    let opts = || StoreOptions {
        hnsw_min_rows: 64,
        index: hamane::IndexKind::IvfPq,
        quantization: Some(Quantization::Pq { m: None }),
        nprobe: 8,
        ..Default::default()
    };
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();

    let mut rng = StdRng::seed_from_u64(1005);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..3000u64)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v)
        })
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();

    let recall_at = |nprobe: usize, rng: &mut StdRng| -> f64 {
        let queries = 40;
        let mut total = 0.0;
        for _ in 0..queries {
            let q = random_vec(rng);
            let hits: Vec<u64> = col
                .search(&q)
                .k(10)
                .nprobe(nprobe)
                .run()
                .unwrap()
                .iter()
                .map(|h| h.id)
                .collect();
            let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
            total += recall(&hits, &truth);
        }
        total / queries as f64
    };

    let r1 = recall_at(1, &mut rng);
    let r_full = recall_at(1000, &mut rng);
    // full-probe + 再ランクで高 recall。nprobe を増やすと改善
    assert!(
        r_full >= r1,
        "full-probe recall {r_full} should be >= nprobe=1 {r1}"
    );
    assert!(r_full >= 0.90, "ivfpq full-probe recall@10 = {r_full}");

    // 再 open しても IVF-PQ 経路が生きている (score は正確な f32 距離)
    drop(col);
    drop(db);
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db.collection("docs").unwrap();
    let q = random_vec(&mut rng);
    let hits = col.search(&q).k(5).nprobe(1000).run().unwrap();
    assert_eq!(hits.len(), 5);
    let exact = Metric::L2
        .distance_key(&q, &data[hits[0].id as usize].1)
        .sqrt();
    assert!(
        (hits[0].score - exact).abs() < 1e-4,
        "score must be exact f32 distance"
    );
}

#[test]
fn hnsw_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open_with_options(
            dir.path(),
            StoreOptions {
                hnsw_min_rows: 64,
                ..Default::default()
            },
        )
        .unwrap();
        let col = db
            .create_collection(
                "docs",
                CollectionConfig {
                    dim: DIM,
                    metric: Metric::Cosine,
                },
            )
            .unwrap();
        let mut rng = StdRng::seed_from_u64(4305);
        let records: Vec<Record> = (0..500u64)
            .map(|i| Record::new(i, random_vec(&mut rng)))
            .collect();
        col.upsert_batch(records).unwrap();
        col.flush().unwrap();
    }
    let db = Database::open(dir.path()).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), 500);
    let mut rng = StdRng::seed_from_u64(5305);
    let q = random_vec(&mut rng);
    let hits = col.search(&q).k(5).run().unwrap();
    assert_eq!(hits.len(), 5);
    // Cosine: スコアは類似度 (降順)
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
}

/// todo 801: search_threads の設定 (逐次 / プール / 自動) で検索結果が
/// 変わらないこと。セグメントは open 時にディスクから読むだけなので、
/// 同じ DB を開き直せば結果は完全一致するはず。
#[test]
fn search_threads_settings_agree() {
    let Fixture { db, data: _, _dir } = fixture(2000);
    drop(db); // process lock を放してから別オプションで開き直す

    let mut rng = StdRng::seed_from_u64(1801);
    let queries: Vec<Vec<f32>> = (0..20).map(|_| random_vec(&mut rng)).collect();

    // (フィルタなし, フィルタあり) の hit id 列を全クエリ分collect する
    let run_all = |search_threads: usize| -> Vec<(Vec<u64>, Vec<u64>)> {
        let db = Database::open_with_options(
            _dir.path(),
            StoreOptions {
                hnsw_min_rows: 64,
                search_threads,
                ..Default::default()
            },
        )
        .unwrap();
        let col = db.collection("docs").unwrap();
        let ids =
            |hits: Vec<hamane::SearchHit>| -> Vec<u64> { hits.iter().map(|h| h.id).collect() };
        queries
            .iter()
            .map(|q| {
                let plain = ids(col.search(q).k(10).run().unwrap());
                let filtered = ids(col
                    .search(q)
                    .k(10)
                    .filter(Filter::eq("mod10", 3i64))
                    .run()
                    .unwrap());
                (plain, filtered)
            })
            .collect()
    };

    let sequential = run_all(1);
    assert!(sequential.iter().all(|(plain, _)| plain.len() == 10));
    assert_eq!(sequential, run_all(2), "pool (2) differs from sequential");
    assert_eq!(
        sequential,
        run_all(0),
        "pool (auto) differs from sequential"
    );
}

/// todo 801: 同一 Database への同時多発検索。共有プールでも各検索が
/// 単独実行と同じ結果を返すこと。
#[test]
fn concurrent_searches_are_consistent() {
    let fx = fixture(1000);
    let col = fx.db.collection("docs").unwrap();
    let ids = |q: &[f32]| -> Vec<u64> {
        col.search(q)
            .k(10)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect()
    };

    let mut rng = StdRng::seed_from_u64(2801);
    let queries: Vec<Vec<f32>> = (0..10).map(|_| random_vec(&mut rng)).collect();
    let expected: Vec<Vec<u64>> = queries.iter().map(|q| ids(q)).collect();

    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for (q, want) in queries.iter().zip(&expected) {
                    assert_eq!(&ids(q), want);
                }
            });
        }
    });
}

/// OPQ (todo 1103): 回転付き PQ でも 2 段階検索の recall が保たれ、opq.bin が
/// 再 open 後も使われること。回転は直交なのでスコアは生 f32 距離のまま。
#[test]
fn opq_pq_search_preserves_recall() {
    let dir = tempfile::tempdir().unwrap();
    let opts = || StoreOptions {
        hnsw_min_rows: 64,
        quantization: Some(Quantization::Pq { m: None }),
        opq: true,
        ..Default::default()
    };
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();

    let mut rng = StdRng::seed_from_u64(1103);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..3000u64)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v)
        })
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();

    // opq.bin が書かれている (セグメントディレクトリ直下)
    let opq_files: Vec<_> = walk_segment_files(dir.path(), "opq.bin");
    assert!(!opq_files.is_empty(), "opq.bin must be written");

    let mut total = 0.0;
    let queries = 50;
    for _ in 0..queries {
        let q = random_vec(&mut rng);
        let hits: Vec<u64> = col
            .search(&q)
            .k(10)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
        total += recall(&hits, &truth);
    }
    let avg = total / queries as f64;
    assert!(avg >= 0.95, "opq two-stage recall@10 = {avg:.3}");

    // 再 open しても OPQ 経路が生きている (score は正確な f32 距離)
    drop(col);
    drop(db);
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db.collection("docs").unwrap();
    let q = random_vec(&mut rng);
    let hits = col.search(&q).k(5).run().unwrap();
    assert_eq!(hits.len(), 5);
    let exact = Metric::L2
        .distance_key(&q, &data[hits[0].id as usize].1)
        .sqrt();
    assert!(
        (hits[0].score - exact).abs() < 1e-4,
        "score must be exact f32 distance"
    );
}

/// OPQ + IVF-PQ (todo 1103): 粗量子化も残差 PQ も回転後空間で行い、
/// full-probe で高い recall を保つこと。
#[test]
fn opq_ivfpq_search_preserves_recall() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            hnsw_min_rows: 64,
            index: hamane::IndexKind::IvfPq,
            quantization: Some(Quantization::Pq { m: None }),
            opq: true,
            nprobe: 8,
            ..Default::default()
        },
    )
    .unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();

    let mut rng = StdRng::seed_from_u64(11031);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..3000u64)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v)
        })
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();
    assert!(!walk_segment_files(dir.path(), "opq.bin").is_empty());

    let mut total = 0.0;
    let queries = 40;
    for _ in 0..queries {
        let q = random_vec(&mut rng);
        let hits: Vec<u64> = col
            .search(&q)
            .k(10)
            .nprobe(1000)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
        total += recall(&hits, &truth);
    }
    let avg = total / queries as f64;
    assert!(avg >= 0.90, "opq ivfpq full-probe recall@10 = {avg:.3}");
}

/// OPQ (todo 1103): opq.bin を消しても (= M10 で書いたセグメント) 読める。
/// ただしコードは回転後空間なので、回転を失うと精度は落ちる。ここでは
/// 「壊れず読めて検索が返る」ことだけを確認する (前方互換の保証)。
#[test]
fn segment_without_opq_file_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let opts = |opq: bool| StoreOptions {
        hnsw_min_rows: 64,
        quantization: Some(Quantization::Pq { m: None }),
        opq,
        ..Default::default()
    };
    let db = Database::open_with_options(dir.path(), opts(true)).unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();
    let mut rng = StdRng::seed_from_u64(9);
    let records: Vec<Record> = (0..2000u64)
        .map(|i| Record::new(i, random_vec(&mut rng)))
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();
    drop(col);
    drop(db);

    for path in walk_segment_files(dir.path(), "opq.bin") {
        std::fs::remove_file(path).unwrap();
    }

    let db = Database::open_with_options(dir.path(), opts(false)).unwrap();
    let col = db.collection("docs").unwrap();
    let hits = col.search(&random_vec(&mut rng)).k(5).run().unwrap();
    assert_eq!(hits.len(), 5);
}

/// DB ディレクトリ配下から指定名のセグメントファイルを集める (テスト用)。
fn walk_segment_files(root: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
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

/// OPQ (todo 1103): opq.bin の破損は open 時の直交性検証で弾かれる
/// (CRC は他のセグメントファイルと同じく `verify_checksums` の担当)。
#[test]
fn corrupted_opq_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let opts = || StoreOptions {
        hnsw_min_rows: 64,
        quantization: Some(Quantization::Pq { m: None }),
        opq: true,
        ..Default::default()
    };
    let db = Database::open_with_options(dir.path(), opts()).unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();
    let mut rng = StdRng::seed_from_u64(77);
    let records: Vec<Record> = (0..2000u64)
        .map(|i| Record::new(i, random_vec(&mut rng)))
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();
    drop(col);
    drop(db);

    let path = walk_segment_files(dir.path(), "opq.bin").pop().unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    // header 64B 直後の f32 (R[0][0]) の指数部を壊す = 直交性が明確に崩れる
    bytes[64 + 3] ^= 0x40;
    std::fs::write(&path, &bytes).unwrap();

    assert!(
        Database::open_with_options(dir.path(), opts()).is_err(),
        "corrupted opq.bin must be rejected"
    );
}

/// 4-bit PQ (todo 1202): コードが半分のバイト数になり、再ランクで recall を保つ。
/// ヘッダに nbits があるのでフォーマットは自己記述的 (8bit セグメントと共存可)。
#[test]
fn pq_4bit_halves_code_size_and_keeps_recall() {
    let build = |nbits: u8, m: usize, seed: u64| -> (std::path::PathBuf, tempfile::TempDir, f64) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_with_options(
            dir.path(),
            StoreOptions {
                hnsw_min_rows: 64,
                quantization: Some(Quantization::Pq { m: Some(m) }),
                pq_nbits: nbits,
                ..Default::default()
            },
        )
        .unwrap();
        let col = db
            .create_collection(
                "docs",
                CollectionConfig {
                    dim: DIM,
                    metric: Metric::L2,
                },
            )
            .unwrap();
        let mut rng = StdRng::seed_from_u64(seed);
        let mut data = Vec::new();
        let records: Vec<Record> = (0..3000u64)
            .map(|i| {
                let v = random_vec(&mut rng);
                data.push((i, v.clone()));
                Record::new(i, v)
            })
            .collect();
        col.upsert_batch(records).unwrap();
        col.flush().unwrap();

        let mut total = 0.0;
        let queries = 40;
        for _ in 0..queries {
            let q = random_vec(&mut rng);
            let hits: Vec<u64> = col
                .search(&q)
                .k(10)
                .run()
                .unwrap()
                .iter()
                .map(|h| h.id)
                .collect();
            let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
            total += recall(&hits, &truth);
        }
        let path = walk_segment_files(dir.path(), "vectors_pq.bin")
            .pop()
            .unwrap();
        let recall = total / queries as f64;
        (path, dir, recall)
    };

    // (a) 基準: 8bit, m=4 → 4 B/行
    let (p8, _d8, r8) = build(8, 4, 1202);
    // (b) 同じ m で 4bit → 2 B/行 (メモリ半分・粗い)
    let (p4, _d4, r4) = build(4, 4, 1202);
    // (c) m を 2 倍にした 4bit → 4 B/行 (同じコード長)
    let (p4b, _d4b, r4b) = build(4, 8, 1202);
    let size = |p: &std::path::Path| std::fs::metadata(p).unwrap().len();

    // コード領域は半分。コードブックも 256 → 16 セントロイドで小さい
    assert!(
        size(&p4) < size(&p8),
        "4bit file {} must be smaller than 8bit {}",
        size(&p4),
        size(&p8)
    );
    // (c) は 8bit と同じコード長で、サブベクトルが細かいぶん recall も出る
    assert!(
        r4b >= 0.95,
        "4bit m=8 recall@10 = {r4b:.3} (8bit m=4: {r8:.3})"
    );
    assert!(r8 >= 0.95, "8bit pq recall@10 = {r8:.3}");
    // (b) はコードが半分なので粗くなるが、再ランク (4bit は k×8) で実用域に残る
    assert!(r4 >= 0.75, "4bit m=4 recall@10 = {r4:.3}");
    assert!(
        size(&p4b) <= size(&p8),
        "同じコード長なら 8bit 以下のサイズ"
    );
}

/// 4-bit PQ + IVF-PQ + OPQ の組み合わせでも検索が通ること (todo 1202)。
#[test]
fn pq_4bit_works_with_ivfpq_and_opq() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            hnsw_min_rows: 64,
            index: hamane::IndexKind::IvfPq,
            quantization: Some(Quantization::Pq { m: Some(8) }),
            pq_nbits: 4,
            opq: true,
            nprobe: 8,
            ..Default::default()
        },
    )
    .unwrap();
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: DIM,
                metric: Metric::L2,
            },
        )
        .unwrap();
    let mut rng = StdRng::seed_from_u64(12021);
    let mut data = Vec::new();
    let records: Vec<Record> = (0..2000u64)
        .map(|i| {
            let v = random_vec(&mut rng);
            data.push((i, v.clone()));
            Record::new(i, v)
        })
        .collect();
    col.upsert_batch(records).unwrap();
    col.flush().unwrap();

    let mut total = 0.0;
    let queries = 30;
    for _ in 0..queries {
        let q = random_vec(&mut rng);
        let hits: Vec<u64> = col
            .search(&q)
            .k(10)
            .nprobe(1000)
            .run()
            .unwrap()
            .iter()
            .map(|h| h.id)
            .collect();
        let truth = flat_topk(data.iter().map(|(i, v)| (*i, v)), &q, 10, |_| true);
        total += recall(&hits, &truth);
    }
    let avg = total / queries as f64;
    assert!(
        avg >= 0.85,
        "4bit ivfpq+opq full-probe recall@10 = {avg:.3}"
    );
}
