//! 構成の組み合わせ網羅テスト (todo 1301 D)。
//!
//! index × quantization × pq_nbits × opq の**許可された全構成**について、
//! 同じデータで CRUD → フラッシュ → 再 open → 検索を回し、
//!
//! - 返る score が常に**正確な f32 距離**であること (量子化は 1 段目だけ)
//! - recall が閾値以上であること
//! - フィルタ (pre/post 両経路) と ID 種別 (u64 / 文字列) に影響がないこと
//! - メトリック 3 種で成立すること
//!
//! を確認する。個々の構成は他のテストでも見ているが、ここでは
//! 「組み合わせで壊れていないか」をテーブル駆動でまとめて押さえる。

mod common;

use common::{dataset, flat_topk, random_vec, recall, records};
use hamane::{
    CollectionConfig, Database, Filter, IndexKind, MetaValue, Metric, Quantization, Record,
    StoreOptions,
};
use rand::rngs::StdRng;
use rand::SeedableRng;

const DIM: usize = 16;
const N: u64 = 1200;

/// 検証する構成。`StoreOptions` の量子化・索引まわりだけを持つ。
#[derive(Clone, Copy)]
struct Config {
    name: &'static str,
    index: IndexKind,
    quantization: Option<Quantization>,
    pq_nbits: u8,
    opq: bool,
    /// この構成で満たすべき recall@10 の下限 (nprobe/ef は既定値)
    min_recall: f64,
}

/// docs/design/quantization.md §5 + opq.md §4 + quantization.md §8 の
/// 許可される組み合わせを網羅する。
fn configs() -> Vec<Config> {
    let pq = |m: Option<usize>| Some(Quantization::Pq { m });
    vec![
        Config {
            name: "f32-hnsw",
            index: IndexKind::Hnsw,
            quantization: None,
            pq_nbits: 8,
            opq: false,
            min_recall: 0.95,
        },
        Config {
            name: "sq8-hnsw",
            index: IndexKind::Hnsw,
            quantization: Some(Quantization::Sq8),
            pq_nbits: 8,
            opq: false,
            min_recall: 0.95,
        },
        Config {
            name: "pq8-hnsw",
            index: IndexKind::Hnsw,
            quantization: pq(None),
            pq_nbits: 8,
            opq: false,
            min_recall: 0.95,
        },
        Config {
            name: "pq8-opq-hnsw",
            index: IndexKind::Hnsw,
            quantization: pq(None),
            pq_nbits: 8,
            opq: true,
            min_recall: 0.95,
        },
        Config {
            name: "pq4-hnsw",
            index: IndexKind::Hnsw,
            // 4bit は m を倍にしてコード長を揃える (docs/design/quantization.md §8.2)
            quantization: pq(Some(8)),
            pq_nbits: 4,
            opq: false,
            min_recall: 0.90,
        },
        Config {
            name: "pq4-opq-hnsw",
            index: IndexKind::Hnsw,
            quantization: pq(Some(8)),
            pq_nbits: 4,
            opq: true,
            min_recall: 0.90,
        },
        Config {
            name: "ivf",
            index: IndexKind::Ivf,
            quantization: None,
            pq_nbits: 8,
            opq: false,
            // nprobe 既定 8 なので枝刈りのぶん低め
            min_recall: 0.50,
        },
        Config {
            name: "ivfpq8",
            index: IndexKind::IvfPq,
            quantization: pq(None),
            pq_nbits: 8,
            opq: false,
            min_recall: 0.50,
        },
        Config {
            name: "ivfpq4-opq",
            index: IndexKind::IvfPq,
            quantization: pq(Some(8)),
            pq_nbits: 4,
            opq: true,
            min_recall: 0.45,
        },
    ]
}

impl Config {
    fn options(&self) -> StoreOptions {
        StoreOptions {
            hnsw_min_rows: 64,
            index: self.index,
            quantization: self.quantization,
            pq_nbits: self.pq_nbits,
            opq: self.opq,
            ..Default::default()
        }
    }
}

fn open(dir: &std::path::Path, config: &Config) -> Database {
    Database::open_with_options(dir, config.options())
        .unwrap_or_else(|e| panic!("{}: open failed: {e}", config.name))
}

fn create(db: &Database, metric: Metric) -> std::sync::Arc<hamane::Collection> {
    db.create_collection("docs", CollectionConfig { dim: DIM, metric })
        .unwrap()
}

/// 全構成で CRUD → フラッシュ → 再 open → 検索が成立すること。
/// score は常に正確な f32 距離 (量子化は候補選びにしか使わない)。
#[test]
fn every_config_survives_crud_flush_and_reopen() {
    for config in configs() {
        let dir = tempfile::tempdir().unwrap();
        let data = dataset(N, DIM, 1301);
        {
            let db = open(dir.path(), &config);
            let col = create(&db, Metric::L2);
            col.upsert_batch(records(&data)).unwrap();

            // 上書きと削除を混ぜてからフラッシュする
            col.upsert(Record::new(0, data[1].1.clone())).unwrap();
            col.delete(1u64).unwrap();
            col.flush().unwrap();
            assert_eq!(
                col.len(),
                N as usize - 1,
                "{}: len after delete",
                config.name
            );
            assert!(
                col.get(1u64).is_none(),
                "{}: deleted row visible",
                config.name
            );
            assert_eq!(
                col.get(0u64).unwrap().vector,
                data[1].1,
                "{}: overwrite lost",
                config.name
            );
        }

        // 再 open して検索する (索引・量子化ファイルが読み直される)
        let db = open(dir.path(), &config);
        let col = db.collection("docs").unwrap();
        assert_eq!(
            col.len(),
            N as usize - 1,
            "{}: len after reopen",
            config.name
        );

        let mut rng = StdRng::seed_from_u64(7);
        let live: Vec<(u64, Vec<f32>)> = data
            .iter()
            .filter(|(id, _)| *id != 1)
            .map(|(id, v)| {
                // id=0 は data[1] のベクトルで上書き済み
                let v = if *id == 0 {
                    data[1].1.clone()
                } else {
                    v.clone()
                };
                (*id, v)
            })
            .collect();

        let mut total = 0.0;
        let queries = 20;
        for _ in 0..queries {
            let q = random_vec(&mut rng, DIM);
            let hits = col.search(&q).k(10).run().unwrap();
            assert!(!hits.is_empty(), "{}: empty result", config.name);

            // score は正確な f32 距離
            let top = &hits[0];
            let vector = &live.iter().find(|(id, _)| *id == top.id).unwrap().1;
            let exact = Metric::L2.score_from_key(Metric::L2.distance_key(&q, vector));
            assert!(
                (top.score - exact).abs() < 1e-4,
                "{}: score {} is not the exact distance {exact}",
                config.name,
                top.score
            );

            let ids: Vec<u64> = hits.iter().map(|h| h.id).collect();
            let truth = flat_topk(
                live.iter().map(|(i, v)| (*i, v)),
                &q,
                10,
                Metric::L2,
                |_| true,
            );
            total += recall(&ids, &truth);
        }
        let avg = total / queries as f64;
        assert!(
            avg >= config.min_recall,
            "{}: recall@10 = {avg:.3} < {}",
            config.name,
            config.min_recall
        );
    }
}

/// 主要構成 × メトリック 3 種。Cosine は正規化が入るので別扱いになる経路。
#[test]
fn configs_work_for_every_metric() {
    let subset = ["f32-hnsw", "sq8-hnsw", "pq8-hnsw", "pq4-hnsw", "ivfpq8"];
    for config in configs().into_iter().filter(|c| subset.contains(&c.name)) {
        for metric in [Metric::L2, Metric::Cosine, Metric::Dot] {
            let dir = tempfile::tempdir().unwrap();
            let data = dataset(N, DIM, 99);
            let db = open(dir.path(), &config);
            let col = create(&db, metric);
            col.upsert_batch(records(&data)).unwrap();
            col.flush().unwrap();

            // Cosine は投入時に正規化されるので、正解は**保存されたベクトル**で作る
            // (クエリ側の正規化は検索が内部で行う。内積のランキングは
            //  クエリのスケールに依らないので正解計算では正規化不要)
            let stored: Vec<(u64, Vec<f32>)> = data
                .iter()
                .map(|(id, _)| (*id, col.get(*id).unwrap().vector.clone()))
                .collect();

            let mut rng = StdRng::seed_from_u64(5);
            let mut total = 0.0;
            let queries = 10;
            for _ in 0..queries {
                let q = random_vec(&mut rng, DIM);
                let ids: Vec<u64> = col
                    .search(&q)
                    .k(10)
                    .run()
                    .unwrap()
                    .iter()
                    .map(|h| h.id)
                    .collect();
                let truth = flat_topk(stored.iter().map(|(i, v)| (*i, v)), &q, 10, metric, |_| {
                    true
                });
                total += recall(&ids, &truth);
            }
            let avg = total / queries as f64;
            assert!(
                avg >= config.min_recall,
                "{} + {metric:?}: recall@10 = {avg:.3}",
                config.name
            );
        }
    }
}

/// 構成 × フィルタ経路 (pre / post) × ID 種別 (u64 / 文字列)。
///
/// pre-filter は選択率が低いとき (ここでは 1/10)、post-filter は高いとき
/// (1/2) に選ばれる。どちらの経路でも一致しないレコードが漏れないこと。
#[test]
fn configs_work_with_filters_and_string_ids() {
    let subset = ["f32-hnsw", "sq8-hnsw", "pq4-hnsw", "ivf", "ivfpq8"];
    for config in configs().into_iter().filter(|c| subset.contains(&c.name)) {
        for string_ids in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let data = dataset(N, DIM, 424);
            let db = open(dir.path(), &config);
            let col = create(&db, Metric::L2);

            // group: 1/10 が "rare"、1/2 が "half"
            let recs: Vec<Record> = data
                .iter()
                .map(|(id, v)| {
                    let rec = if string_ids {
                        Record::new(format!("doc-{id}"), v.clone())
                    } else {
                        Record::new(*id, v.clone())
                    };
                    rec.with_meta("rare", *id % 10 == 0)
                        .with_meta("half", *id % 2 == 0)
                })
                .collect();
            col.upsert_batch(recs).unwrap();
            col.flush().unwrap();

            let mut rng = StdRng::seed_from_u64(11);
            for (key, keep) in [
                ("rare", 10u64), // 選択率 10% → pre-filter が選ばれる想定
                ("half", 2),     // 選択率 50% → post-filter が選ばれる想定
            ] {
                let q = random_vec(&mut rng, DIM);
                let hits = col
                    .search(&q)
                    .k(10)
                    .filter(Filter::eq(key, MetaValue::Bool(true)))
                    .run()
                    .unwrap();
                assert!(!hits.is_empty(), "{}: empty filtered result", config.name);
                for hit in &hits {
                    assert_eq!(
                        hit.metadata.get(key),
                        Some(&MetaValue::Bool(true)),
                        "{} ({key}): filter leaked a non-matching record",
                        config.name
                    );
                    if string_ids {
                        let ext = hit.metadata.get("_ext_id").expect("string id preserved");
                        let MetaValue::Str(s) = ext else {
                            panic!("_ext_id must be a string")
                        };
                        let num: u64 = s.trim_start_matches("doc-").parse().unwrap();
                        assert_eq!(
                            num % keep,
                            0,
                            "{}: filtered id {s} does not match",
                            config.name
                        );
                    } else {
                        assert_eq!(
                            hit.id % keep,
                            0,
                            "{}: filtered id {} mismatch",
                            config.name,
                            hit.id
                        );
                    }
                }
            }
        }
    }
}
