//! 永続化 (M2) の公開 API 統合テスト: open / flush / 再 open / マージ検索。

use hamane::{CollectionConfig, Database, Filter, Metric, Record, StoreOptions};

fn config(dim: usize, metric: Metric) -> CollectionConfig {
    CollectionConfig { dim, metric }
}

#[test]
fn reopen_restores_from_wal() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        let col = db
            .create_collection("docs", config(3, Metric::Cosine))
            .unwrap();
        col.upsert(Record::new(1, vec![1.0, 0.0, 0.0]).with_meta("lang", "ja"))
            .unwrap();
        col.upsert(Record::new(2, vec![0.0, 1.0, 0.0]).with_meta("lang", "en"))
            .unwrap();
        col.delete(2).unwrap();
        // flush せずに drop (クラッシュ相当)
    }
    let db = Database::open(dir.path()).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), 1);
    let rec = col.get(1).unwrap();
    assert_eq!(rec.metadata.get("lang"), Some(&"ja".into()));
    assert!(col.get(2).is_none());

    let hits = col.search(&[1.0, 0.0, 0.0]).k(5).run().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 1);
}

#[test]
fn create_collection_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        db.create_collection("a", config(2, Metric::L2)).unwrap();
    }
    let db = Database::open(dir.path()).unwrap();
    assert_eq!(db.collection_names(), vec!["a"]);
    let col = db.collection("a").unwrap();
    assert_eq!(col.config().dim, 2);
    assert_eq!(col.config().metric, Metric::L2);
}

#[test]
fn search_merges_memtable_and_segments() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path()).unwrap();
    let col = db.create_collection("pts", config(1, Metric::L2)).unwrap();

    // セグメント 1: id 0..5
    for i in 0..5u64 {
        col.upsert(Record::new(i, vec![i as f32]).with_meta("gen", 1))
            .unwrap();
    }
    col.flush().unwrap();
    // セグメント 2: id 2 を上書き、id 3 を削除
    col.upsert(Record::new(2, vec![100.0]).with_meta("gen", 2))
        .unwrap();
    col.delete(3).unwrap();
    col.flush().unwrap();
    // memtable: id 4 を上書き、id 9 を追加
    col.upsert(Record::new(4, vec![200.0]).with_meta("gen", 3))
        .unwrap();
    col.upsert(Record::new(9, vec![9.0]).with_meta("gen", 3))
        .unwrap();

    // 原点近傍: 0, 1 (2 は 100 に移動、3 は削除済み)
    let hits = col.search(&[0.0]).k(3).run().unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![0, 1, 9]);

    // 上書きされた値と世代が見えること
    let hits = col.search(&[100.0]).k(1).run().unwrap();
    assert_eq!(hits[0].id, 2);
    assert_eq!(hits[0].metadata.get("gen"), Some(&2.into()));
    let hits = col.search(&[200.0]).k(1).run().unwrap();
    assert_eq!(hits[0].id, 4);

    assert_eq!(col.len(), 5); // 0,1,2,4,9

    // 再 open しても同じ
    drop(hits);
    drop(col);
    drop(db);
    let db = Database::open(dir.path()).unwrap();
    let col = db.collection("pts").unwrap();
    assert_eq!(col.len(), 5);
    let hits = col.search(&[0.0]).k(3).run().unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![0, 1, 9]);
}

#[test]
fn filtered_search_across_segments() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path()).unwrap();
    let col = db.create_collection("docs", config(1, Metric::L2)).unwrap();
    for i in 0..10u64 {
        col.upsert(Record::new(i, vec![i as f32]).with_meta("even", i % 2 == 0))
            .unwrap();
        if i == 4 {
            col.flush().unwrap(); // 前半をセグメントへ
        }
    }
    let hits = col
        .search(&[0.0])
        .k(3)
        .filter(Filter::eq("even", true))
        .run()
        .unwrap();
    assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![0, 2, 4]);
}

#[test]
fn auto_flush_by_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with_options(
        dir.path(),
        StoreOptions {
            flush_threshold_bytes: 128,
            ..Default::default()
        },
    )
    .unwrap();
    let col = db
        .create_collection("docs", config(16, Metric::L2))
        .unwrap();
    for i in 0..50u64 {
        col.upsert(Record::new(i, vec![i as f32; 16])).unwrap();
    }
    assert_eq!(col.len(), 50);
    let hits = col.search(&[0.0; 16]).k(1).run().unwrap();
    assert_eq!(hits[0].id, 0);

    // フラッシュ後に再 open してもすべて残っている
    drop(hits);
    drop(col);
    drop(db);
    let db = Database::open(dir.path()).unwrap();
    assert_eq!(db.collection("docs").unwrap().len(), 50);
}

#[test]
fn upsert_batch_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        let col = db
            .create_collection("docs", config(2, Metric::Dot))
            .unwrap();
        let records: Vec<Record> = (0..100u64)
            .map(|i| Record::new(i, vec![i as f32, 1.0]))
            .collect();
        col.upsert_batch(records).unwrap();
        assert_eq!(col.len(), 100);
    }
    let db = Database::open(dir.path()).unwrap();
    assert_eq!(db.collection("docs").unwrap().len(), 100);
}

#[test]
fn drop_collection_persists() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        let col = db.create_collection("a", config(1, Metric::L2)).unwrap();
        col.upsert(Record::new(1, vec![1.0])).unwrap();
        col.flush().unwrap();
        db.drop_collection("a").unwrap();
    }
    let db = Database::open(dir.path()).unwrap();
    assert!(db.collection_names().is_empty());
    // 同名で再作成しても古いデータは見えない
    let col = db.create_collection("a", config(1, Metric::L2)).unwrap();
    assert_eq!(col.len(), 0);
}

/// collection のリネーム (todo 1801)。WAL リプレイとフラッシュ後の両方で保たれる。
#[test]
fn rename_collection_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        let col = db.create_collection("docs", config(2, Metric::L2)).unwrap();
        for i in 0..10u64 {
            col.upsert(Record::new(i, vec![i as f32, 0.0])).unwrap();
        }
        db.rename_collection("docs", "documents").unwrap();

        // 新しい名前で開けて中身は同じ
        assert!(db.collection("docs").is_err());
        let renamed = db.collection("documents").unwrap();
        assert_eq!(renamed.len(), 10);
        assert_eq!(renamed.get(3u64).unwrap().vector, vec![3.0, 0.0]);
    }

    // 再 open (WAL リプレイ) をまたいで保たれる
    {
        let db = Database::open(dir.path()).unwrap();
        assert_eq!(db.collection_names(), vec!["documents".to_string()]);
        let col = db.collection("documents").unwrap();
        assert_eq!(col.len(), 10);
        col.flush().unwrap(); // manifest に載せる
    }

    // フラッシュ後 (manifest 経由) でも保たれる
    {
        let db = Database::open(dir.path()).unwrap();
        let col = db.collection("documents").unwrap();
        assert_eq!(col.len(), 10);
        assert_eq!(col.search(&[3.0, 0.0]).k(1).run().unwrap()[0].id, 3);

        // 検証: 既存名への改名は拒否、無い名前はエラー、同名は no-op
        db.create_collection("other", config(2, Metric::L2))
            .unwrap();
        assert!(db.rename_collection("documents", "other").is_err());
        assert!(db.rename_collection("missing", "x").is_err());
        db.rename_collection("documents", "documents").unwrap();
        assert_eq!(db.collection("documents").unwrap().len(), 10);
    }
}

/// 名前の原子的な入れ替え = 無停止の索引差し替え (todo 1801)。
#[test]
fn swap_collections_switches_implementations() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = Database::open(dir.path()).unwrap();
        // 稼働中の実体
        let live = db.create_collection("docs", config(2, Metric::L2)).unwrap();
        for i in 0..5u64 {
            live.upsert(Record::new(i, vec![i as f32, 0.0]).with_meta("gen", "old"))
                .unwrap();
        }
        // 裏で作り直した実体
        let staging = db
            .create_collection("docs_v2", config(2, Metric::L2))
            .unwrap();
        for i in 0..20u64 {
            staging
                .upsert(Record::new(i, vec![i as f32, 1.0]).with_meta("gen", "new"))
                .unwrap();
        }

        db.swap_collections("docs", "docs_v2").unwrap();

        // 読み手は "docs" のまま新実体を見る
        let docs = db.collection("docs").unwrap();
        assert_eq!(docs.len(), 20);
        assert_eq!(
            docs.get(0u64).unwrap().metadata.get("gen"),
            Some(&hamane::MetaValue::Str("new".into()))
        );
        // 旧実体は "docs_v2" として残る (検証してから drop できる)
        let old = db.collection("docs_v2").unwrap();
        assert_eq!(old.len(), 5);
        assert_eq!(
            old.get(0u64).unwrap().metadata.get("gen"),
            Some(&hamane::MetaValue::Str("old".into()))
        );
    }

    // 再 open しても入れ替わったまま (名前の重複も起きない)
    {
        let db = Database::open(dir.path()).unwrap();
        let mut names = db.collection_names();
        names.sort();
        assert_eq!(names, vec!["docs".to_string(), "docs_v2".to_string()]);
        assert_eq!(db.collection("docs").unwrap().len(), 20);
        assert_eq!(db.collection("docs_v2").unwrap().len(), 5);

        // 片方が無ければエラー
        assert!(db.swap_collections("docs", "missing").is_err());
        // 同じ collection 同士は no-op
        db.swap_collections("docs", "docs").unwrap();
        assert_eq!(db.collection("docs").unwrap().len(), 20);
    }
}
