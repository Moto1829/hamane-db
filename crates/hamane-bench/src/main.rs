//! SIFT1M ベンチハーネス (todos/307)。
//!
//! ```text
//! ./scripts/download_sift1m.sh
//! cargo run --release -p hamane-bench -- --data data/sift
//! cargo run --release -p hamane-bench -- --data data/sift --limit 100000  # サブセット
//! ```
//!
//! 計測: 挿入時間 / フラッシュ (HNSW 構築) 時間 / ディスクサイズ /
//! ef ごとの recall@10 と QPS。結果は docs/benchmarks.md に転記する。

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::Parser;
use hamane::{CollectionConfig, Database, Metric, Record, StoreOptions, SyncPolicy};

#[derive(Parser)]
#[command(name = "hamane-bench", about = "SIFT1M benchmark for hamane-db")]
struct Args {
    /// sift_base.fvecs / sift_query.fvecs / sift_groundtruth.ivecs のあるディレクトリ
    #[arg(long, default_value = "data/sift")]
    data: PathBuf,
    /// DB を作る場所 (省略時は一時ディレクトリ)
    #[arg(long)]
    db: Option<PathBuf>,
    /// ベースベクトル数の上限 (フルは 1,000,000。減らすと正解を総当たりで計算)
    #[arg(long, default_value_t = 1_000_000)]
    limit: usize,
    /// クエリ数の上限
    #[arg(long, default_value_t = 10_000)]
    queries: usize,
    #[arg(long, default_value_t = 10)]
    k: usize,
    /// ef_search のスイープ (カンマ区切り)
    #[arg(long, default_value = "16,32,64,128,256")]
    ef: String,
    /// フラッシュ閾値 (バイト)。既定はフラッシュ 1 回になる十分大きな値
    #[arg(long, default_value_t = usize::MAX)]
    flush_threshold: usize,
    /// HNSW 構築の extendCandidates を無効化する (todo 502 の比較用)
    #[arg(long)]
    no_extend: bool,
    /// HNSW の ef_construction (既定 200)
    #[arg(long, default_value_t = 200)]
    ef_construction: usize,
    /// HNSW 構築スレッド数 (0 = 自動)
    #[arg(long, default_value_t = 0)]
    build_threads: usize,
}

/// .fvecs: 各ベクトルが「次元数 d (i32 LE) + f32×d」の繰り返し。
fn read_fvecs(path: &Path, limit: usize) -> std::io::Result<Vec<Vec<f32>>> {
    let buf = std::fs::read(path)?;
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= buf.len() && out.len() < limit {
        let d = i32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let end = pos + d * 4;
        assert!(end <= buf.len(), "truncated fvecs file");
        let v: Vec<f32> = buf[pos..end]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        pos = end;
        out.push(v);
    }
    Ok(out)
}

/// .ivecs: fvecs と同形式で要素が i32。
fn read_ivecs(path: &Path, limit: usize) -> std::io::Result<Vec<Vec<u32>>> {
    let buf = std::fs::read(path)?;
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= buf.len() && out.len() < limit {
        let d = i32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let end = pos + d * 4;
        assert!(end <= buf.len(), "truncated ivecs file");
        let v: Vec<u32> = buf[pos..end]
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes(c.try_into().unwrap()) as u32)
            .collect();
        pos = end;
        out.push(v);
    }
    Ok(out)
}

/// 総当たりで正解 top-k を計算する (サブセット実行時)。
fn brute_force_truth(base: &[Vec<f32>], queries: &[Vec<f32>], k: usize) -> Vec<Vec<u32>> {
    let started = Instant::now();
    let truth: Vec<Vec<u32>> = queries
        .iter()
        .map(|q| {
            let mut all: Vec<(f32, u32)> = base
                .iter()
                .enumerate()
                .map(|(i, v)| (Metric::L2.distance_key(q, v), i as u32))
                .collect();
            all.sort_by(|a, b| a.partial_cmp(b).unwrap());
            all.into_iter().take(k).map(|(_, i)| i).collect()
        })
        .collect();
    eprintln!(
        "brute-force ground truth: {:.1}s",
        started.elapsed().as_secs_f64()
    );
    truth
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(m) = entry.metadata() {
                total += m.len();
            }
        }
    }
    total
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    eprintln!("loading dataset from {} ...", args.data.display());
    let base = read_fvecs(&args.data.join("sift_base.fvecs"), args.limit)?;
    let queries = read_fvecs(&args.data.join("sift_query.fvecs"), args.queries)?;
    let dim = base.first().map(|v| v.len()).unwrap_or(0);
    eprintln!(
        "base: {} vectors, dim {}, queries: {}",
        base.len(),
        dim,
        queries.len()
    );

    // 正解: フルデータなら配布の ground truth、サブセットなら総当たり
    let truth: Vec<Vec<u32>> = if base.len() == 1_000_000 {
        read_ivecs(&args.data.join("sift_groundtruth.ivecs"), queries.len())?
            .into_iter()
            .map(|v| v.into_iter().take(args.k).collect())
            .collect()
    } else {
        brute_force_truth(&base, &queries, args.k)
    };

    // DB 構築
    let tmp;
    let db_dir = match &args.db {
        Some(p) => p.clone(),
        None => {
            tmp = std::env::temp_dir().join(format!("hamane-bench-{}", std::process::id()));
            tmp.clone()
        }
    };
    if db_dir.exists() {
        std::fs::remove_dir_all(&db_dir)?;
    }
    let db = Database::open_with_options(
        &db_dir,
        StoreOptions {
            sync: SyncPolicy::EveryN(u32::MAX),
            flush_threshold_bytes: args.flush_threshold,
            hnsw: hamane::HnswParams {
                extend_candidates: !args.no_extend,
                ef_construction: args.ef_construction,
                build_threads: args.build_threads,
                ..Default::default()
            },
            ..Default::default()
        },
    )?;
    eprintln!(
        "hnsw: extend_candidates={}, ef_construction={}, build_threads={}",
        !args.no_extend, args.ef_construction, args.build_threads
    );
    let col = db.create_collection(
        "sift",
        CollectionConfig {
            dim,
            metric: Metric::L2,
        },
    )?;

    let started = Instant::now();
    for (chunk_no, chunk) in base.chunks(10_000).enumerate() {
        let records: Vec<Record> = chunk
            .iter()
            .enumerate()
            .map(|(i, v)| Record::new((chunk_no * 10_000 + i) as u64, v.clone()))
            .collect();
        col.upsert_batch(records)?;
    }
    let insert_secs = started.elapsed().as_secs_f64();
    eprintln!("insert: {insert_secs:.1}s");

    let started = Instant::now();
    db.flush()?;
    let flush_secs = started.elapsed().as_secs_f64();
    eprintln!("flush (HNSW build): {flush_secs:.1}s");
    let disk_mb = dir_size(&db_dir) as f64 / (1024.0 * 1024.0);

    // ウォームアップ (mmap のページイン等を計測から外す)
    for q in queries.iter().take(200) {
        col.search(q).k(args.k).ef(64).run()?;
    }

    // ef スイープ
    println!(
        "\n## SIFT ベンチ結果 (n={}, queries={}, k={})\n",
        base.len(),
        queries.len(),
        args.k
    );
    println!(
        "- 挿入: {insert_secs:.1}s ({:.0} rec/s) / フラッシュ+HNSW 構築: {flush_secs:.1}s / ディスク: {disk_mb:.0} MB\n",
        base.len() as f64 / insert_secs
    );
    println!("| ef | recall@{} | QPS (1 thread) | mean latency |", args.k);
    println!("|---|---|---|---|");
    for ef in args.ef.split(',') {
        let ef: usize = ef.trim().parse()?;
        let started = Instant::now();
        let mut hit = 0usize;
        for (q, t) in queries.iter().zip(&truth) {
            let hits = col.search(q).k(args.k).ef(ef).run()?;
            hit += hits.iter().filter(|h| t.contains(&(h.id as u32))).count();
        }
        let elapsed = started.elapsed().as_secs_f64();
        let recall = hit as f64 / (queries.len() * args.k) as f64;
        let qps = queries.len() as f64 / elapsed;
        println!(
            "| {ef} | {recall:.4} | {qps:.0} | {:.2} ms |",
            elapsed * 1000.0 / queries.len() as f64
        );
    }

    if args.db.is_none() {
        std::fs::remove_dir_all(&db_dir).ok();
    }
    Ok(())
}
