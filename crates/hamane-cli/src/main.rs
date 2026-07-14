//! hamane-db の CLI (todos/403)。動作確認・デバッグ・デモ用。
//!
//! ```text
//! hamane create ./db docs --dim 4 --metric cosine
//! cat records.jsonl | hamane insert ./db docs
//! hamane search ./db docs --vector '[0.1,0.2,0.3,0.4]' --k 5 --filter '{"eq":["lang","ja"]}'
//! hamane info ./db
//! ```

use std::io::BufRead;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use hamane::{CollectionConfig, Database, Filter, MetaValue, Metric, Record};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(name = "hamane", about = "hamane-db vector database CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum MetricArg {
    L2,
    Cosine,
    Dot,
}

impl From<MetricArg> for Metric {
    fn from(m: MetricArg) -> Self {
        match m {
            MetricArg::L2 => Metric::L2,
            MetricArg::Cosine => Metric::Cosine,
            MetricArg::Dot => Metric::Dot,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Collection を作成する
    Create {
        db: PathBuf,
        collection: String,
        #[arg(long)]
        dim: usize,
        #[arg(long, value_enum, default_value = "cosine")]
        metric: MetricArg,
    },
    /// stdin の JSONL ({"id":1,"vector":[...],"meta":{...}}) を挿入する
    Insert { db: PathBuf, collection: String },
    /// 近傍検索する
    Search {
        db: PathBuf,
        collection: String,
        /// クエリベクトル (JSON 配列)
        #[arg(long)]
        vector: String,
        #[arg(long, default_value_t = 10)]
        k: usize,
        /// HNSW の探索幅 (省略時は既定値)
        #[arg(long)]
        ef: Option<usize>,
        /// フィルタ (JSON。例: {"eq":["lang","ja"]}, {"and":[...]})
        #[arg(long)]
        filter: Option<String>,
        /// 人間向けに整形して出力
        #[arg(long)]
        pretty: bool,
    },
    /// collection 一覧と件数を表示する
    Info { db: PathBuf },
    /// memtable をセグメントへ書き出す
    Flush { db: PathBuf },
    /// セグメントを統合して上書き・削除を物理適用する
    Compact { db: PathBuf },
    /// 一貫性のあるバックアップを取る (dest は空ディレクトリ)
    Backup { db: PathBuf, dest: PathBuf },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Create {
            db,
            collection,
            dim,
            metric,
        } => {
            let db = Database::open(&db)?;
            db.create_collection(
                &collection,
                CollectionConfig {
                    dim,
                    metric: metric.into(),
                },
            )?;
            println!("{}", json!({"created": collection, "dim": dim}));
        }
        Command::Insert { db, collection } => {
            let db = Database::open(&db)?;
            let col = db.collection(&collection)?;
            let mut batch = Vec::new();
            let mut total = 0usize;
            for line in std::io::stdin().lock().lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                batch.push(parse_record(&line)?);
                if batch.len() >= 1000 {
                    total += batch.len();
                    col.upsert_batch(std::mem::take(&mut batch))?;
                }
            }
            total += batch.len();
            col.upsert_batch(batch)?;
            println!("{}", json!({"inserted": total}));
        }
        Command::Search {
            db,
            collection,
            vector,
            k,
            ef,
            filter,
            pretty,
        } => {
            let db = Database::open(&db)?;
            let col = db.collection(&collection)?;
            let query: Vec<f32> = serde_json::from_str(&vector)?;
            let mut builder = col.search(&query).k(k);
            if let Some(ef) = ef {
                builder = builder.ef(ef);
            }
            if let Some(f) = &filter {
                builder = builder.filter(parse_filter(&serde_json::from_str(f)?)?);
            }
            let hits: Vec<Value> = builder
                .run()?
                .iter()
                .map(|h| {
                    json!({
                        "id": h.id,
                        "ext_id": h.ext_id(),
                        "score": h.score,
                        "meta": meta_to_json(&h.metadata),
                    })
                })
                .collect();
            let out = json!({ "hits": hits });
            if pretty {
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("{out}");
            }
        }
        Command::Info { db } => {
            let db = Database::open(&db)?;
            let collections: Vec<Value> = db
                .collection_names()
                .iter()
                .map(|name| {
                    let col = db.collection(name).expect("collection exists");
                    let config = col.config();
                    json!({
                        "name": name,
                        "dim": config.dim,
                        "metric": format!("{:?}", config.metric),
                        "len": col.len(),
                        "segments": col
                            .segment_stats()
                            .unwrap_or_default()
                            .iter()
                            .map(|s| json!({
                                "seg_id": s.seg_id,
                                "rows": s.record_count,
                                "tombstones": s.tombstone_count,
                                "hnsw": s.has_hnsw,
                            }))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            println!("{}", json!({ "collections": collections }));
        }
        Command::Flush { db } => {
            Database::open(&db)?.flush()?;
            println!("{}", json!({"flushed": true}));
        }
        Command::Compact { db } => {
            let db = Database::open(&db)?;
            db.flush()?;
            db.compact()?;
            println!("{}", json!({"compacted": true}));
        }
        Command::Backup { db, dest } => {
            let db = Database::open(&db)?;
            db.backup(&dest)?;
            println!("{}", json!({"backup": dest}));
        }
    }
    Ok(())
}

fn parse_record(line: &str) -> Result<Record, Box<dyn std::error::Error>> {
    let v: Value = serde_json::from_str(line)?;
    let id: hamane::RecordId = match v.get("id") {
        Some(Value::Number(n)) => {
            hamane::RecordId::Num(n.as_u64().ok_or("\"id\" must be a non-negative integer")?)
        }
        Some(Value::String(s)) => hamane::RecordId::Str(s.clone()),
        _ => return Err("record needs \"id\" (non-negative integer or string)".into()),
    };
    let vector: Vec<f32> = v
        .get("vector")
        .and_then(Value::as_array)
        .ok_or("record needs \"vector\" array")?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32).ok_or("vector must be numeric"))
        .collect::<Result<_, _>>()?;
    let mut record = Record::new(id, vector);
    if let Some(meta) = v.get("meta").and_then(Value::as_object) {
        for (key, value) in meta {
            record = record.with_meta(key.clone(), json_to_meta(value)?);
        }
    }
    Ok(record)
}

fn json_to_meta(v: &Value) -> Result<MetaValue, Box<dyn std::error::Error>> {
    match v {
        Value::String(s) => Ok(MetaValue::Str(s.clone())),
        Value::Bool(b) => Ok(MetaValue::Bool(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(MetaValue::Int(i))
            } else {
                Ok(MetaValue::Float(n.as_f64().ok_or("invalid number")?))
            }
        }
        _ => Err("meta values must be string/number/bool".into()),
    }
}

fn meta_to_json(meta: &hamane::Metadata) -> Value {
    let map: serde_json::Map<String, Value> = meta
        .iter()
        .map(|(k, v)| {
            let value = match v {
                MetaValue::Str(s) => json!(s),
                MetaValue::Int(i) => json!(i),
                MetaValue::Float(f) => json!(f),
                MetaValue::Bool(b) => json!(b),
            };
            (k.clone(), value)
        })
        .collect();
    Value::Object(map)
}

/// フィルタの JSON 表現をパースする。
/// `{"eq":["k",v]}`, `{"in":["k",[v...]]}`, `{"gt":["k",v]}` (gte/lt/lte 同様)、
/// `{"and":[f...]}`, `{"or":[f...]}`, `{"not":f}`。
fn parse_filter(v: &Value) -> Result<Filter, Box<dyn std::error::Error>> {
    let obj = v.as_object().ok_or("filter must be a JSON object")?;
    let (op, arg) = obj.iter().next().ok_or("empty filter object")?;
    if obj.len() != 1 {
        return Err("filter object must have exactly one key".into());
    }

    let key_value = |arg: &Value| -> Result<(String, MetaValue), Box<dyn std::error::Error>> {
        let pair = arg.as_array().ok_or("expected [key, value]")?;
        if pair.len() != 2 {
            return Err("expected [key, value]".into());
        }
        let key = pair[0].as_str().ok_or("filter key must be string")?;
        Ok((key.to_owned(), json_to_meta(&pair[1])?))
    };

    match op.as_str() {
        "eq" => {
            let (k, v) = key_value(arg)?;
            Ok(Filter::eq(k, v))
        }
        "gt" => {
            let (k, v) = key_value(arg)?;
            Ok(Filter::gt(k, v))
        }
        "gte" => {
            let (k, v) = key_value(arg)?;
            Ok(Filter::gte(k, v))
        }
        "lt" => {
            let (k, v) = key_value(arg)?;
            Ok(Filter::lt(k, v))
        }
        "lte" => {
            let (k, v) = key_value(arg)?;
            Ok(Filter::lte(k, v))
        }
        "in" => {
            let pair = arg.as_array().ok_or("expected [key, [values]]")?;
            if pair.len() != 2 {
                return Err("expected [key, [values]]".into());
            }
            let key = pair[0].as_str().ok_or("filter key must be string")?;
            let values: Vec<MetaValue> = pair[1]
                .as_array()
                .ok_or("expected value array")?
                .iter()
                .map(json_to_meta)
                .collect::<Result<_, _>>()?;
            Ok(Filter::is_in(key, values))
        }
        "and" => {
            let filters: Vec<Filter> = arg
                .as_array()
                .ok_or("expected filter array")?
                .iter()
                .map(parse_filter)
                .collect::<Result<_, _>>()?;
            Ok(Filter::and(filters))
        }
        "or" => {
            let filters: Vec<Filter> = arg
                .as_array()
                .ok_or("expected filter array")?
                .iter()
                .map(parse_filter)
                .collect::<Result<_, _>>()?;
            Ok(Filter::or(filters))
        }
        "not" => Ok(Filter::not(parse_filter(arg)?)),
        other => Err(format!("unknown filter op: {other}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_record_full() {
        let r = parse_record(r#"{"id": 7, "vector": [0.5, -1], "meta": {"lang": "ja", "year": 2026, "public": true}}"#)
            .unwrap();
        assert_eq!(r.id, hamane::RecordId::Num(7));
        assert_eq!(r.vector, vec![0.5, -1.0]);
        assert_eq!(r.metadata.get("lang"), Some(&MetaValue::Str("ja".into())));
        assert_eq!(r.metadata.get("year"), Some(&MetaValue::Int(2026)));
        assert_eq!(r.metadata.get("public"), Some(&MetaValue::Bool(true)));
    }

    #[test]
    fn parse_filter_variants() {
        let f = parse_filter(&serde_json::json!({"eq": ["lang", "ja"]})).unwrap();
        assert_eq!(f, Filter::eq("lang", "ja"));

        let f = parse_filter(&serde_json::json!({
            "and": [{"gt": ["year", 2000]}, {"not": {"eq": ["lang", "en"]}}]
        }))
        .unwrap();
        assert_eq!(
            f,
            Filter::and([
                Filter::gt("year", 2000),
                Filter::not(Filter::eq("lang", "en"))
            ])
        );

        let f = parse_filter(&serde_json::json!({"in": ["lang", ["ja", "en"]]})).unwrap();
        assert_eq!(f, Filter::is_in("lang", ["ja", "en"]));

        assert!(parse_filter(&serde_json::json!({"bad": []})).is_err());
        assert!(parse_filter(&serde_json::json!("eq")).is_err());
    }
}
