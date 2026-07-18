//! primary → replica の E2E 同期テスト (todo 904)。
//! 実 TCP で primary を立て、ReplicaSync::sync_once で同期する。

use std::sync::Arc;

use hamane::{CollectionConfig, Database, HamaneError, Metric, Record};
use hamane_server::replica::ReplicaSync;

const KEY: &str = "sync-key";

/// primary サーバーを 127.0.0.1 の空きポートで立てて base URL を返す。
async fn serve_primary(db: Arc<Database>) -> String {
    let app = hamane_server::router_with_auth(db, Some(KEY.into()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn sync(s: &mut ReplicaSync) {
    tokio::task::block_in_place(|| s.sync_once()).expect("sync_once");
}

fn vec4(x: f32) -> Vec<f32> {
    vec![x, 0.0, 0.0, 0.0]
}

#[tokio::test(flavor = "multi_thread")]
async fn wal_tail_snapshot_and_readonly() {
    let primary_dir = tempfile::tempdir().unwrap();
    let replica_dir = tempfile::tempdir().unwrap();

    let primary = Arc::new(Database::open(primary_dir.path()).unwrap());
    let col = primary
        .create_collection(
            "docs",
            CollectionConfig {
                dim: 4,
                metric: Metric::L2,
            },
        )
        .unwrap();
    col.upsert(Record::new(1, vec4(1.0)).with_meta("lang", "ja"))
        .unwrap();
    col.upsert(Record::new(2, vec4(2.0))).unwrap();
    let base = serve_primary(Arc::clone(&primary)).await;

    let replica = Arc::new(Database::open_replica(replica_dir.path(), Default::default()).unwrap());
    let mut s = ReplicaSync::new(base, Some(KEY.into()), Arc::clone(&replica));

    // 1. WAL tail のみ (フラッシュ前) の同期
    sync(&mut s);
    let rcol = replica.collection("docs").unwrap();
    assert_eq!(rcol.len(), 2);
    let hits = rcol.search(&vec4(1.0)).k(1).run().unwrap();
    assert_eq!(hits[0].id, 1);
    assert_eq!(
        hits[0].metadata.get("lang").map(|v| format!("{v:?}")),
        Some("Str(\"ja\")".into())
    );

    // 2. フラッシュ跨ぎ (スナップショット同期) + 新 WAL の tail
    primary.flush().unwrap();
    col.upsert(Record::new(3, vec4(3.0))).unwrap();
    col.delete(2u64).unwrap();
    sync(&mut s);
    assert!(replica.manifest_gen() >= 1);
    let rcol = replica.collection("docs").unwrap();
    assert_eq!(rcol.len(), 2, "upsert(3) + delete(2)");
    assert!(rcol.get(2u64).is_none());
    assert_eq!(rcol.get(3u64).unwrap().vector, vec4(3.0));
    // セグメント + memtable のマージ検索
    let ids: Vec<u64> = rcol
        .search(&vec4(0.0))
        .k(10)
        .run()
        .unwrap()
        .iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(ids, vec![1, 3]);

    // 3. replica への書き込みは拒否される
    let err = rcol.upsert(Record::new(9, vec4(9.0))).unwrap_err();
    assert!(matches!(err, HamaneError::ReadOnlyReplica), "{err:?}");

    // 4. 同期は冪等 (変化がなくてもエラーにならない)
    sync(&mut s);
    assert_eq!(replica.collection("docs").unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn compaction_and_promotion() {
    let primary_dir = tempfile::tempdir().unwrap();
    let replica_dir = tempfile::tempdir().unwrap();

    let primary = Arc::new(Database::open(primary_dir.path()).unwrap());
    let col = primary
        .create_collection(
            "docs",
            CollectionConfig {
                dim: 4,
                metric: Metric::L2,
            },
        )
        .unwrap();
    // 2 世代 + コンパクション (上書き・削除の物理適用)
    for i in 0..10u64 {
        col.upsert(Record::new(i, vec4(i as f32))).unwrap();
    }
    primary.flush().unwrap();
    for i in 0..5u64 {
        col.delete(i).unwrap();
    }
    primary.flush().unwrap();
    primary.compact().unwrap();
    let base = serve_primary(Arc::clone(&primary)).await;

    let replica = Arc::new(Database::open_replica(replica_dir.path(), Default::default()).unwrap());
    let mut s = ReplicaSync::new(base, Some(KEY.into()), Arc::clone(&replica));
    sync(&mut s);
    assert_eq!(replica.collection("docs").unwrap().len(), 5);

    // 昇格: puller と replica ハンドルを落とし、通常モードで開き直す
    drop(s);
    drop(replica);
    let promoted = Database::open(replica_dir.path()).unwrap();
    let col = promoted.collection("docs").unwrap();
    assert_eq!(col.len(), 5);
    col.upsert(Record::new(100, vec4(100.0))).unwrap();
    assert_eq!(col.len(), 6, "昇格後は書き込める");
}

#[tokio::test(flavor = "multi_thread")]
async fn replica_survives_restart_mid_stream() {
    let primary_dir = tempfile::tempdir().unwrap();
    let replica_dir = tempfile::tempdir().unwrap();

    let primary = Arc::new(Database::open(primary_dir.path()).unwrap());
    let col = primary
        .create_collection(
            "docs",
            CollectionConfig {
                dim: 4,
                metric: Metric::L2,
            },
        )
        .unwrap();
    col.upsert(Record::new(1, vec4(1.0))).unwrap();
    let base = serve_primary(Arc::clone(&primary)).await;

    // 1 回同期してから replica を落とし、primary に追記後、開き直して再同期
    {
        let replica =
            Arc::new(Database::open_replica(replica_dir.path(), Default::default()).unwrap());
        let mut s = ReplicaSync::new(base.clone(), Some(KEY.into()), Arc::clone(&replica));
        sync(&mut s);
        assert_eq!(replica.collection("docs").unwrap().len(), 1);
    }
    col.upsert(Record::new(2, vec4(2.0))).unwrap();

    let replica = Arc::new(Database::open_replica(replica_dir.path(), Default::default()).unwrap());
    // ローカル WAL のリプレイで 1 件目は同期前から見える
    assert_eq!(replica.collection("docs").unwrap().len(), 1);
    let mut s = ReplicaSync::new(base, Some(KEY.into()), Arc::clone(&replica));
    sync(&mut s);
    assert_eq!(replica.collection("docs").unwrap().len(), 2);
}
