//! 公開 API 経由の統合テスト (M1 完了条件: upsert / delete / search が動く)。

use hamane::{CollectionConfig, Database, Filter, HamaneError, Metric, Record};

fn db_with_docs(metric: Metric) -> Database {
    let db = Database::in_memory();
    let col = db
        .create_collection("docs", CollectionConfig { dim: 3, metric })
        .unwrap();
    col.upsert(Record::new(1, vec![1.0, 0.0, 0.0]).with_meta("lang", "ja"))
        .unwrap();
    col.upsert(Record::new(2, vec![0.0, 1.0, 0.0]).with_meta("lang", "en"))
        .unwrap();
    col.upsert(Record::new(3, vec![0.9, 0.1, 0.0]).with_meta("lang", "ja"))
        .unwrap();
    db
}

#[test]
fn upsert_search_delete_roundtrip() {
    let db = db_with_docs(Metric::Cosine);
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), 3);

    let hits = col.search(&[1.0, 0.0, 0.0]).k(2).run().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, 1);
    assert_eq!(hits[1].id, 3);
    assert!((hits[0].score - 1.0).abs() < 1e-5); // 自分自身とのコサイン類似度は 1

    assert!(col.delete(1).unwrap());
    assert!(!col.delete(1).unwrap()); // 二重削除は false
    let hits = col.search(&[1.0, 0.0, 0.0]).k(2).run().unwrap();
    assert_eq!(hits[0].id, 3);
}

#[test]
fn upsert_replaces_existing_record() {
    let db = db_with_docs(Metric::L2);
    let col = db.collection("docs").unwrap();
    col.upsert(Record::new(1, vec![5.0, 5.0, 5.0]).with_meta("lang", "fr"))
        .unwrap();
    assert_eq!(col.len(), 3); // 置き換えなので件数は不変

    let rec = col.get(1).unwrap();
    assert_eq!(rec.vector, vec![5.0, 5.0, 5.0]);
    let hits = col.search(&[5.0, 5.0, 5.0]).k(1).run().unwrap();
    assert_eq!(hits[0].id, 1);
    assert_eq!(hits[0].metadata.get("lang"), Some(&"fr".into()));
}

#[test]
fn filtered_search() {
    let db = db_with_docs(Metric::Cosine);
    let col = db.collection("docs").unwrap();
    // クエリは id=2 (en) に最も近いが、ja フィルタで除外される
    let hits = col
        .search(&[0.0, 1.0, 0.0])
        .k(10)
        .filter(Filter::eq("lang", "ja"))
        .run()
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits
        .iter()
        .all(|h| h.metadata.get("lang") == Some(&"ja".into())));
    assert_eq!(hits[0].id, 3); // ja のうち [0,1,0] に近いのは 3
}

#[test]
fn dimension_and_vector_validation() {
    let db = db_with_docs(Metric::Cosine);
    let col = db.collection("docs").unwrap();

    let err = col.upsert(Record::new(9, vec![1.0, 2.0])).unwrap_err();
    assert!(matches!(
        err,
        HamaneError::DimensionMismatch {
            expected: 3,
            actual: 2
        }
    ));

    let err = col
        .upsert(Record::new(9, vec![f32::NAN, 0.0, 0.0]))
        .unwrap_err();
    assert!(matches!(err, HamaneError::InvalidVector(_)));

    // Cosine ではゼロベクトルを拒否する
    let err = col.upsert(Record::new(9, vec![0.0; 3])).unwrap_err();
    assert!(matches!(err, HamaneError::InvalidVector(_)));

    let err = col.search(&[1.0]).run().unwrap_err();
    assert!(matches!(err, HamaneError::DimensionMismatch { .. }));
}

#[test]
fn collection_lifecycle() {
    let db = Database::in_memory();
    let config = CollectionConfig {
        dim: 2,
        metric: Metric::L2,
    };
    db.create_collection("a", config).unwrap();

    let err = db.create_collection("a", config).unwrap_err();
    assert!(matches!(err, HamaneError::CollectionExists(_)));

    let err = db
        .create_collection(
            "bad",
            CollectionConfig {
                dim: 0,
                metric: Metric::L2,
            },
        )
        .unwrap_err();
    assert!(matches!(err, HamaneError::InvalidConfig(_)));

    assert_eq!(db.collection_names(), vec!["a"]);
    db.drop_collection("a").unwrap();
    assert!(matches!(
        db.collection("a").unwrap_err(),
        HamaneError::CollectionNotFound(_)
    ));
}

#[test]
fn l2_scores_are_distances() {
    let db = Database::in_memory();
    let col = db
        .create_collection(
            "pts",
            CollectionConfig {
                dim: 1,
                metric: Metric::L2,
            },
        )
        .unwrap();
    for i in 0..5u64 {
        col.upsert(Record::new(i, vec![i as f32])).unwrap();
    }
    let hits = col.search(&[0.0]).k(5).run().unwrap();
    let ids: Vec<_> = hits.iter().map(|h| h.id).collect();
    assert_eq!(ids, vec![0, 1, 2, 3, 4]);
    assert!((hits[3].score - 3.0).abs() < 1e-5);
}
