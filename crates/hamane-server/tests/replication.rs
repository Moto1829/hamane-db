//! /replication API の結合テスト (todo 902)。
//!
//! primary 側はファイルを読むだけなので、「HTTP で取れるバイト列 ==
//! ディスク上のファイル」を軸に検証する。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use hamane::{CollectionConfig, Database, Metric, Record};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::util::ServiceExt;

async fn get_raw(app: &Router, path: &str) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn get_json(app: &Router, path: &str) -> (StatusCode, Value) {
    let (status, bytes) = get_raw(app, path).await;
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// フラッシュ済みセグメント 1 個 + WAL tail (未フラッシュ 1 件) の primary を作る。
fn primary(dir: &std::path::Path) -> (Router, Arc<Database>) {
    let db = Arc::new(Database::open(dir).unwrap());
    let col = db
        .create_collection(
            "docs",
            CollectionConfig {
                dim: 4,
                metric: Metric::L2,
            },
        )
        .unwrap();
    let records: Vec<Record> = (0..64u64)
        .map(|i| Record::new(i, vec![i as f32, 0.0, 0.0, 0.0]))
        .collect();
    col.upsert_batch(records).unwrap();
    db.flush().unwrap();
    // WAL tail 用の未フラッシュ書き込み
    col.upsert(Record::new(1000, vec![9.0, 9.0, 9.0, 9.0]))
        .unwrap();
    (hamane_server::router(Arc::clone(&db)), db)
}

#[tokio::test]
async fn state_manifest_and_segment_match_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _db) = primary(dir.path());

    let (status, state) = get_json(&app, "/replication/state").await;
    assert_eq!(status, StatusCode::OK);
    let gen = state["manifest_gen"].as_u64().unwrap();
    assert!(gen >= 1, "flush should have advanced the generation");
    assert_eq!(
        state["manifest_name"].as_str().unwrap(),
        format!("MANIFEST-{gen:010}")
    );
    assert!(state["wal_seq"].as_u64().is_some());
    assert!(state["wal_len"].as_u64().unwrap() > 0, "unflushed upsert");

    // manifest のバイト列がディスクと一致
    let (status, body) = get_raw(&app, &format!("/replication/manifest/{gen}")).await;
    assert_eq!(status, StatusCode::OK);
    let on_disk = std::fs::read(dir.path().join(format!("MANIFEST-{gen:010}"))).unwrap();
    assert_eq!(body, on_disk);

    // セグメントファイルがディスクと一致 (collection_id 0 / seg_id はディスクから発見)
    let col_dir = dir.path().join("collections").join("0");
    let seg_name = std::fs::read_dir(&col_dir)
        .unwrap()
        .filter_map(|e| e.unwrap().file_name().into_string().ok())
        .find(|n| n.starts_with("seg-"))
        .expect("one flushed segment");
    let seg_id: u64 = seg_name.strip_prefix("seg-").unwrap().parse().unwrap();
    for file in ["vectors.bin", "ids.bin", "meta.bin", "tombstones.bin"] {
        let (status, body) =
            get_raw(&app, &format!("/replication/segment/0/{seg_id}/{file}")).await;
        assert_eq!(status, StatusCode::OK, "{file}");
        assert_eq!(
            body,
            std::fs::read(col_dir.join(&seg_name).join(file)).unwrap()
        );
    }
}

#[tokio::test]
async fn wal_tail_reads_by_offset() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _db) = primary(dir.path());

    let (_, state) = get_json(&app, "/replication/state").await;
    let seq = state["wal_seq"].as_u64().unwrap();
    let len = state["wal_len"].as_u64().unwrap();
    let on_disk = std::fs::read(dir.path().join("wal").join(format!("{seq:020}.wal"))).unwrap();
    assert_eq!(on_disk.len() as u64, len);

    // 全体 / 途中から / 末尾ちょうど / 末尾以降 (すべて 200、後者 2 つは空)
    let (status, body) = get_raw(&app, &format!("/replication/wal/{seq}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, on_disk);
    let (_, body) = get_raw(&app, &format!("/replication/wal/{seq}?offset=10")).await;
    assert_eq!(body, on_disk[10..]);
    let (_, body) = get_raw(&app, &format!("/replication/wal/{seq}?offset={len}")).await;
    assert!(body.is_empty());
    let (status, body) = get_raw(
        &app,
        &format!("/replication/wal/{seq}?offset={}", len + 100),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
}

#[tokio::test]
async fn missing_and_invalid_paths_are_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _db) = primary(dir.path());

    let (status, _) = get_raw(&app, "/replication/manifest/9999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_raw(&app, "/replication/wal/9999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_raw(&app, "/replication/segment/0/0/evil.bin").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "non-whitelisted file name");
    let (status, _) = get_raw(&app, "/replication/segment/0/999999/vectors.bin").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replication_requires_persistence_and_auth() {
    // in-memory DB では 400
    let app = hamane_server::router(Arc::new(Database::in_memory()));
    let (status, body) = get_json(&app, "/replication/state").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("persistent"));

    // 認証有効時は API キーが要る (認証レイヤの内側にあること)
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(dir.path()).unwrap());
    let authed = hamane_server::router_with_auth(db, Some("secret-key".into()));
    let (status, _) = get_raw(&authed, "/replication/state").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
