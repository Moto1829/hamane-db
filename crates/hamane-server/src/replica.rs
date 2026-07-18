//! replica 側の同期ループ (todo 904、docs/design/replication.md §3, §6)。
//!
//! primary の /replication API をポーリングし、(1) manifest 世代が進んで
//! いればスナップショット同期 (manifest + セグメント)、(2) アクティブ WAL の
//! tail を追記 + follower memtable へ適用、を繰り返す。ディスクレイアウトを
//! primary と同一に保つため、昇格は `--replicate-from` なしで開き直すだけでよい。
//!
//! HTTP クライアントは std::net の最小実装 (GET のみ、`Content-Length` 前提)。
//! chunked encoding を返す中間プロキシ越しでは動かない (既知の制限)。

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hamane::Database;
use hamane_storage::format::MAGIC_WAL;
use hamane_storage::manifest::{manifest_file_name, Manifest, CURRENT_FILE};
use hamane_storage::segment::{self, segment_dir_name};
use hamane_storage::wal::wal_file_name;

type SyncResult<T> = Result<T, std::io::Error>;

fn other(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

/// `http://host:port` への最小 GET。(ステータス, ボディ) を返す。
/// Content-Length があればボディ長を検証する (途中切断の検出)。
fn http_get(base: &str, path: &str, api_key: Option<&str>) -> SyncResult<(u16, Vec<u8>)> {
    let host = base
        .strip_prefix("http://")
        .ok_or_else(|| other(format!("only http:// is supported: {base}")))?
        .trim_end_matches('/');
    let timeout = Duration::from_secs(30);
    let addr = host
        .parse()
        .map_err(|e| other(format!("invalid address {host}: {e}")))?;
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let auth = match api_key {
        Some(k) => format!("X-Api-Key: {k}\r\n"),
        None => String::new(),
    };
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;

    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| other("malformed HTTP response"))?;
    let head = std::str::from_utf8(&raw[..header_end]).map_err(|_| other("non-utf8 header"))?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| other(format!("malformed status line: {head:.64}")))?;
    let body = raw[header_end + 4..].to_vec();
    if let Some(len) = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse::<usize>().ok())?
    }) {
        if body.len() != len {
            return Err(other(format!(
                "truncated response: got {} of {len} bytes",
                body.len()
            )));
        }
    }
    Ok((status, body))
}

/// primary への同期クライアント。単一スレッドから使うこと。
pub struct ReplicaSync {
    base: String,
    api_key: Option<String>,
    db_dir: PathBuf,
    db: Arc<Database>,
    /// tail 追跡中の WAL seq
    wal_seq: Option<u64>,
    /// ローカル WAL ファイルの長さ (magic + 完全なフレームのみを書く)
    file_len: u64,
    /// fetch 済みだがフレーム未完でディスク未書き込みのバイト列
    pending: Vec<u8>,
}

impl ReplicaSync {
    pub fn new(base: String, api_key: Option<String>, db: Arc<Database>) -> Self {
        let db_dir = db
            .path()
            .expect("replica database is persistent")
            .to_path_buf();
        Self {
            base,
            api_key,
            db_dir,
            db,
            wal_seq: None,
            file_len: 0,
            pending: Vec::new(),
        }
    }

    fn get(&self, path: &str) -> SyncResult<Vec<u8>> {
        let (status, body) = http_get(&self.base, path, self.api_key.as_deref())?;
        match status {
            200 => Ok(body),
            404 => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{path}: 404"),
            )),
            s => Err(other(format!("{path}: HTTP {s}"))),
        }
    }

    /// 1 回の同期 (state → 必要ならスナップショット → WAL tail)。
    ///
    /// エラーは呼び出し側がログして次のポーリングで再試行する。
    /// 404 (rotate・コンパクション競合) も同様に次回の再同期で回復する。
    pub fn sync_once(&mut self) -> SyncResult<()> {
        let state: serde_json::Value = serde_json::from_slice(&self.get("/replication/state")?)
            .map_err(|e| other(format!("bad state json: {e}")))?;
        let gen = state["manifest_gen"]
            .as_u64()
            .ok_or_else(|| other("state missing manifest_gen"))?;
        if gen > self.db.manifest_gen() {
            self.snapshot_sync(gen)?;
        }
        if let Some(seq) = state["wal_seq"].as_u64() {
            self.tail_sync(seq)?;
        }
        Ok(())
    }

    /// manifest 世代の同期: 足りないセグメントファイルを fetch して
    /// CURRENT を切り替え、Store の状態を差し替える。
    fn snapshot_sync(&mut self, gen: u64) -> SyncResult<()> {
        let manifest_bytes = self.get(&format!("/replication/manifest/{gen}"))?;
        let manifest =
            Manifest::decode(&manifest_bytes).map_err(|e| other(format!("bad manifest: {e}")))?;

        for col in &manifest.collections {
            for seg in &col.segments {
                let seg_dir = self
                    .db_dir
                    .join("collections")
                    .join(col.collection_id.to_string())
                    .join(segment_dir_name(seg.seg_id));
                std::fs::create_dir_all(&seg_dir)?;
                let required = [
                    segment::FILE_VECTORS,
                    segment::FILE_IDS,
                    segment::FILE_META,
                    segment::FILE_TOMBSTONES,
                ];
                let optional = [segment::FILE_HNSW, segment::FILE_SQ8];
                for file in required.iter().chain(&optional) {
                    let dest = seg_dir.join(file);
                    if dest.exists() {
                        continue;
                    }
                    let url = format!(
                        "/replication/segment/{}/{}/{file}",
                        col.collection_id, seg.seg_id
                    );
                    match self.get(&url) {
                        Ok(bytes) => write_durable(&dest, &bytes)?,
                        // hnsw / sq8 はセグメントに存在しないことがある
                        Err(e)
                            if e.kind() == std::io::ErrorKind::NotFound
                                && optional.contains(file) =>
                        {
                            continue
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }

        // manifest → CURRENT の順で書く (ローカル open と同じ原子性。
        // ここでクラッシュしても CURRENT は常に完全な世代を指す)
        let name = manifest_file_name(gen);
        write_durable(&self.db_dir.join(&name), &manifest_bytes)?;
        let tmp = self.db_dir.join("CURRENT.tmp");
        write_durable(&tmp, format!("{name}\n").as_bytes())?;
        std::fs::rename(&tmp, self.db_dir.join(CURRENT_FILE))?;

        self.db
            .switch_generation()
            .map_err(|e| other(format!("switch_generation: {e}")))?;
        // load_state が古い WAL を削除しているので追跡をリセットする
        self.wal_seq = None;
        self.pending.clear();
        Ok(())
    }

    /// アクティブ WAL の tail を fetch し、完全なフレームだけを
    /// ローカルファイルへ追記 + memtable へ適用する。
    fn tail_sync(&mut self, seq: u64) -> SyncResult<()> {
        if self.wal_seq != Some(seq) {
            // 追跡対象の切り替え (初回 or rotate 後)。ローカルの長さから再開
            self.wal_seq = Some(seq);
            self.pending.clear();
            self.file_len = std::fs::metadata(self.wal_path(seq))
                .map(|m| m.len())
                .unwrap_or(0);
        }
        let offset = self.file_len + self.pending.len() as u64;
        let fetched = self.get(&format!("/replication/wal/{seq}?offset={offset}"))?;
        if fetched.is_empty() {
            return Ok(());
        }
        self.pending.extend_from_slice(&fetched);

        // ファイル先頭なら magic を検証して書き出す
        if self.file_len == 0 {
            if self.pending.len() < MAGIC_WAL.len() {
                return Ok(()); // magic すら揃っていない。次回に持ち越し
            }
            if self.pending[..MAGIC_WAL.len()] != MAGIC_WAL {
                return Err(other("bad WAL magic from primary"));
            }
            append_durable(&self.wal_path(seq), &MAGIC_WAL)?;
            self.file_len = MAGIC_WAL.len() as u64;
            self.pending.drain(..MAGIC_WAL.len());
        }

        // 完全なフレームだけ適用し、同じバイト列をディスクにも残す
        // (クラッシュしても通常のリプレイで同じ状態に戻る)
        let consumed = self
            .db
            .apply_wal_frames(&self.pending)
            .map_err(|e| other(format!("apply_wal_frames: {e}")))?;
        if consumed > 0 {
            append_durable(&self.wal_path(seq), &self.pending[..consumed])?;
            self.file_len += consumed as u64;
            self.pending.drain(..consumed);
        }
        Ok(())
    }

    fn wal_path(&self, seq: u64) -> PathBuf {
        self.db_dir.join("wal").join(wal_file_name(seq))
    }

    /// ポーリングループ (デーモンスレッドで回す)。
    pub fn run(mut self, interval: Duration) {
        let mut last_error: Option<String> = None;
        loop {
            match self.sync_once() {
                Ok(()) => last_error = None,
                Err(e) => {
                    // 同じエラーの連続は 1 回だけログする
                    let msg = e.to_string();
                    if last_error.as_deref() != Some(&msg) {
                        eprintln!("replica sync error (will retry): {msg}");
                        last_error = Some(msg);
                    }
                }
            }
            std::thread::sleep(interval);
        }
    }
}

fn write_durable(path: &std::path::Path, bytes: &[u8]) -> SyncResult<()> {
    std::fs::write(path, bytes)?;
    std::fs::File::open(path)?.sync_data()
}

fn append_durable(path: &std::path::Path, bytes: &[u8]) -> SyncResult<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_data()
}
