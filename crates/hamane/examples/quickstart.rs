//! 最小の使い方 (todo 1302)。collection 作成 → 投入 → 検索 → 更新 → 削除。
//!
//! 実行: `cargo run --example quickstart`
//!
//! hamane-db は「ベクトル版 SQLite」を目指した**組み込み型**エンジンです。
//! サーバを立てずにライブラリとして呼び、データはディレクトリ 1 つに入ります。

use hamane::{CollectionConfig, Database, Metric, Record};

fn main() -> hamane::Result<()> {
    // 一時ディレクトリに DB を作る (実際は永続化したいパスを渡す)
    let dir = tempdir("quickstart");

    // 1. DB を開く。ディレクトリが無ければ作られ、あれば復旧される
    let db = Database::open(&dir)?;

    // 2. collection を作る。dim と metric は後から変えられない
    let docs = db.create_collection(
        "docs",
        CollectionConfig {
            dim: 4,
            metric: Metric::Cosine, // L2 / Cosine / Dot
        },
    )?;

    // 3. レコードを入れる。id は u64 か文字列 (string_ids.rs を参照)
    //    メタデータは検索時のフィルタに使える (metadata_filter.rs を参照)
    docs.upsert(Record::new(1u64, vec![1.0, 0.0, 0.0, 0.0]).with_meta("lang", "ja"))?;
    docs.upsert(Record::new(2u64, vec![0.0, 1.0, 0.0, 0.0]).with_meta("lang", "en"))?;

    // まとめて入れるときは upsert_batch のほうが速い (WAL の fsync がまとまる)
    docs.upsert_batch(vec![
        Record::new(3u64, vec![0.9, 0.1, 0.0, 0.0]).with_meta("lang", "ja"),
        Record::new(4u64, vec![0.0, 0.0, 1.0, 0.0]).with_meta("lang", "ja"),
    ])?;

    println!("件数: {}", docs.len());

    // 4. 近傍検索。k は取得件数
    let hits = docs.search(&[1.0, 0.05, 0.0, 0.0]).k(3).run()?;
    println!("\n[1.0, 0.05, 0, 0] に近い順:");
    for hit in &hits {
        // score は metric に応じた値 (Cosine は類似度、L2 は距離)
        println!(
            "  id={} score={:.4} meta={:?}",
            hit.id, hit.score, hit.metadata
        );
    }

    // 5. 同じ id で upsert すると上書きされる
    docs.upsert(Record::new(1u64, vec![0.0, 0.0, 0.0, 1.0]).with_meta("lang", "ja"))?;
    let updated = docs.get(1u64).expect("id=1 exists");
    println!("\n更新後の id=1: {:?}", updated.vector);

    // 6. 削除
    docs.delete(2u64)?;
    println!("削除後の件数: {}", docs.len());

    // 7. 明示的なフラッシュ (省略可)。
    //    通常は memtable が閾値を超えると自動でセグメント化される。
    //    プロセスを落とす前でも WAL があるのでデータは失われない
    docs.flush()?;

    // 8. 開き直しても同じデータが読める
    drop(docs);
    drop(db);
    let db = Database::open(&dir)?;
    let docs = db.collection("docs")?;
    println!("再 open 後の件数: {}", docs.len());

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

/// サンプル用の一時ディレクトリ (プロセス ID 付きで衝突しないようにする)。
fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hamane-example-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}
