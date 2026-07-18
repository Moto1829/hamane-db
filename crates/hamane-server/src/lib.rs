//! hamane-db の HTTP API (todo 603)。組み込みエンジンの薄いラッパ。
//!
//! | メソッド | パス | 内容 |
//! |---|---|---|
//! | GET | /collections | collection 一覧 |
//! | PUT | /collections/{name} | 作成 (body: {"dim": N, "metric": "l2\|cosine\|dot"}) |
//! | DELETE | /collections/{name} | 削除 |
//! | GET | /collections/{name} | 情報 (件数・セグメント構成) |
//! | POST | /collections/{name}/records | upsert (単発 or 配列) |
//! | GET | /collections/{name}/records/{id} | 点参照 |
//! | DELETE | /collections/{name}/records/{id} | レコード削除 |
//! | POST | /collections/{name}/search | 検索 (vector, k, ef, filter) |
//! | POST | /admin/flush | フラッシュ |
//! | POST | /admin/compact | コンパクション |
//! | GET | /health | 死活確認 (**認証不要**。orchestrator の probe 用) |
//! | GET | /replication/state | レプリカ同期用の世代情報 (todo 902) |
//! | GET | /replication/manifest/{gen} | MANIFEST ファイル |
//! | GET | /replication/segment/{cid}/{seg}/{file} | セグメントファイル |
//! | GET | /replication/wal/{seq}?offset=N | WAL の tail 読み |
//!
//! 認証は静的 API キー (todo 705): `router_with_auth(db, Some(key))` で
//! 全エンドポイントが `Authorization: Bearer <key>` または
//! `X-Api-Key: <key>` を要求する。TLS はスコープ外 (リバースプロキシ前提)。
//! レコード・フィルタの JSON 表現は CLI (hamane-cli) と同一。

pub mod replica;

use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hamane::{
    CollectionConfig, Database, Filter, HamaneError, MetaValue, Metric, Record, RecordId,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// アプリケーション状態。Database は Send+Sync なので Arc 共有のみ。
#[derive(Clone)]
pub struct AppState {
    db: Arc<Database>,
}

/// ルーターを構築する (認証なし。ローカル開発・テスト向け)。
pub fn router(db: Arc<Database>) -> Router {
    router_with_auth(db, None)
}

/// ルーターを構築する。`api_key` が Some なら全エンドポイントで
/// `Authorization: Bearer <key>` または `X-Api-Key: <key>` を要求する。
pub fn router_with_auth(db: Arc<Database>, api_key: Option<String>) -> Router {
    let health_db = Arc::clone(&db);
    let router = Router::new()
        .route("/collections", get(list_collections))
        .route(
            "/collections/{name}",
            put(create_collection)
                .get(collection_info)
                .delete(drop_collection),
        )
        .route("/collections/{name}/records", post(upsert_records))
        .route(
            "/collections/{name}/records/{id}",
            get(get_record).delete(delete_record),
        )
        .route("/collections/{name}/search", post(search))
        .route("/admin/flush", post(flush))
        .route("/admin/compact", post(compact))
        .route("/replication/state", get(replication_state))
        .route("/replication/manifest/{gen}", get(replication_manifest))
        .route(
            "/replication/segment/{collection_id}/{seg_id}/{file}",
            get(replication_segment),
        )
        .route("/replication/wal/{seq}", get(replication_wal))
        .with_state(AppState { db });
    let router = match api_key {
        Some(key) => {
            let key = Arc::new(key);
            router.layer(middleware::from_fn(move |req, next| {
                let key = Arc::clone(&key);
                async move { require_api_key(key, req, next).await }
            }))
        }
        None => router,
    };
    // /health は認証レイヤの外 (todo 802)。API キーを持たない
    // orchestrator (Docker HEALTHCHECK / k8s probe) から叩けるようにする。
    // role / manifest_gen はレプリカ構成の監視用 (todo 904)
    router.route(
        "/health",
        get(move || {
            let db = Arc::clone(&health_db);
            async move {
                Json(json!({
                    "status": "ok",
                    "role": if db.is_replica() { "replica" } else { "primary" },
                    "manifest_gen": db.manifest_gen(),
                }))
            }
        }),
    )
}

/// 定数時間の等価比較 (タイミング攻撃対策)。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// API キー検証ミドルウェア (todo 705)。
async fn require_api_key(key: Arc<String>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| req.headers().get("x-api-key").and_then(|v| v.to_str().ok()));
    match presented {
        Some(p) if constant_time_eq(p.as_bytes(), key.as_bytes()) => next.run(req).await,
        _ => ApiError(StatusCode::UNAUTHORIZED, "unauthorized".into()).into_response(),
    }
}

/// エラー → HTTP ステータスの対応。
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<HamaneError> for ApiError {
    fn from(e: HamaneError) -> Self {
        let status = match &e {
            HamaneError::DimensionMismatch { .. }
            | HamaneError::InvalidVector(_)
            | HamaneError::InvalidConfig(_) => StatusCode::BAD_REQUEST,
            HamaneError::CollectionNotFound(_) => StatusCode::NOT_FOUND,
            // replica への書き込み (todo 903)。primary に送るべきリクエスト
            HamaneError::CollectionExists(_) | HamaneError::ReadOnlyReplica => StatusCode::CONFLICT,
            HamaneError::Corrupted(_) | HamaneError::Io(_) | HamaneError::Locked(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        ApiError(status, e.to_string())
    }
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

/// ブロッキング呼び出し (fsync 等を含む) を tokio の blocking プールへ。
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

// ---------------------------------------------------------------------------
// collection 管理
// ---------------------------------------------------------------------------

async fn list_collections(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let names = state.db.collection_names();
    Ok(Json(json!({ "collections": names })))
}

#[derive(Deserialize)]
struct CreateCollectionBody {
    dim: usize,
    #[serde(default = "default_metric")]
    metric: String,
}

fn default_metric() -> String {
    "cosine".into()
}

fn parse_metric(s: &str) -> Result<Metric, ApiError> {
    match s {
        "l2" => Ok(Metric::L2),
        "cosine" => Ok(Metric::Cosine),
        "dot" => Ok(Metric::Dot),
        other => Err(bad_request(format!("unknown metric: {other}"))),
    }
}

async fn create_collection(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<CreateCollectionBody>,
) -> Result<Json<Value>, ApiError> {
    let metric = parse_metric(&body.metric)?;
    let db = Arc::clone(&state.db);
    blocking(move || {
        db.create_collection(
            &name,
            CollectionConfig {
                dim: body.dim,
                metric,
            },
        )?;
        Ok(Json(json!({"created": name})))
    })
    .await
}

async fn collection_info(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let col = state.db.collection(&name)?;
    let config = col.config();
    let segments: Vec<Value> = col
        .segment_stats()?
        .iter()
        .map(|s| {
            json!({
                "seg_id": s.seg_id,
                "rows": s.record_count,
                "tombstones": s.tombstone_count,
                "hnsw": s.has_hnsw,
            })
        })
        .collect();
    Ok(Json(json!({
        "name": name,
        "dim": config.dim,
        "metric": format!("{:?}", config.metric).to_lowercase(),
        "len": col.len(),
        "segments": segments,
    })))
}

async fn drop_collection(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let db = Arc::clone(&state.db);
    blocking(move || {
        db.drop_collection(&name)?;
        Ok(Json(json!({"dropped": name})))
    })
    .await
}

// ---------------------------------------------------------------------------
// レコード
// ---------------------------------------------------------------------------

/// JSON 値 → RecordId (数値 or 文字列)。
fn parse_record_id(v: &Value) -> Result<RecordId, ApiError> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .map(RecordId::Num)
            .ok_or_else(|| bad_request("\"id\" must be a non-negative integer")),
        Value::String(s) => Ok(RecordId::Str(s.clone())),
        _ => Err(bad_request("\"id\" must be an integer or string")),
    }
}

/// パスパラメータ → RecordId。数値に見えれば Num、そうでなければ Str。
fn record_id_from_path(s: &str) -> RecordId {
    match s.parse::<u64>() {
        Ok(n) => RecordId::Num(n),
        Err(_) => RecordId::Str(s.to_owned()),
    }
}

fn json_to_meta(v: &Value) -> Result<MetaValue, ApiError> {
    match v {
        Value::String(s) => Ok(MetaValue::Str(s.clone())),
        Value::Bool(b) => Ok(MetaValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(MetaValue::Int(i))
            } else {
                n.as_f64()
                    .map(MetaValue::Float)
                    .ok_or_else(|| bad_request("invalid number in meta"))
            }
        }
        _ => Err(bad_request("meta values must be string/number/bool")),
    }
}

fn meta_to_json(meta: &hamane::Metadata) -> Value {
    let map: serde_json::Map<String, Value> = meta
        .iter()
        .map(|(k, v)| {
            let value = match v {
                MetaValue::Str(s) => json!(s),
                MetaValue::Int(i) => json!(i),
                MetaValue::Float(f) => json!(f),
                MetaValue::Bool(b) => json!(b),
            };
            (k.clone(), value)
        })
        .collect();
    Value::Object(map)
}

fn parse_record(v: &Value) -> Result<Record, ApiError> {
    let id = parse_record_id(
        v.get("id")
            .ok_or_else(|| bad_request("record needs \"id\""))?,
    )?;
    let vector: Vec<f32> = v
        .get("vector")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("record needs \"vector\" array"))?
        .iter()
        .map(|x| {
            x.as_f64()
                .map(|f| f as f32)
                .ok_or_else(|| bad_request("vector must be numeric"))
        })
        .collect::<Result<_, _>>()?;
    let mut record = Record::new(id, vector);
    if let Some(meta) = v.get("meta").and_then(Value::as_object) {
        for (key, value) in meta {
            record = record.with_meta(key.clone(), json_to_meta(value)?);
        }
    }
    Ok(record)
}

async fn upsert_records(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    // 単発オブジェクトと配列の両方を受ける
    let records: Vec<Record> = match &body {
        Value::Array(items) => items.iter().map(parse_record).collect::<Result<_, _>>()?,
        obj @ Value::Object(_) => vec![parse_record(obj)?],
        _ => return Err(bad_request("body must be a record or an array of records")),
    };
    let count = records.len();
    let db = Arc::clone(&state.db);
    blocking(move || {
        db.collection(&name)?.upsert_batch(records)?;
        Ok(Json(json!({"upserted": count})))
    })
    .await
}

async fn get_record(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let col = state.db.collection(&name)?;
    let rid = record_id_from_path(&id);
    match col.get(rid) {
        Some(rec) => Ok(Json(json!({
            "id": id,
            "vector": rec.vector,
            "meta": meta_to_json(&rec.metadata),
        }))),
        None => Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("record not found: {id}"),
        )),
    }
}

async fn delete_record(
    State(state): State<AppState>,
    Path((name, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let db = Arc::clone(&state.db);
    blocking(move || {
        let existed = db.collection(&name)?.delete(record_id_from_path(&id))?;
        Ok(Json(json!({"deleted": existed})))
    })
    .await
}

// ---------------------------------------------------------------------------
// 検索
// ---------------------------------------------------------------------------

/// フィルタの JSON 表現 (CLI と同一): {"eq": ["k", v]}, {"and": [...]}, ...
fn parse_filter(v: &Value) -> Result<Filter, ApiError> {
    let obj = v
        .as_object()
        .ok_or_else(|| bad_request("filter must be a JSON object"))?;
    if obj.len() != 1 {
        return Err(bad_request("filter object must have exactly one key"));
    }
    let (op, arg) = obj.iter().next().expect("len checked");

    let key_value = |arg: &Value| -> Result<(String, MetaValue), ApiError> {
        let pair = arg
            .as_array()
            .filter(|p| p.len() == 2)
            .ok_or_else(|| bad_request("expected [key, value]"))?;
        let key = pair[0]
            .as_str()
            .ok_or_else(|| bad_request("filter key must be string"))?;
        Ok((key.to_owned(), json_to_meta(&pair[1])?))
    };

    match op.as_str() {
        "eq" => key_value(arg).map(|(k, v)| Filter::eq(k, v)),
        "gt" => key_value(arg).map(|(k, v)| Filter::gt(k, v)),
        "gte" => key_value(arg).map(|(k, v)| Filter::gte(k, v)),
        "lt" => key_value(arg).map(|(k, v)| Filter::lt(k, v)),
        "lte" => key_value(arg).map(|(k, v)| Filter::lte(k, v)),
        "in" => {
            let pair = arg
                .as_array()
                .filter(|p| p.len() == 2)
                .ok_or_else(|| bad_request("expected [key, [values]]"))?;
            let key = pair[0]
                .as_str()
                .ok_or_else(|| bad_request("filter key must be string"))?;
            let values: Vec<MetaValue> = pair[1]
                .as_array()
                .ok_or_else(|| bad_request("expected value array"))?
                .iter()
                .map(json_to_meta)
                .collect::<Result<_, _>>()?;
            Ok(Filter::is_in(key, values))
        }
        "and" => {
            let filters: Vec<Filter> = arg
                .as_array()
                .ok_or_else(|| bad_request("expected filter array"))?
                .iter()
                .map(parse_filter)
                .collect::<Result<_, _>>()?;
            Ok(Filter::and(filters))
        }
        "or" => {
            let filters: Vec<Filter> = arg
                .as_array()
                .ok_or_else(|| bad_request("expected filter array"))?
                .iter()
                .map(parse_filter)
                .collect::<Result<_, _>>()?;
            Ok(Filter::or(filters))
        }
        "not" => Ok(Filter::not(parse_filter(arg)?)),
        other => Err(bad_request(format!("unknown filter op: {other}"))),
    }
}

#[derive(Deserialize)]
struct SearchBody {
    vector: Vec<f32>,
    #[serde(default = "default_k")]
    k: usize,
    ef: Option<usize>,
    filter: Option<Value>,
}

fn default_k() -> usize {
    10
}

async fn search(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<SearchBody>,
) -> Result<Json<Value>, ApiError> {
    let filter = body.filter.as_ref().map(parse_filter).transpose()?;
    let db = Arc::clone(&state.db);
    blocking(move || {
        let col = db.collection(&name)?;
        let mut builder = col.search(&body.vector).k(body.k);
        if let Some(ef) = body.ef {
            builder = builder.ef(ef);
        }
        if let Some(f) = filter {
            builder = builder.filter(f);
        }
        let hits: Vec<Value> = builder
            .run()?
            .iter()
            .map(|h| {
                json!({
                    "id": h.id,
                    "ext_id": h.ext_id(),
                    "score": h.score,
                    "meta": meta_to_json(&h.metadata),
                })
            })
            .collect();
        Ok(Json(json!({ "hits": hits })))
    })
    .await
}

// ---------------------------------------------------------------------------
// admin
// ---------------------------------------------------------------------------

async fn flush(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let db = Arc::clone(&state.db);
    blocking(move || {
        db.flush()?;
        Ok(Json(json!({"flushed": true})))
    })
    .await
}

async fn compact(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let db = Arc::clone(&state.db);
    blocking(move || {
        db.flush()?;
        db.compact()?;
        Ok(Json(json!({"compacted": true})))
    })
    .await
}

// ---------------------------------------------------------------------------
// レプリケーション (todo 902、docs/design/replication.md §2)
//
// primary 側はストレージエンジンに触れず db_dir のファイルを読むだけ。
// manifest / セグメントは不変、WAL は append-only なのでロック不要。
// 競合 (rotate / コンパクション後の削除) は 404 になり、replica が
// state からの再同期で回復する。
// ---------------------------------------------------------------------------

/// レプリケーション対象のディレクトリ (in-memory DB は対象外)。
fn replication_dir(state: &AppState) -> Result<std::path::PathBuf, ApiError> {
    state
        .db
        .path()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| bad_request("replication requires a persistent database"))
}

/// io::Error → 404 (NotFound) / 500 の対応。
fn io_api_error(e: std::io::Error) -> ApiError {
    let status = if e.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    ApiError(status, e.to_string())
}

async fn replication_state(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let dir = replication_dir(&state)?;
    blocking(move || {
        let current = std::fs::read_to_string(dir.join(hamane_storage::manifest::CURRENT_FILE))
            .map_err(io_api_error)?;
        let manifest_name = current.trim().to_string();
        let manifest_gen: u64 = manifest_name
            .strip_prefix("MANIFEST-")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("CURRENT points to invalid name: {manifest_name}"),
                )
            })?;
        // アクティブ WAL = 最大 seq。rotate 直後の一瞬は旧 WAL が残り得るが、
        // state は同期のヒントにすぎず、ずれは次のポーリングで解消する
        let wal = hamane_storage::wal::list_wal_files(&dir.join("wal"))
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let (wal_seq, wal_len) = match wal.last() {
            Some((seq, path)) => (
                Some(*seq),
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            ),
            None => (None, 0),
        };
        Ok(Json(json!({
            "manifest_gen": manifest_gen,
            "manifest_name": manifest_name,
            "wal_seq": wal_seq,
            "wal_len": wal_len,
        })))
    })
    .await
}

async fn replication_manifest(
    State(state): State<AppState>,
    Path(gen): Path<u64>,
) -> Result<Vec<u8>, ApiError> {
    let dir = replication_dir(&state)?;
    blocking(move || {
        std::fs::read(dir.join(hamane_storage::manifest::manifest_file_name(gen)))
            .map_err(io_api_error)
    })
    .await
}

async fn replication_segment(
    State(state): State<AppState>,
    Path((collection_id, seg_id, file)): Path<(u32, u64, String)>,
) -> Result<Vec<u8>, ApiError> {
    use hamane_storage::segment as seg;
    // パストラバーサル防止: セグメント構成ファイル名のみ許可
    const ALLOWED: [&str; 6] = [
        seg::FILE_VECTORS,
        seg::FILE_IDS,
        seg::FILE_META,
        seg::FILE_TOMBSTONES,
        seg::FILE_HNSW,
        seg::FILE_SQ8,
    ];
    if !ALLOWED.contains(&file.as_str()) {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("unknown segment file: {file}"),
        ));
    }
    let dir = replication_dir(&state)?;
    blocking(move || {
        let path = dir
            .join("collections")
            .join(collection_id.to_string())
            .join(seg::segment_dir_name(seg_id))
            .join(&file);
        std::fs::read(path).map_err(io_api_error)
    })
    .await
}

#[derive(Deserialize)]
struct WalQuery {
    #[serde(default)]
    offset: u64,
}

async fn replication_wal(
    State(state): State<AppState>,
    Path(seq): Path<u64>,
    axum::extract::Query(q): axum::extract::Query<WalQuery>,
) -> Result<Vec<u8>, ApiError> {
    let dir = replication_dir(&state)?;
    blocking(move || {
        use std::io::{Read, Seek, SeekFrom};
        let path = dir
            .join("wal")
            .join(hamane_storage::wal::wal_file_name(seq));
        let mut f = std::fs::File::open(path).map_err(io_api_error)?;
        // offset がファイル末尾以降なら空 (追記待ち。エラーではない)
        f.seek(SeekFrom::Start(q.offset)).map_err(io_api_error)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(io_api_error)?;
        Ok(buf)
    })
    .await
}
