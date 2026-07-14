# hamane-db

Rust 製の組み込み型ベクトルデータベースエンジン。

- **永続化**: WAL + 不変セグメント + manifest によるクラッシュ一貫性。
  group commit (`SyncPolicy::Batch`) 対応
- **検索**: セグメントごとの HNSW (近似) + memtable の Flat (正確) をマージ。
  セグメント間はスレッド並列。SQ8 量子化 + f32 再ランクの 2 段階検索 (opt-in)
- **書き込みが止まらない**: フラッシュ (並列 HNSW 構築) とコンパクション
  (universal 風部分マージ) はバックグラウンド実行 (フラッシュ中の upsert p99 8µs)
- **ID**: u64 と文字列 (UUID 等) の両対応
- **フィルタ**: メタデータ条件つき検索 (選択率に応じて pre/post filter を自動選択)
- **距離**: L2 / コサイン / 内積 (NEON / AVX2 の SIMD カーネル)
- **インターフェース**: Rust ライブラリ / CLI / HTTP サーバ / Python (pyo3)

- **仕様書**: [docs/spec/](docs/spec/) (mdBook。`mdbook serve docs/spec` でローカル閲覧、
  GitHub Pages で公開)
- 設計の背景: [docs/DESIGN.md](docs/DESIGN.md) / 実装タスク: [todos/](todos/)

## ライブラリとして使う

```rust
use hamane::{Database, CollectionConfig, Metric, Record, Filter};

let db = Database::open("path/to/db")?; // または Database::in_memory()
let col = db.create_collection("docs", CollectionConfig {
    dim: 768,
    metric: Metric::Cosine,
})?;

col.upsert(Record::new(1, vec![0.1; 768]).with_meta("lang", "ja"))?;

let hits = col.search(&query_vec)
    .k(10)
    .filter(Filter::eq("lang", "ja"))
    .ef(128) // HNSW の探索幅 (任意)
    .run()?;
```

## CLI

```sh
cargo install --path crates/hamane-cli

hamane create ./db docs --dim 4 --metric cosine
echo '{"id": 1, "vector": [0.1, 0.2, 0.3, 0.4], "meta": {"lang": "ja"}}' | hamane insert ./db docs
hamane search ./db docs --vector '[0.1,0.2,0.3,0.4]' --k 5 --filter '{"eq":["lang","ja"]}' --pretty
hamane info ./db
hamane flush ./db && hamane compact ./db
```

## HTTP サーバ

```sh
cargo run --release -p hamane-server -- --db ./db --listen 127.0.0.1:8080 \
    --api-key my-secret   # 省略時は認証なし (HAMANE_API_KEY でも指定可)

curl -X PUT localhost:8080/collections/docs -H 'content-type: application/json' \
     -H 'authorization: Bearer my-secret' -d '{"dim": 4, "metric": "cosine"}'
curl -X POST localhost:8080/collections/docs/records -H 'content-type: application/json' \
     -d '{"id": "doc-1", "vector": [0.1, 0.2, 0.3, 0.4], "meta": {"lang": "ja"}}'
curl -X POST localhost:8080/collections/docs/search -H 'content-type: application/json' \
     -d '{"vector": [0.1, 0.2, 0.3, 0.4], "k": 5, "filter": {"eq": ["lang", "ja"]}}'
```

## Python

```sh
cd crates/hamane-py
pip install maturin && maturin develop --release
```

```python
import hamane
db = hamane.Database("./db")
col = db.create_collection("docs", dim=768, metric="cosine")
col.upsert("doc-1", vec, meta={"lang": "ja"})
col.upsert_batch(ids, matrix)   # numpy (n, dim) 対応
hits = col.search(vec, k=10, filter={"eq": ["lang", "ja"]})
```

## 開発

```sh
cargo test --workspace          # 全テスト (クラッシュ耐性・プロパティテスト含む)
cargo clippy --workspace --all-targets
cargo bench -p hamane-core      # 距離カーネルのベンチ
```
