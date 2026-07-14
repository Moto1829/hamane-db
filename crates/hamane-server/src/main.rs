//! hamane-server のエントリポイント。
//!
//! ```text
//! hamane-server --db ./mydb --listen 127.0.0.1:8080
//! ```

use std::sync::Arc;

use clap::Parser;
use hamane::Database;

#[derive(Parser)]
#[command(name = "hamane-server", about = "HTTP server for hamane-db", version)]
struct Args {
    /// データベースディレクトリ
    #[arg(long)]
    db: std::path::PathBuf,
    /// listen アドレス
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: String,
    /// API キー (Authorization: Bearer <key> / X-Api-Key で検証)。
    /// 未指定なら環境変数 HAMANE_API_KEY、それもなければ認証なし
    #[arg(long, env = "HAMANE_API_KEY")]
    api_key: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.api_key.is_none() {
        eprintln!("warning: no API key configured (--api-key / HAMANE_API_KEY); the server is unauthenticated");
    }
    let db = Arc::new(Database::open(&args.db)?);
    let app = hamane_server::router_with_auth(Arc::clone(&db), args.api_key);

    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    eprintln!("hamane-server listening on {}", args.listen);

    // グレースフルシャットダウン: Ctrl-C で flush してから終了
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("shutting down (flushing)...");
        })
        .await?;
    db.flush()?;
    Ok(())
}
