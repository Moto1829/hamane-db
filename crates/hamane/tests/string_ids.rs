//! 文字列 ID (todo 601) の統合テスト。
//! u64 API との後方互換・辞書のクラッシュ耐性 (WAL/セグメント復元) を検証する。

use hamane::{CollectionConfig, Database, Metric, Record, RecordId};

fn config(dim: usize) -> CollectionConfig {
    CollectionConfig {
        dim,
        metric: Metric::L2,
    }
}

#[test]
fn string_id_crud_roundtrip() {
    let db = Database::in_memory();
    let col = db.create_collection("docs", config(2)).unwrap();

    col.upsert(Record::new("doc-a", vec![1.0, 0.0]).with_meta("lang", "ja"))
        .unwrap();
    col.upsert(Record::new("doc-b", vec![0.0, 1.0])).unwrap();
    assert_eq!(col.len(), 2);

    // get (文字列)
    let rec = col.get("doc-a").unwrap();
    assert_eq!(rec.id, RecordId::Str("doc-a".into()));
    assert_eq!(rec.vector, vec![1.0, 0.0]);

    // 上書き: 同じ文字列 ID は同じ内部 ID に解決され、件数は増えない
    col.upsert(Record::new("doc-a", vec![5.0, 5.0])).unwrap();
    assert_eq!(col.len(), 2);
    assert_eq!(col.get("doc-a").unwrap().vector, vec![5.0, 5.0]);

    // 検索結果から文字列 ID を取れる
    let hits = col.search(&[0.0, 1.0]).k(1).run().unwrap();
    assert_eq!(hits[0].ext_id(), Some("doc-b"));

    // 削除
    assert!(col.delete("doc-a").unwrap());
    assert!(!col.delete("doc-a").unwrap());
    assert!(col.get("doc-a").is_none());
    assert_eq!(col.len(), 1);

    // 未知の文字列 ID
    assert!(col.get("missing").is_none());
    assert!(!col.delete("missing").unwrap());
}

#[test]
fn u64_and_string_ids_coexist() {
    let db = Database::in_memory();
    let col = db.create_collection("docs", config(1)).unwrap();
    // u64 API は従来どおり (EXT_ID_BASE 未満を推奨)
    col.upsert(Record::new(1u64, vec![1.0])).unwrap();
    col.upsert(Record::new("one", vec![10.0])).unwrap();
    assert_eq!(col.len(), 2);
    assert_eq!(col.get(1u64).unwrap().vector, vec![1.0]);
    assert_eq!(col.get("one").unwrap().vector, vec![10.0]);
}

#[test]
fn string_id_survives_wal_replay_and_flush() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        let col = db.create_collection("docs", config(1)).unwrap();
        col.upsert(Record::new("wal-only", vec![1.0])).unwrap();
        col.upsert(Record::new("flushed", vec![2.0])).unwrap();
        db.flush().unwrap();
        col.upsert(Record::new("after-flush", vec![3.0])).unwrap();
        // flush せず drop → after-flush は WAL からのリプレイで復元される
    }
    let db = Database::open(dir.path()).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), 3);
    // セグメント由来・WAL 由来の両方の辞書が復元される
    assert_eq!(col.get("flushed").unwrap().vector, vec![2.0]);
    assert_eq!(col.get("after-flush").unwrap().vector, vec![3.0]);
    assert_eq!(col.get("wal-only").unwrap().vector, vec![1.0]);

    // 復元後の上書きが新規レコードにならない (内部 ID が同じ)
    col.upsert(Record::new("flushed", vec![20.0])).unwrap();
    assert_eq!(col.len(), 3);
    assert_eq!(col.get("flushed").unwrap().vector, vec![20.0]);

    // 復元後の新規挿入が既存の内部 ID と衝突しない (採番の復元)
    col.upsert(Record::new("new-after-reopen", vec![4.0]))
        .unwrap();
    assert_eq!(col.len(), 4);
    for key in ["flushed", "after-flush", "wal-only", "new-after-reopen"] {
        assert!(col.get(key).is_some(), "{key} must exist");
    }
}

#[test]
fn deleted_string_id_can_be_reinserted() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path()).unwrap();
    let col = db.create_collection("docs", config(1)).unwrap();
    col.upsert(Record::new("x", vec![1.0])).unwrap();
    col.delete("x").unwrap();
    col.upsert(Record::new("x", vec![2.0])).unwrap();
    assert_eq!(col.get("x").unwrap().vector, vec![2.0]);
    assert_eq!(col.len(), 1);
    db.flush().unwrap();
    drop(col);
    drop(db);
    let db = Database::open(dir.path()).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.get("x").unwrap().vector, vec![2.0]);
    assert_eq!(col.len(), 1);
}
