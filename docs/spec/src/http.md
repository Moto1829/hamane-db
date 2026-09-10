# HTTP API リファレンス

`hamane-server` は組み込みエンジンの薄いラッパーです。
リクエスト・レスポンスの JSON 表現は [CLI](cli.md) と同じです。

```bash
# 起動 (Rust ツールチェーンがある場合)
cargo run --release -p hamane-server -- --db ./mydb --listen 127.0.0.1:8080

# Docker
docker run -p 8080:8080 -v hamane-data:/data -e HAMANE_API_KEY=secret ghcr.io/moto1829/hamane-db
```

## 認証

`--api-key` または環境変数 `HAMANE_API_KEY` を指定すると、**`/health` 以外の
全エンドポイント**が認証を要求します。未指定なら認証なし (起動時に警告)。

```bash
curl -H 'Authorization: Bearer secret' http://127.0.0.1:8080/collections
curl -H 'X-Api-Key: secret'            http://127.0.0.1:8080/collections
```

TLS はスコープ外です (リバースプロキシの前提)。

## エンドポイント一覧

| メソッド | パス | 内容 |
|---|---|---|
| GET | `/health` | 死活確認 (**認証不要**) |
| GET | `/collections` | collection 一覧 |
| PUT | `/collections/{name}` | 作成 |
| GET | `/collections/{name}` | 情報 (件数・セグメント構成) |
| DELETE | `/collections/{name}` | 削除 |
| POST | `/collections/{name}/records` | upsert (単体または配列) |
| GET | `/collections/{name}/records/{id}` | 点参照 |
| DELETE | `/collections/{name}/records/{id}` | 削除 |
| POST | `/collections/{name}/search` | 近傍検索 |
| POST | `/admin/flush` | フラッシュ |
| POST | `/admin/compact` | コンパクション |
| GET | `/replication/*` | レプリカ同期用 ([レプリケーション](replication.md)) |

## 使い方 (curl)

以下は `KEY=secret` を設定済みとします。

```bash
AUTH="Authorization: Bearer $KEY"
JSON="Content-Type: application/json"
```

### 死活確認

```bash
curl -s http://127.0.0.1:8080/health
# {"status":"ok","role":"primary","manifest_gen":3}
```

`role` は `primary` / `replica`。`manifest_gen` はレプリカの追従確認に使えます。

### collection の作成・一覧・情報・削除

```bash
curl -s -X PUT -H "$AUTH" -H "$JSON" \
  -d '{"dim":4,"metric":"l2"}' http://127.0.0.1:8080/collections/docs
# {"created":"docs"}

curl -s -H "$AUTH" http://127.0.0.1:8080/collections
# {"collections":["docs"]}

curl -s -H "$AUTH" http://127.0.0.1:8080/collections/docs
# {"name":"docs","dim":4,"metric":"l2","len":2,
#  "segments":[{"seg_id":0,"rows":2,"tombstones":0,"hnsw":false}]}

curl -s -X DELETE -H "$AUTH" http://127.0.0.1:8080/collections/docs
```

`metric` は `l2` / `cosine` / `dot` (既定 `cosine`)。dim と metric は後から変えられません。

### レコードの投入

単体でも配列でも同じエンドポイントです。**大量に入れるときは配列**にすると
WAL の fsync がまとまって速くなります。

```bash
# 単体
curl -s -X POST -H "$AUTH" -H "$JSON" \
  -d '{"id":1,"vector":[1,0,0,0],"meta":{"lang":"ja","year":2026}}' \
  http://127.0.0.1:8080/collections/docs/records
# {"upserted":1}

# 配列 (文字列 ID も混ぜられる)
curl -s -X POST -H "$AUTH" -H "$JSON" \
  -d '[{"id":2,"vector":[0,1,0,0]},
       {"id":"doc-abc","vector":[0,0,1,0],"meta":{"lang":"en"}}]' \
  http://127.0.0.1:8080/collections/docs/records
# {"upserted":2}
```

JSONL のファイルから流し込む例:

```bash
jq -s -c . records.jsonl | curl -s -X POST -H "$AUTH" -H "$JSON" \
  --data-binary @- http://127.0.0.1:8080/collections/docs/records
```

### 点参照と削除

```bash
curl -s -H "$AUTH" http://127.0.0.1:8080/collections/docs/records/1
# {"id":"1","vector":[1.0,0.0,0.0,0.0],"meta":{"lang":"ja","year":2026}}

curl -s -H "$AUTH" http://127.0.0.1:8080/collections/docs/records/doc-abc

curl -s -X DELETE -H "$AUTH" http://127.0.0.1:8080/collections/docs/records/1
# {"deleted":true}
```

### 検索

```bash
curl -s -X POST -H "$AUTH" -H "$JSON" \
  -d '{"vector":[1,0,0,0],"k":5}' \
  http://127.0.0.1:8080/collections/docs/search
# {"hits":[{"id":1,"ext_id":null,"score":0.0,"meta":{...}}, ...]}
```

| フィールド | 既定 | 意味 |
|---|---|---|
| `vector` | 必須 | クエリベクトル (collection の dim と一致) |
| `k` | 10 | 取得件数 |
| `ef` | サーバ設定 | HNSW の探索幅。大きいほど高精度・低速 |
| `nprobe` | サーバ設定 | IVF / IVF-PQ で走査するクラスタ数 |
| `filter` | なし | メタデータ条件 (下記) |

`score` は metric に応じた値です (L2 は距離、Cosine は類似度、Dot は内積)。
量子化を有効にしていても **score は常に正確な f32 距離**です
([検索](search.md#量子化による高速化))。文字列 ID のレコードは `ext_id` に
元の文字列が入ります。

### フィルタ

CLI と同じ JSON 表現です。

```bash
# 等値
curl -s -X POST -H "$AUTH" -H "$JSON" \
  -d '{"vector":[1,0,0,0],"k":5,"filter":{"eq":["lang","ja"]}}' \
  http://127.0.0.1:8080/collections/docs/search

# AND / OR / NOT の組み合わせ
curl -s -X POST -H "$AUTH" -H "$JSON" -d '{
  "vector":[1,0,0,0], "k":5,
  "filter":{"and":[
    {"eq":["lang","ja"]},
    {"not":{"eq":["draft",true]}},
    {"or":[{"eq":["year",2025]},{"eq":["year",2026]}]}
  ]}
}' http://127.0.0.1:8080/collections/docs/search
```

### 管理操作

```bash
curl -s -X POST -H "$AUTH" http://127.0.0.1:8080/admin/flush
# {"flushed":true}
curl -s -X POST -H "$AUTH" http://127.0.0.1:8080/admin/compact
# {"compacted":true}
```

どちらも省略可能です (フラッシュは閾値で、コンパクションはセグメント数で
自動的に走ります)。バックアップ前に明示的に呼ぶといった使い方をします。

## エラー

エラーは `{"error":"..."}` の形で返ります。ステータスの対応は
[エラーリファレンス](errors.md) を参照してください。よく出るもの:

| ステータス | 例 |
|---|---|
| 400 | 次元不一致、NaN を含むベクトル、不正なフィルタ |
| 401 | API キーが無い・違う |
| 404 | collection / レコードが無い |
| 409 | collection の重複作成、**レプリカへの書き込み** |

## Rust から呼ぶ

`crates/hamane-server/examples/http_client.rs` に一通りの例があります。

```bash
cargo run --release -p hamane-server --example http_client -- http://127.0.0.1:8080 secret
```

Python なら `requests` で同じことができます。
埋め込みエンジンとして直接使う場合は [Python バインディング](getting-started.md)
の方が速い (プロセス間通信が無い) ので、用途に応じて選んでください。
