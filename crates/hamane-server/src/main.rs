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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let db = Arc::new(Database::open(&args.db)?);
    let app = hamane_server::router(Arc::clone(&db));

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
