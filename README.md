# hamane-db

Rust 製の組み込み型ベクトルデータベースエンジン。

- **永続化**: WAL + 不変セグメント + manifest によるクラッシュ一貫性
- **検索**: セグメントごとの HNSW (近似) + memtable の Flat (正確) をマージ
- **フィルタ**: メタデータ条件つき検索 (選択率に応じて pre/post filter を自動選択)
- **距離**: L2 / コサイン / 内積 (NEON / AVX2 の SIMD カーネル)

設計は [docs/DESIGN.md](docs/DESIGN.md)、実装タスクは [todos/](todos/) を参照。

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

## 開発

```sh
cargo test --workspace          # 全テスト (クラッシュ耐性・プロパティテスト含む)
cargo clippy --workspace --all-targets
cargo bench -p hamane-core      # 距離カーネルのベンチ
```
