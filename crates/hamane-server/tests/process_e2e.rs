//! 実プロセスの E2E (todo 1301 C)。
//!
//! `hamane-server` バイナリを子プロセスとして起動し、HTTP 越しに操作する。
//! 既存の `api.rs` は tower の `oneshot` でルーターを直接叩くので、
//! 「本当にプロセスとして起動して待ち受けるか」「再起動でデータが残るか」
//! 「レプリカを昇格できるか」はここでしか確認できない。
//!
//! HTTP クライアントは依存を増やさないよう std::net で最小限を書く
//! (`replica.rs` の puller と同じ方針。Content-Length 前提)。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 起動した子プロセス。Drop で必ず kill する (テストが失敗しても残さない)。
struct Server {
    child: Child,
    addr: String,
    stopped: bool,
}

impl Server {
    /// 空きポートを取って起動し、/health が返るまで待つ。
    fn start(db: &Path, extra: &[&str]) -> Self {
        let port = free_port();
        let addr = format!("127.0.0.1:{port}");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hamane-server"));
        cmd.args(["--db", db.to_str().unwrap(), "--listen", &addr])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().expect("failed to spawn hamane-server");
        let server = Self {
            child,
            addr,
            stopped: false,
        };
        server.wait_ready();
        server
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(res) = self.try_request("GET", "/health", None) {
                if res.contains("\"status\":\"ok\"") {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not become ready: {}", self.addr);
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> String {
        self.try_request(method, path, body)
            .unwrap_or_else(|e| panic!("{method} {path} failed: {e}"))
    }

    fn try_request(&self, method: &str, path: &str, body: Option<&str>) -> std::io::Result<String> {
        let timeout = Duration::from_secs(10);
        let mut stream = TcpStream::connect_timeout(&self.addr.parse().unwrap(), timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let body = body.unwrap_or("");
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.addr,
            body.len()
        )?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        Ok(raw)
    }

    /// 停止して終了を待つ (再起動テスト用。ロックが解放されるまで待つ必要がある)。
    fn stop(mut self) {
        self.terminate();
    }

    fn terminate(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.stopped = true;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // テストが panic しても子プロセスを残さない
        self.terminate();
    }
}

/// 空いている TCP ポートを 1 つ確保して返す。
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn vector_json(i: u64) -> String {
    let v: Vec<f32> = (0..4).map(|d| i as f32 + d as f32 * 0.1).collect();
    format!("{v:?}")
}

/// 実プロセスで起動 → 投入 → **再起動** → データが残っていること。
/// フラッシュ前 (WAL のみ) と後の両方を確認する。
#[test]
fn server_restart_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");

    let server = Server::start(&db, &[]);
    server.request(
        "PUT",
        "/collections/docs",
        Some(r#"{"dim":4,"metric":"l2"}"#),
    );
    for i in 0..50u64 {
        let body = format!(r#"{{"id":{i},"vector":{}}}"#, vector_json(i));
        let res = server.request("POST", "/collections/docs/records", Some(&body));
        assert!(res.contains("200 OK"), "upsert failed: {res}");
    }
    // フラッシュせずに落とす = WAL からの復旧経路
    server.stop();

    let server = Server::start(&db, &[]);
    let info = server.request("GET", "/collections/docs", None);
    assert!(info.contains("\"len\":50"), "after restart (wal): {info}");

    // フラッシュしてからもう一度落とす = セグメントからの復旧経路
    server.request("POST", "/admin/flush", Some("{}"));
    for i in 50..80u64 {
        let body = format!(r#"{{"id":{i},"vector":{}}}"#, vector_json(i));
        server.request("POST", "/collections/docs/records", Some(&body));
    }
    server.stop();

    let server = Server::start(&db, &[]);
    let info = server.request("GET", "/collections/docs", None);
    assert!(
        info.contains("\"len\":80"),
        "after restart (segment+wal): {info}"
    );

    // 検索も通る
    let body = format!(r#"{{"vector":{},"k":3}}"#, vector_json(60));
    let res = server.request("POST", "/collections/docs/search", Some(&body));
    assert!(res.contains("\"id\":60"), "search after restart: {res}");
}

/// primary → replica を実プロセスで同期し、**replica を昇格**できること。
///
/// 昇格 = `--replicate-from` を外して起動し直すだけ (docs/design/replication.md)。
#[test]
fn replica_syncs_and_can_be_promoted() {
    let dir = tempfile::tempdir().unwrap();
    let primary_dir = dir.path().join("primary");
    let replica_dir = dir.path().join("replica");

    let primary = Server::start(&primary_dir, &[]);
    primary.request(
        "PUT",
        "/collections/docs",
        Some(r#"{"dim":4,"metric":"l2"}"#),
    );
    for i in 0..30u64 {
        let body = format!(r#"{{"id":{i},"vector":{}}}"#, vector_json(i));
        primary.request("POST", "/collections/docs/records", Some(&body));
    }
    primary.request("POST", "/admin/flush", Some("{}"));

    let primary_url = format!("http://{}", primary.addr);
    let replica = Server::start(
        &replica_dir,
        &[
            "--replicate-from",
            &primary_url,
            "--poll-interval-ms",
            "100",
        ],
    );

    // /health が replica を名乗る
    let health = replica.request("GET", "/health", None);
    assert!(health.contains("\"role\":\"replica\""), "health = {health}");

    // 同期を待つ (ポーリング間隔 100ms)
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let info = replica.request("GET", "/collections/docs", None);
        if info.contains("\"len\":30") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "replica did not catch up: {info}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // replica への書き込みは 409 (ReadOnlyReplica)
    let body = format!(r#"{{"id":999,"vector":{}}}"#, vector_json(999));
    let res = replica.request("POST", "/collections/docs/records", Some(&body));
    assert!(
        res.contains("409"),
        "write to replica must be rejected: {res}"
    );

    // 昇格: primary を止め、replica を --replicate-from 無しで起動し直す
    primary.stop();
    replica.stop();
    let promoted = Server::start(&replica_dir, &[]);
    let health = promoted.request("GET", "/health", None);
    assert!(
        health.contains("\"role\":\"primary\""),
        "promoted health = {health}"
    );

    let info = promoted.request("GET", "/collections/docs", None);
    assert!(info.contains("\"len\":30"), "promoted lost data: {info}");

    // 昇格後は書ける
    let body = format!(r#"{{"id":999,"vector":{}}}"#, vector_json(999));
    let res = promoted.request("POST", "/collections/docs/records", Some(&body));
    assert!(res.contains("200 OK"), "promoted must accept writes: {res}");
    let info = promoted.request("GET", "/collections/docs", None);
    assert!(info.contains("\"len\":31"), "promoted write lost: {info}");
}
