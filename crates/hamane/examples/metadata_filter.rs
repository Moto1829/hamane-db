//! メタデータ条件つき検索 (todo 1302)。
//!
//! 実行: `cargo run --release --example metadata_filter`
//!
//! フィルタは検索の**実行計画**に影響します。hamane-db は条件の選択率を
//! サンプリングして、
//!
//! - 選択率が低い (絞り込みが強い) → **pre-filter**: 先に条件で絞ってから
//!   総当たりで距離計算する
//! - 選択率が高い → **post-filter**: HNSW で辿りながら条件で落とす
//!
//! を自動で選びます。利用者は同じ API を呼ぶだけです。

use hamane::{CollectionConfig, Database, Filter, MetaValue, Metric, Record};

const DIM: usize = 8;
const N: u64 = 20_000;

fn main() -> hamane::Result<()> {
    let dir = std::env::temp_dir().join(format!("hamane-example-filter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db = Database::open(&dir)?;
    let docs = db.create_collection(
        "docs",
        CollectionConfig {
            dim: DIM,
            metric: Metric::L2,
        },
    )?;

    // lang: 1/2 が "ja"、category: 1/50 が "rare"、year: 2020〜2024
    let records: Vec<Record> = (0..N)
        .map(|i| {
            let v: Vec<f32> = (0..DIM).map(|d| pseudo_random(i * 31 + d as u64)).collect();
            Record::new(i, v)
                .with_meta("lang", if i % 2 == 0 { "ja" } else { "en" })
                .with_meta("category", if i % 50 == 0 { "rare" } else { "common" })
                .with_meta("year", 2020 + (i % 5) as i64)
                .with_meta("public", i % 3 != 0)
        })
        .collect();
    docs.upsert_batch(records)?;
    docs.flush()?;
    println!("{N} 件を投入しました\n");

    let query: Vec<f32> = (0..DIM).map(|d| pseudo_random(7 * 31 + d as u64)).collect();

    // 1. フィルタなし
    show("フィルタなし", &docs, &query, None)?;

    // 2. 単純な等値 (選択率 50% → post-filter が選ばれる)
    show(
        "lang = ja (選択率 50%)",
        &docs,
        &query,
        Some(Filter::eq("lang", "ja")),
    )?;

    // 3. 強い絞り込み (選択率 2% → pre-filter が選ばれる)
    show(
        "category = rare (選択率 2%)",
        &docs,
        &query,
        Some(Filter::eq("category", "rare")),
    )?;

    // 4. AND / OR / NOT の組み合わせ
    let complex = Filter::and(vec![
        Filter::eq("lang", "ja"),
        Filter::eq("public", MetaValue::Bool(true)),
        Filter::or(vec![
            Filter::eq("year", MetaValue::Int(2023)),
            Filter::eq("year", MetaValue::Int(2024)),
        ]),
    ]);
    show(
        "ja かつ public かつ (2023 or 2024)",
        &docs,
        &query,
        Some(complex),
    )?;

    // 5. 一致が 0 件でもエラーにはならず、空の結果が返る
    show(
        "存在しない条件",
        &docs,
        &query,
        Some(Filter::eq("lang", "xx")),
    )?;

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

fn show(
    label: &str,
    docs: &hamane::Collection,
    query: &[f32],
    filter: Option<Filter>,
) -> hamane::Result<()> {
    let started = std::time::Instant::now();
    let mut builder = docs.search(query).k(5);
    if let Some(f) = filter {
        builder = builder.filter(f);
    }
    let hits = builder.run()?;
    println!("{label}: {} 件 ({:.1?})", hits.len(), started.elapsed());
    for hit in hits.iter().take(3) {
        println!(
            "  id={} score={:.4} lang={:?} year={:?}",
            hit.id,
            hit.score,
            hit.metadata.get("lang"),
            hit.metadata.get("year")
        );
    }
    println!();
    Ok(())
}

/// 依存を増やさない決定的な疑似乱数 (0.0〜1.0)。
fn pseudo_random(seed: u64) -> f32 {
    let x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((x >> 33) as f32) / (u32::MAX as f32 / 2.0) - 1.0
}
