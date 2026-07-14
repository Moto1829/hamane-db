//! プロパティテスト (todos/211): ランダムな操作系列で実装と参照モデルを比較する。
//!
//! 参照モデルは HashMap + 全探索。Upsert / Delete / Flush / Reopen を
//! 任意順で適用し、各ステップ後に get と search (フィルタあり/なし) の
//! 結果が完全一致することを検証する。

use std::collections::HashMap;

use hamane::{CollectionConfig, Database, Filter, Metric, Record, StoreOptions, SyncPolicy};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Upsert { id: u64, v: i8, even: bool },
    Delete { id: u64 },
    Flush,
    Compact,
    Reopen,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => (0u64..8, -8i8..8, any::<bool>())
            .prop_map(|(id, v, even)| Op::Upsert { id, v, even }),
        2 => (0u64..8).prop_map(|id| Op::Delete { id }),
        1 => Just(Op::Flush),
        1 => Just(Op::Compact),
        1 => Just(Op::Reopen),
    ]
}

/// 参照モデル: id → (値, evenフラグ)
type Model = HashMap<u64, (f32, bool)>;

fn expected_search(model: &Model, q: f32, k: usize, even_only: bool) -> Vec<u64> {
    let mut entries: Vec<(f32, u64)> = model
        .iter()
        .filter(|(_, (_, even))| !even_only || *even)
        .map(|(id, (v, _))| ((v - q) * (v - q), *id))
        .collect();
    entries.sort_by(|a, b| a.partial_cmp(b).unwrap());
    entries.truncate(k);
    entries.into_iter().map(|(_, id)| id).collect()
}

fn check_consistency(db: &Database, model: &Model) {
    let col = db.collection("c").unwrap();
    // get
    for id in 0..8u64 {
        let actual = col.get(id).map(|r| r.vector[0]);
        let expected = model.get(&id).map(|(v, _)| *v);
        assert_eq!(actual, expected, "get({id})");
    }
    // len
    assert_eq!(col.len(), model.len());
    // search
    for q in [-5.0f32, 0.0, 5.0] {
        for k in [1usize, 3, 10] {
            let hits = col.search(&[q]).k(k).run().unwrap();
            let actual: Vec<u64> = hits.iter().map(|h| h.id).collect();
            assert_eq!(
                actual,
                expected_search(model, q, k, false),
                "search q={q} k={k}"
            );

            let hits = col
                .search(&[q])
                .k(k)
                .filter(Filter::eq("even", true))
                .run()
                .unwrap();
            let actual: Vec<u64> = hits.iter().map(|h| h.id).collect();
            assert_eq!(
                actual,
                expected_search(model, q, k, true),
                "filtered search q={q} k={k}"
            );
        }
    }
}

fn open_db(path: &std::path::Path) -> Database {
    Database::open_with_options(
        path,
        StoreOptions {
            // テスト高速化: fsync を実質無効化 (プロセスは生きたまま reopen するので
            // OS ページキャッシュ経由でデータは見える)
            sync: SyncPolicy::EveryN(1_000_000),
            ..Default::default()
        },
    )
    .unwrap()
}

fn run_case(ops: &[Op]) {
    let dir = tempfile::tempdir().unwrap();
    let mut db = open_db(dir.path());
    let _ = &db; // 束縛を明示 (Reopen で drop → open の順序を保つため下記参照)
    db.create_collection(
        "c",
        CollectionConfig {
            dim: 1,
            metric: Metric::L2,
        },
    )
    .unwrap();
    let mut model: Model = HashMap::new();

    for op in ops {
        match op {
            Op::Upsert { id, v, even } => {
                let col = db.collection("c").unwrap();
                col.upsert(Record::new(*id, vec![*v as f32]).with_meta("even", *even))
                    .unwrap();
                model.insert(*id, (*v as f32, *even));
            }
            Op::Delete { id } => {
                let col = db.collection("c").unwrap();
                let existed = col.delete(*id).unwrap();
                assert_eq!(existed, model.remove(id).is_some(), "delete({id}) existed");
            }
            Op::Flush => {
                db.flush().unwrap();
            }
            Op::Compact => {
                db.compact().unwrap();
            }
            Op::Reopen => {
                // プロセスロック (todo 702) があるため、先に閉じてから開き直す
                drop(db);
                db = open_db(dir.path());
            }
        }
        check_consistency(&db, &model);
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    #[test]
    fn matches_reference_model(ops in prop::collection::vec(op_strategy(), 1..40)) {
        run_case(&ops);
    }
}

/// 回帰: 「flush 後の上書き → reopen」の固定シナリオ (proptest で見つかった場合の置き場)。
#[test]
fn fixed_scenario_flush_overwrite_reopen() {
    run_case(&[
        Op::Upsert {
            id: 1,
            v: 1,
            even: true,
        },
        Op::Upsert {
            id: 2,
            v: 2,
            even: false,
        },
        Op::Flush,
        Op::Upsert {
            id: 1,
            v: -3,
            even: false,
        },
        Op::Delete { id: 2 },
        Op::Reopen,
        Op::Upsert {
            id: 3,
            v: 0,
            even: true,
        },
        Op::Flush,
        Op::Delete { id: 1 },
        Op::Compact,
        Op::Reopen,
    ]);
}
