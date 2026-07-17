# 導入とクイックスタート

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

テストや一時的な用途では永続化なしで使えます。

```rust
let db = Database::in_memory();
// 以降は Database::open と完全に同じ API。flush/compact は no-op
```
