//! 推薦 (内積検索 + 絞り込み) (todo 1302)。
//!
//! 実行: `cargo run --release --example recommendation`
//!
//! 行列分解などで得たユーザー・アイテムの潜在ベクトルは、
//! **内積 (Metric::Dot)** が大きいほど「好み」に一致します。
//! コサインと違って大きさ (人気度・バイアス) が効くのが特徴です。
//!
//! 在庫や年齢制限のような条件はメタデータフィルタで落とします。

use hamane::{CollectionConfig, Database, Filter, MetaValue, Metric, Record};

const DIM: usize = 16;

fn main() -> hamane::Result<()> {
    let dir = std::env::temp_dir().join(format!("hamane-example-recsys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let db = Database::open(&dir)?;
    let items = db.create_collection(
        "items",
        CollectionConfig {
            dim: DIM,
            metric: Metric::Dot, // 内積 = 好みスコア
        },
    )?;

    // アイテム 5,000 点。3 つのジャンルに偏りを持たせる
    let genres = ["action", "romance", "documentary"];
    let records: Vec<Record> = (0..5_000u64)
        .map(|i| {
            let genre = genres[(i % 3) as usize];
            let popularity = 0.5 + (i % 100) as f32 / 100.0; // 人気度をベクトルの大きさに乗せる
            let v: Vec<f32> = (0..DIM)
                .map(|d| genre_axis(genre, d) * popularity + noise(i * 31 + d as u64) * 0.1)
                .collect();
            Record::new(i, v)
                .with_meta("genre", genre)
                .with_meta("in_stock", i % 7 != 0)
                .with_meta("age_limit", if i % 11 == 0 { 18i64 } else { 0 })
        })
        .collect();
    items.upsert_batch(records)?;
    items.flush()?;
    println!("{} 点のアイテムを投入しました\n", items.len());

    // アクション寄りのユーザーベクトル (実際は行列分解や学習で得る)
    let user: Vec<f32> = (0..DIM).map(|d| genre_axis("action", d)).collect();

    println!("--- そのまま推薦 (内積が大きい順) ---");
    for hit in items.search(&user).k(5).run()? {
        println!(
            "  id={} score={:.3} genre={:?} 在庫={:?}",
            hit.id,
            hit.score,
            hit.metadata.get("genre"),
            hit.metadata.get("in_stock")
        );
    }

    println!("\n--- 在庫あり かつ 年齢制限なし に絞る ---");
    let filter = Filter::and(vec![
        Filter::eq("in_stock", MetaValue::Bool(true)),
        Filter::eq("age_limit", MetaValue::Int(0)),
    ]);
    for hit in items.search(&user).k(5).filter(filter).run()? {
        println!(
            "  id={} score={:.3} genre={:?}",
            hit.id,
            hit.score,
            hit.metadata.get("genre")
        );
    }

    // 「このアイテムに似ている」= アイテムベクトルをクエリにする (item-to-item)
    let seed = items.get(10u64).expect("item 10 exists");
    println!("\n--- id=10 に似ているアイテム ---");
    for hit in items.search(&seed.vector).k(5).run()? {
        if hit.id == 10 {
            continue; // 自分自身は除く
        }
        println!(
            "  id={} score={:.3} genre={:?}",
            hit.id,
            hit.score,
            hit.metadata.get("genre")
        );
    }

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

/// ジャンルごとに異なる軸を強く持たせる (潜在因子のつもり)。
fn genre_axis(genre: &str, d: usize) -> f32 {
    let base = match genre {
        "action" => 0,
        "romance" => 5,
        _ => 10,
    };
    if (base..base + 5).contains(&d) {
        1.0
    } else {
        0.0
    }
}

fn noise(seed: u64) -> f32 {
    let x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((x >> 33) as f32) / (u32::MAX as f32 / 2.0) - 1.0
}
