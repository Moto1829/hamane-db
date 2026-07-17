//! HTTP API の結合テスト (todo 603)。Router を直接 oneshot で叩く。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use hamane::Database;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::util::ServiceExt;

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

fn test_app() -> Router {
    hamane_server::router(Arc::new(Database::in_memory()))
}

#[tokio::test]
async fn collection_lifecycle_and_crud() {
    let app = test_app();

    // 作成
    let (status, _) = request(
        &app,
        "PUT",
        "/collections/docs",
        Some(json!({"dim": 3, "metric": "l2"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 重複作成は 409
    let (status, _) = request(&app, "PUT", "/collections/docs", Some(json!({"dim": 3}))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // 一覧
    let (status, body) = request(&app, "GET", "/collections", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["collections"], json!(["docs"]));

    // upsert (配列 + 文字列 ID 混在)
    let (status, body) = request(
        &app,
        "POST",
        "/collections/docs/records",
        Some(json!([
            {"id": 1, "vector": [1.0, 0.0, 0.0], "meta": {"lang": "ja"}},
            {"id": "doc-x", "vector": [0.0, 1.0, 0.0]},
        ])),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["upserted"], 2);

    // info
    let (status, body) = request(&app, "GET", "/collections/docs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["len"], 2);
    assert_eq!(body["dim"], 3);

    // 点参照 (数値 / 文字列)
    let (status, body) = request(&app, "GET", "/collections/docs/records/1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["lang"], "ja");
    let (status, _) = request(&app, "GET", "/collections/docs/records/doc-x", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&app, "GET", "/collections/docs/records/999", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 検索 (フィルタつき)
    let (status, body) = request(
        &app,
        "POST",
        "/collections/docs/search",
        Some(json!({
            "vector": [0.9, 0.1, 0.0],
            "k": 5,
            "filter": {"eq": ["lang", "ja"]},
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["id"], 1);

    // 文字列 ID が検索結果に出る
    let (_, body) = request(
        &app,
        "POST",
        "/collections/docs/search",
        Some(json!({"vector": [0.0, 1.0, 0.0], "k": 1})),
    )
    .await;
    assert_eq!(body["hits"][0]["ext_id"], "doc-x");

    // 削除
    let (status, body) = request(&app, "DELETE", "/collections/docs/records/1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], true);

    // collection 削除
    let (status, _) = request(&app, "DELETE", "/collections/docs", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&app, "GET", "/collections/docs", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// API キー認証 (todo 705)。
#[tokio::test]
async fn api_key_authentication() {
    let app =
        hamane_server::router_with_auth(Arc::new(Database::in_memory()), Some("secret-key".into()));

    // キーなし → 401
    let (status, body) = request(&app, "GET", "/collections", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");

    // 間違ったキー → 401
    let req = Request::builder()
        .method("GET")
        .uri("/collections")
        .header("authorization", "Bearer wrong-key")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Bearer で正しいキー → 200
    let req = Request::builder()
        .method("GET")
        .uri("/collections")
        .header("authorization", "Bearer secret-key")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // X-Api-Key でも 200
    let req = Request::builder()
        .method("GET")
        .uri("/collections")
        .header("x-api-key", "secret-key")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // キー未設定のルーターは素通し (既存テスト全体がこの経路)
    let open_app = test_app();
    let (status, _) = request(&open_app, "GET", "/collections", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn error_status_codes() {
    let app = test_app();

    // 存在しない collection
    let (status, _) = request(
        &app,
        "POST",
        "/collections/nope/search",
        Some(json!({"vector": [1.0]})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 不正な設定
    let (status, _) = request(&app, "PUT", "/collections/bad", Some(json!({"dim": 0}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 次元不一致
    request(&app, "PUT", "/collections/docs", Some(json!({"dim": 3}))).await;
    let (status, body) = request(
        &app,
        "POST",
        "/collections/docs/records",
        Some(json!({"id": 1, "vector": [1.0]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("dimension"));

    // 不正フィルタ
    let (status, _) = request(
        &app,
        "POST",
        "/collections/docs/search",
        Some(json!({"vector": [1.0, 0.0, 0.0], "filter": {"bogus": []}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_flush_and_compact_with_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::open(dir.path()).unwrap());
    let app = hamane_server::router(Arc::clone(&db));

    request(
        &app,
        "PUT",
        "/collections/docs",
        Some(json!({"dim": 2, "metric": "l2"})),
    )
    .await;
    let records: Vec<Value> = (0..50)
        .map(|i| json!({"id": i, "vector": [i as f32, 0.0]}))
        .collect();
    request(
        &app,
        "POST",
        "/collections/docs/records",
        Some(json!(records)),
    )
    .await;

    let (status, _) = request(&app, "POST", "/admin/flush", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&app, "POST", "/admin/compact", None).await;
    assert_eq!(status, StatusCode::OK);

    // フラッシュ後もサーバ経由の検索が正しい
    let (status, body) = request(
        &app,
        "POST",
        "/collections/docs/search",
        Some(json!({"vector": [0.0, 0.0], "k": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<u64> = body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, vec![0, 1, 2]);
}

/// todo 802: /health は認証の有無にかかわらずキーなしで 200 を返す
/// (Docker HEALTHCHECK / k8s probe は API キーを持たないため)。
#[tokio::test]
async fn health_endpoint_bypasses_auth() {
    let (status, body) = request(&test_app(), "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");

    let authed =
        hamane_server::router_with_auth(Arc::new(Database::in_memory()), Some("secret-key".into()));
    let (status, body) = request(&authed, "GET", "/health", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "health must not require the API key"
    );
    assert_eq!(body["status"], "ok");
}
