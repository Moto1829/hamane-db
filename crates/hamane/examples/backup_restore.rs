//! バックアップと復元 (todo 1302)。
//!
//! 実行: `cargo run --example backup_restore`
//!
//! `Database::backup(dest)` は**一貫性のあるスナップショット**を作ります。
//! 内部で flush してから状態ロックの中でファイルをコピーするので、
//! 書き込み中に取っても半端な状態にはなりません。
//! 復元は「コピーしたディレクトリを open するだけ」です。

use hamane::{CollectionConfig, Database, Metric, Record};

fn main() -> hamane::Result<()> {
    let base = std::env::temp_dir().join(format!("hamane-example-backup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let live = base.join("live");
    let backup = base.join("backup");

    let db = Database::open(&live)?;
    let col = db.create_collection(
        "docs",
        CollectionConfig {
            dim: 4,
            metric: Metric::L2,
        },
    )?;
    for i in 0..1_000u64 {
        col.upsert(Record::new(i, vec![i as f32, 0.0, 0.0, 0.0]).with_meta("batch", "first"))?;
    }

    // バックアップ先は空ディレクトリ (存在しなければ作られる)
    db.backup(&backup)?;
    println!("バックアップを取りました: {}", backup.display());

    // バックアップ後に元 DB を変更する
    for i in 1_000..1_500u64 {
        col.upsert(Record::new(i, vec![i as f32, 0.0, 0.0, 0.0]).with_meta("batch", "second"))?;
    }
    col.delete(0u64)?;
    println!("元 DB を更新: 件数 {}", col.len());

    // 復元 = コピーを open するだけ。バックアップ時点の状態が見える
    let restored = Database::open(&backup)?;
    let rcol = restored.collection("docs")?;
    println!("バックアップの件数: {} (取得時点の 1000 件)", rcol.len());
    println!(
        "バックアップに id=0 は残っている: {}",
        rcol.get(0u64).is_some()
    );
    println!(
        "バックアップに id=1000 は無い: {}",
        rcol.get(1000u64).is_none()
    );

    let hits = rcol.search(&[500.0, 0.0, 0.0, 0.0]).k(3).run()?;
    println!(
        "バックアップ側で検索: id={} score={:.3}",
        hits[0].id, hits[0].score
    );

    std::fs::remove_dir_all(&base).ok();
    Ok(())
}
