//! hamane-server のエントリポイント。
//!
//! ```text
//! hamane-server --db ./mydb --listen 127.0.0.1:8080
//! ```
//!
//! Docker では環境変数でも指定できる (todo 802):
//! `HAMANE_DB=/data HAMANE_LISTEN=0.0.0.0:8080 hamane-server`

use std::sync::Arc;

use clap::Parser;
use hamane::Database;

#[derive(Parser)]
#[command(name = "hamane-server", about = "HTTP server for hamane-db", version)]
struct Args {
    /// データベースディレクトリ
    #[arg(long, env = "HAMANE_DB")]
    db: Option<std::path::PathBuf>,
    /// listen アドレス
    #[arg(long, env = "HAMANE_LISTEN", default_value = "127.0.0.1:8080")]
    listen: String,
    /// API キー (Authorization: Bearer <key> / X-Api-Key で検証)。
    /// 未指定なら環境変数 HAMANE_API_KEY、それもなければ認証なし
    #[arg(long, env = "HAMANE_API_KEY")]
    api_key: Option<String>,
    /// 起動中のサーバーの /health を確認して終了する (exit 0 = 正常)。
    /// scratch イメージの Docker HEALTHCHECK 用 (todo 802)
    #[arg(long)]
    healthcheck: bool,
}

/// listen アドレスの /health を std::net だけで叩く (Docker HEALTHCHECK 用。
/// scratch イメージには curl がないため自前で確認する)。
fn healthcheck(listen: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{Read, Write};
    // 0.0.0.0 は bind 用アドレスなので loopback に読み替える
    let addr = match listen.strip_prefix("0.0.0.0:") {
        Some(port) => format!("127.0.0.1:{port}"),
        None => listen.to_string(),
    };
    let timeout = std::time::Duration::from_secs(3);
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse()?, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.take(256).read_to_string(&mut response).ok();
    let status_line = response.lines().next().unwrap_or("");
    if status_line.contains(" 200 ") {
        Ok(())
    } else {
        Err(format!("unhealthy: {status_line:?}").into())
    }
}

/// SIGTERM (Docker/k8s の stop) と Ctrl-C のどちらでもシャットダウンする。
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.healthcheck {
        return healthcheck(&args.listen);
    }
    let Some(db_path) = &args.db else {
        return Err("--db (or HAMANE_DB) is required".into());
    };
    if args.api_key.is_none() {
        eprintln!("warning: no API key configured (--api-key / HAMANE_API_KEY); the server is unauthenticated");
    }
    let db = Arc::new(Database::open(db_path)?);
    let app = hamane_server::router_with_auth(Arc::clone(&db), args.api_key);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    eprintln!("hamane-server listening on {}", args.listen);

    // グレースフルシャットダウン: SIGTERM / Ctrl-C で flush してから終了
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
            eprintln!("shutting down (flushing)...");
        })
        .await?;
    db.flush()?;
    Ok(())
}
