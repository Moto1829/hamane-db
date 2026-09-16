# 導入とクイックスタート

> このページから「ベクトル」「次元」「metric」といった言葉が出てきます。
> 意味がぴんと来ないときは [用語集](glossary.md) を開いてください。

## インストール

Cargo プロジェクトに追加します (現在は git 依存)。

```toml
[dependencies]
hamane = { git = "https://github.com/Moto1829/hamane-db" }
```

CLI を使う場合:

```sh
cargo install --git https://github.com/Moto1829/hamane-db hamane-cli
```

HTTP サーバを Docker で立てる場合 (Rust ツールチェーン不要):

```sh
docker run -p 8080:8080 -v hamane-data:/data -e HAMANE_API_KEY=my-secret \
    ghcr.io/moto1829/hamane-db:latest
```

データは `/data` ボリュームに永続化され、`docker stop` (SIGTERM) で flush
してから終了します。`GET /health` は認証不要の死活確認エンドポイントです。

## ベクトルはどこから来るのか

**hamane-db はベクトル化 (埋め込み) をしません。** 文章や画像を数値の並びへ
変換するのは埋め込みモデルの仕事で、hamane-db が担当するのは
「**出来上がったベクトルを保存して、近いものを高速に探す**」ところだけです。

```text
  テキスト / 画像
        │
        ▼  ← ここは外部 (埋め込みモデル)
  [0.12, -0.03, 0.51, ...]        ← float32 の並び。長さ = モデルの出力次元
        │
        ▼  ← ここから hamane-db
  Record::new(id, vector) で投入 → 近傍検索
```

SQLite が文字列の意味を解釈しないのと同じ立場です。モデルは用途・言語・
コストで選ぶものなので、DB 側が特定のモデルを抱え込まない設計にしています。

### ベクトルの用意のしかた

| 方法 | 例 | 次元 |
|---|---|---|
| API を呼ぶ | OpenAI `text-embedding-3-small`、Cohere | 1536 など |
| ローカルモデル | Rust: fastembed / candle、Python: sentence-transformers | 384 / 768 など |
| 自前の特徴量 | 行列分解で得た潜在ベクトル (推薦) | 任意 |

Python (ローカルモデル) の例:

```python
from sentence_transformers import SentenceTransformer
import hamane

model = SentenceTransformer("intfloat/multilingual-e5-small")  # 384 次元
db = hamane.Database("./mydb")
col = db.create_collection("docs", dim=384, metric="cosine")

texts = ["犬が好きです", "猫を飼っています", "決算報告書の作成"]
vectors = model.encode(texts, normalize_embeddings=True).astype("float32")
col.upsert_batch(list(range(len(texts))), vectors)   # (n, dim) の numpy 行列をそのまま渡せる
db.flush()

hits = col.search(model.encode("子犬について").astype("float32"), k=3)
```

Rust (API を呼ぶ場合) は、モデルの応答から `Vec<f32>` を取り出して渡すだけです:

```rust,ignore
let vector: Vec<f32> = embed_with_your_model(text)?;   // 外部 API / fastembed など
col.upsert(Record::new(doc_id, vector).with_meta("path", path))?;
```

### 決めておくこと

- **`dim` はモデルの出力次元と一致させます。後から変えられません。**
  モデルを差し替えるときは新しい collection を作り直し、
  [`swap_collections`](data-model.md#改名と無停止の差し替え) で切り替えます
- **metric は埋め込みなら `Cosine` が定番**です (長さの違いを無視して向きだけ比べる)。
  推薦などで「人気度」も効かせたいときは `Dot`、座標のような値なら `L2`
- 同じ collection に**別のモデルの出力を混ぜない**でください。距離が意味を持ちません

### 動くサンプル

`crates/hamane/examples/rag_pipeline.rs` に、チャンク分割 → 埋め込み → 投入 →
検索 → 出典つきプロンプト組み立て、までの一連があります。

ただしその `embed()` は**外部依存を増やさないための疑似実装** (文字 2-gram の
ハッシュ) で、**意味は捉えません**。実運用では上の例のようにモデルの出力へ
差し替えてください。他のコードはそのまま動きます。

## 最小の例

```rust
use hamane::{Database, CollectionConfig, Metric, Record, Filter};

fn main() -> hamane::Result<()> {
    // ディレクトリを開く (なければ初期化)。in-memory なら Database::in_memory()
    let db = Database::open("./mydb")?;

    let col = db.create_collection("docs", CollectionConfig {
        dim: 4,
        metric: Metric::Cosine,
    })?;

    // 挿入 (upsert: 同じ id は置き換え)
    col.upsert(Record::new(1, vec![0.1, 0.2, 0.3, 0.4]).with_meta("lang", "ja"))?;
    col.upsert(Record::new(2, vec![0.4, 0.3, 0.2, 0.1]).with_meta("lang", "en"))?;

    // 検索
    let hits = col.search(&[0.1, 0.2, 0.3, 0.4])
        .k(5)
        .filter(Filter::eq("lang", "ja"))
        .run()?;
    for h in &hits {
        println!("id={} score={:.3}", h.id, h.score);
    }

    // 削除
    col.delete(2)?;
    Ok(())
}
```

`Ok` が返った書き込みは、この時点でクラッシュしても失われません
(既定の [`SyncPolicy::Always`](configuration.md) の場合。
詳細は [永続化と耐久性](persistence.md))。

## 大量データの投入

1 件ずつの `upsert` は書き込みごとに fsync するため低速です。
バッチ API を使うと WAL の同期が 1 回にまとまります。

```rust
let records: Vec<Record> = build_records();
col.upsert_batch(records)?;   // fsync は 1 回

// 任意: すぐにセグメント化して HNSW を構築したい場合
db.flush()?;
```

`flush()` を呼ばなくても、memtable が閾値 (既定 64 MiB) を超えると
自動でフラッシュされます。

## 同じ API での in-memory 利用

テストや一時的な用途では、永続化なしの `Database::in_memory()` が使えます。

```rust
let db = Database::in_memory();
// 以降は Database::open と同じ API。flush/compact は no-op
```

ただし**同じなのは API だけで、検索の性能特性は別物**です。in-memory では
セグメントが作られないため HNSW も量子化も構築されず、検索は memtable の
総当たり (Flat) 走査になります。数万件を超える規模で使う前に
[in-memory モードの制約](limits.md#in-memory-モードの制約) を確認してください。

永続化は不要でも ANN の性能が必要な場合は、tmpfs 上のディレクトリを
`Database::open` で開くのが現状の回避策です。
