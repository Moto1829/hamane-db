//! HTTP API を Rust から呼ぶ例 (todo 1302)。
//!
//! ```text
//! # 別のターミナルでサーバを起動しておく
//! cargo run --release -p hamane-server -- --db /tmp/hamane-http --api-key secret
//!
//! # 使い方: <base-url> [api-key]
//! cargo run --release -p hamane-server --example http_client -- http://127.0.0.1:8080 secret
//! ```
//!
//! 組み込みで使えるなら [`hamane`] クレートを直接呼ぶ方が速い (プロセス間通信が
//! 無い) ので、HTTP は「別プロセス・別言語から使う」場合の選択肢です。
//! エンドポイントの一覧と curl 版は docs/spec の HTTP API リファレンスを参照。

use std::env;

use reqwest::blocking::Client;
use serde_json::{json, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let base = args
        .next()
        .unwrap_or_else(|| "http://127.0.0.1:8080".into());
    let api_key = args.next();

    let client = Client::new();
    // API キーがあれば全リクエストに付ける (未設定のサーバなら不要)
    let auth = |req: reqwest::blocking::RequestBuilder| match &api_key {
        Some(key) => req.bearer_auth(key),
        None => req,
    };

    // 0. 死活確認 (/health は認証不要)
    let health: Value = client.get(format!("{base}/health")).send()?.json()?;
    println!("health: {health}");

    // 1. collection を作る (既にあれば 409 が返るので無視する)
    let res = auth(client.put(format!("{base}/collections/demo")))
        .json(&json!({ "dim": 4, "metric": "l2" }))
        .send()?;
    println!("create: {} {}", res.status(), res.text()?);

    // 2. まとめて投入する (配列で送ると WAL の fsync がまとまる)
    let records: Vec<Value> = (0..100u64)
        .map(|i| {
            json!({
                "id": i,
                "vector": [i as f32, i as f32 + 0.1, 0.0, 0.0],
                "meta": { "lang": if i % 2 == 0 { "ja" } else { "en" }, "n": i }
            })
        })
        .collect();
    let res: Value = auth(client.post(format!("{base}/collections/demo/records")))
        .json(&records)
        .send()?
        .json()?;
    println!("upsert: {res}");

    // 3. 検索
    let res: Value = auth(client.post(format!("{base}/collections/demo/search")))
        .json(&json!({ "vector": [10.0, 10.1, 0.0, 0.0], "k": 3 }))
        .send()?
        .json()?;
    println!("\nsearch:");
    for hit in res["hits"].as_array().unwrap_or(&vec![]) {
        println!(
            "  id={} score={:.4} meta={}",
            hit["id"],
            hit["score"].as_f64().unwrap_or_default(),
            hit["meta"]
        );
    }

    // 4. フィルタつき検索 (ef / nprobe も同じボディで指定できる)
    let res: Value = auth(client.post(format!("{base}/collections/demo/search")))
        .json(&json!({
            "vector": [10.0, 10.1, 0.0, 0.0],
            "k": 3,
            "ef": 128,
            "filter": { "eq": ["lang", "ja"] }
        }))
        .send()?
        .json()?;
    println!("\nsearch (lang=ja, ef=128):");
    for hit in res["hits"].as_array().unwrap_or(&vec![]) {
        println!("  id={} meta={}", hit["id"], hit["meta"]);
    }

    // 5. 点参照
    let rec: Value = auth(client.get(format!("{base}/collections/demo/records/10")))
        .send()?
        .json()?;
    println!("\nget(10): {rec}");

    // 6. 管理操作 (省略可。閾値で自動的に走る)
    let res: Value = auth(client.post(format!("{base}/admin/flush")))
        .send()?
        .json()?;
    println!("flush: {res}");

    // 7. collection の情報 (件数とセグメント構成)
    let info: Value = auth(client.get(format!("{base}/collections/demo")))
        .send()?
        .json()?;
    println!("info: {info}");

    // 8. 後片付け
    let res = auth(client.delete(format!("{base}/collections/demo"))).send()?;
    println!("drop: {}", res.status());
    Ok(())
}
