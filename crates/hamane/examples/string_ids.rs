//! 文字列 ID (UUID など) の扱い (todo 1302)。
//!
//! 実行: `cargo run --example string_ids`
//!
//! 内部の行 ID は u64 ですが、`Record::new("doc-123", ..)` のように
//! 文字列を渡すと自動で内部 ID (2^63 以降) が採番され、対応は
//! `_ext_id` メタデータとして保存されます。
//! 検索結果からは `hit.ext_id()` で元の文字列が取れます。

use hamane::{CollectionConfig, Database, Metric, Record};

fn main() -> hamane::Result<()> {
    let dir = std::env::temp_dir().join(format!("hamane-example-strid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Database::open(&dir)?;
    let docs = db.create_collection(
        "docs",
        CollectionConfig {
            dim: 3,
            metric: Metric::L2,
        },
    )?;

    // 文字列 ID で投入する
    docs.upsert(Record::new(
        "550e8400-e29b-41d4-a716-446655440000",
        vec![1.0, 0.0, 0.0],
    ))?;
    docs.upsert(Record::new(
        "article/2026/09/vector-db",
        vec![0.9, 0.1, 0.0],
    ))?;
    docs.upsert(Record::new("user:alice", vec![0.0, 1.0, 0.0]))?;

    // u64 ID と混在させてもよい
    docs.upsert(Record::new(42u64, vec![0.0, 0.0, 1.0]))?;
    println!("件数: {}", docs.len());

    // 点参照は文字列でそのまま引ける
    let got = docs.get("user:alice").expect("exists");
    println!("user:alice = {:?}", got.vector);

    // 検索結果からは ext_id() で元の文字列が取れる (u64 の場合は None)
    println!("\n[1.0, 0.05, 0.0] に近い順:");
    for hit in docs.search(&[1.0, 0.05, 0.0]).k(4).run()? {
        match hit.ext_id() {
            Some(s) => println!("  {s} (内部 id={}) score={:.4}", hit.id, hit.score),
            None => println!("  u64 id={} score={:.4}", hit.id, hit.score),
        }
    }

    // 上書き・削除も文字列で行える
    docs.upsert(Record::new("user:alice", vec![0.0, 0.5, 0.5]))?;
    docs.delete("article/2026/09/vector-db")?;
    println!("\n更新・削除後の件数: {}", docs.len());

    // 削除した ID は再利用できる (新しい内部 ID が振られる)
    docs.upsert(Record::new(
        "article/2026/09/vector-db",
        vec![0.2, 0.2, 0.2],
    ))?;
    println!("再挿入後の件数: {}", docs.len());

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}
