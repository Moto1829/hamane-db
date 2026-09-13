//! CLI の E2E テスト (todo 1301 C)。
//!
//! 実バイナリを子プロセスとして起動し、CLI で作った DB が
//! ライブラリからそのまま読めること (逆も) を確認する。
//! ここが壊れると「CLI で入れたデータがサーバから見えない」類の事故になる。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use hamane::{CollectionConfig, Database, Metric, Record};

/// CLI バイナリのパス (cargo がテスト時に渡す)。
fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hamane"))
}

/// 引数を渡して実行し、標準出力を返す (失敗したら panic)。
fn run(args: &[&str]) -> String {
    let out = cli().args(args).output().expect("failed to spawn CLI");
    assert!(
        out.status.success(),
        "cli {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout is utf-8")
}

/// stdin に JSONL を流し込んで insert する。
fn insert_jsonl(db: &Path, collection: &str, lines: &str) -> String {
    let mut child = cli()
        .args(["insert", db.to_str().unwrap(), collection])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn CLI");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(lines.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "insert failed");
    String::from_utf8(out.stdout).unwrap()
}

fn jsonl(n: u64) -> String {
    (0..n)
        .map(|i| {
            let v: Vec<f32> = (0..4).map(|d| (i as f32) + d as f32 * 0.1).collect();
            let lang = if i % 2 == 0 { "ja" } else { "en" };
            format!(
                r#"{{"id":{i},"vector":{v:?},"meta":{{"lang":"{lang}"}}}}"#,
                v = v
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// CLI で作った DB を `Database::open` で読めること。
#[test]
fn cli_written_db_is_readable_by_library() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db_arg = path.to_str().unwrap();

    run(&["create", db_arg, "docs", "--dim", "4", "--metric", "l2"]);
    let inserted = insert_jsonl(&path, "docs", &jsonl(200));
    assert!(inserted.contains("\"inserted\":200"), "got {inserted}");
    run(&["flush", db_arg]);

    // info はセグメント構成まで JSON で出す
    let info = run(&["info", db_arg]);
    assert!(info.contains("\"len\":200"), "info = {info}");

    // ライブラリから開いて同じデータが見える
    let db = Database::open(&path).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), 200);
    assert_eq!(col.get(7u64).unwrap().vector, vec![7.0, 7.1, 7.2, 7.3]);

    let hits = col.search(&[7.0, 7.1, 7.2, 7.3]).k(3).run().unwrap();
    assert_eq!(hits[0].id, 7);
    assert!(hits[0].score < 1e-3, "score = {}", hits[0].score);
}

/// ライブラリで作った DB を CLI から検索できること (フィルタつき)。
#[test]
fn library_written_db_is_searchable_by_cli() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    {
        let db = Database::open(&path).unwrap();
        let col = db
            .create_collection(
                "docs",
                CollectionConfig {
                    dim: 4,
                    metric: Metric::L2,
                },
            )
            .unwrap();
        for i in 0..100u64 {
            let v: Vec<f32> = (0..4).map(|d| i as f32 + d as f32 * 0.1).collect();
            let lang = if i % 2 == 0 { "ja" } else { "en" };
            col.upsert(Record::new(i, v).with_meta("lang", lang))
                .unwrap();
        }
        col.flush().unwrap();
    }

    let db_arg = path.to_str().unwrap();
    let out = run(&[
        "search",
        db_arg,
        "docs",
        "--vector",
        "[10.0,10.1,10.2,10.3]",
        "--k",
        "5",
    ]);
    assert!(out.contains("\"id\":10"), "search output = {out}");

    // フィルタ (奇数 id = en) を指定すると偶数が落ちる
    let filtered = run(&[
        "search",
        db_arg,
        "docs",
        "--vector",
        "[10.0,10.1,10.2,10.3]",
        "--k",
        "5",
        "--filter",
        r#"{"eq":["lang","en"]}"#,
    ]);
    let value: serde_json::Value = serde_json::from_str(&filtered).unwrap();
    let hits = value["hits"].as_array().expect("hits array");
    assert!(!hits.is_empty(), "filtered search returned nothing");
    for hit in hits {
        let id = hit["id"].as_u64().unwrap();
        assert_eq!(id % 2, 1, "filter leaked an even id: {id}");
    }
}

/// CLI の compact でセグメントが統合され、内容は変わらないこと。
#[test]
fn cli_compact_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db_arg = path.to_str().unwrap();

    run(&["create", db_arg, "docs", "--dim", "4", "--metric", "l2"]);
    // 3 世代のセグメントを作る
    for round in 0..3u64 {
        insert_jsonl(&path, "docs", &jsonl(100));
        run(&["flush", db_arg]);
        let _ = round;
    }
    run(&["compact", db_arg]);

    let db = Database::open(&path).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), 100, "同じ id を 3 回入れたので 100 件のはず");
    let stats = col.segment_stats().unwrap();
    assert_eq!(
        stats.len(),
        1,
        "compact 後は 1 セグメント (got {})",
        stats.len()
    );
    assert_eq!(col.get(50u64).unwrap().vector, vec![50.0, 50.1, 50.2, 50.3]);
}

/// CLI の backup で取ったコピーが、そのまま open できる完全な DB であること。
#[test]
fn cli_backup_is_a_usable_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let backup = dir.path().join("backup");
    let db_arg = path.to_str().unwrap();

    run(&["create", db_arg, "docs", "--dim", "4", "--metric", "l2"]);
    insert_jsonl(&path, "docs", &jsonl(300));
    // フラッシュ前 (WAL のみ) の状態でバックアップする
    run(&["backup", db_arg, backup.to_str().unwrap()]);

    let db = Database::open(&backup).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(col.len(), 300, "バックアップの件数が合わない");
    assert_eq!(
        col.get(299u64).unwrap().vector,
        vec![299.0, 299.1, 299.2, 299.3]
    );
    let hits = col
        .search(&[299.0, 299.1, 299.2, 299.3])
        .k(1)
        .run()
        .unwrap();
    assert_eq!(hits[0].id, 299);
}

/// IVF セグメントに対して CLI から --nprobe を指定できること (todo 1301)。
/// M10 で nprobe を足したとき CLI と HTTP に露出し忘れていた回帰の防止。
#[test]
fn cli_search_accepts_nprobe() {
    use hamane::{IndexKind, StoreOptions};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    {
        let db = Database::open_with_options(
            &path,
            StoreOptions {
                index: IndexKind::Ivf,
                hnsw_min_rows: 64,
                ..Default::default()
            },
        )
        .unwrap();
        let col = db
            .create_collection(
                "docs",
                CollectionConfig {
                    dim: 4,
                    metric: Metric::L2,
                },
            )
            .unwrap();
        let recs: Vec<Record> = (0..2000u64)
            .map(|i| Record::new(i, vec![i as f32, 0.0, 0.0, 0.0]))
            .collect();
        col.upsert_batch(recs).unwrap();
        col.flush().unwrap();
    }

    let db_arg = path.to_str().unwrap();
    // nprobe を大きくすると全クラスタを走査するので必ず真の最近傍が返る
    let out = run(&[
        "search",
        db_arg,
        "docs",
        "--vector",
        "[1000.0,0,0,0]",
        "--k",
        "1",
        "--nprobe",
        "1000",
    ]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["hits"][0]["id"].as_u64(), Some(1000), "got {out}");
}

/// scan / count / delete --filter (todos 1601, 1602)。
#[test]
fn cli_scan_count_and_delete_by_filter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db_arg = path.to_str().unwrap();

    run(&["create", db_arg, "docs", "--dim", "4", "--metric", "l2"]);
    insert_jsonl(&path, "docs", &jsonl(100));
    run(&["flush", db_arg]);

    // 列挙は id 昇順。limit と next カーソルが効く
    let out = run(&["scan", db_arg, "docs", "--limit", "10"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<u64> = v["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, (0..10).collect::<Vec<u64>>());
    assert_eq!(v["next"].as_u64(), Some(9));

    let out = run(&["scan", db_arg, "docs", "--limit", "10", "--after", "9"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ids: Vec<u64> = v["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, (10..20).collect::<Vec<u64>>());

    // --ids-only はベクトルを省く
    let out = run(&["scan", db_arg, "docs", "--limit", "1", "--ids-only"]);
    assert!(!out.contains("\"vector\""), "got {out}");

    // count (フィルタあり/なし)
    let out = run(&["count", db_arg, "docs"]);
    assert!(out.contains("\"count\":100"), "got {out}");
    let out = run(&[
        "count",
        db_arg,
        "docs",
        "--filter",
        r#"{"eq":["lang","ja"]}"#,
    ]);
    assert!(out.contains("\"count\":50"), "got {out}");

    // 一括削除
    let out = run(&[
        "delete",
        db_arg,
        "docs",
        "--filter",
        r#"{"eq":["lang","ja"]}"#,
    ]);
    assert!(out.contains("\"deleted\":50"), "got {out}");
    let out = run(&["count", db_arg, "docs"]);
    assert!(out.contains("\"count\":50"), "got {out}");
    // 残ったのは en だけ
    let out = run(&[
        "count",
        db_arg,
        "docs",
        "--filter",
        r#"{"eq":["lang","en"]}"#,
    ]);
    assert!(out.contains("\"count\":50"), "got {out}");
}
