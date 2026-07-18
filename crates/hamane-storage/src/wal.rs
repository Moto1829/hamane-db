//! Write-Ahead Log (docs/design/storage.md §2)。
//!
//! ファイル構成: 8 バイト magic の後にフレーム列
//! `crc32c u32 | len u32 | type u8 | payload`。
//! 末尾の部分書き込み・CRC 不一致はクラッシュ痕跡として、そこで読み取りを停止する。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use hamane_core::{Id, Metadata, Metric, Result};

use crate::format::{
    self, corrupted, metric_from_u8, metric_to_u8, put_frame, put_metadata, put_string,
    read_metadata, Frame, Reader, MAGIC_WAL,
};

const TYPE_UPSERT: u8 = 1;
const TYPE_DELETE: u8 = 2;
const TYPE_CREATE_COLLECTION: u8 = 3;
const TYPE_DROP_COLLECTION: u8 = 4;

/// WAL に記録される操作。vector は検証・正規化済みであること。
#[derive(Debug, Clone, PartialEq)]
pub enum WalRecord {
    Upsert {
        collection_id: u32,
        id: Id,
        vector: Vec<f32>,
        metadata: Metadata,
    },
    Delete {
        collection_id: u32,
        id: Id,
    },
    CreateCollection {
        collection_id: u32,
        name: String,
        dim: u32,
        metric: Metric,
    },
    DropCollection {
        collection_id: u32,
    },
}

impl WalRecord {
    fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            WalRecord::Upsert {
                collection_id,
                id,
                vector,
                metadata,
            } => {
                format::put_u8(&mut body, TYPE_UPSERT);
                format::put_u32(&mut body, *collection_id);
                format::put_u64(&mut body, *id);
                format::put_u32(&mut body, vector.len() as u32);
                format::put_f32_slice(&mut body, vector);
                put_metadata(&mut body, metadata);
            }
            WalRecord::Delete { collection_id, id } => {
                format::put_u8(&mut body, TYPE_DELETE);
                format::put_u32(&mut body, *collection_id);
                format::put_u64(&mut body, *id);
            }
            WalRecord::CreateCollection {
                collection_id,
                name,
                dim,
                metric,
            } => {
                format::put_u8(&mut body, TYPE_CREATE_COLLECTION);
                format::put_u32(&mut body, *collection_id);
                put_string(&mut body, name);
                format::put_u32(&mut body, *dim);
                format::put_u8(&mut body, metric_to_u8(*metric));
            }
            WalRecord::DropCollection { collection_id } => {
                format::put_u8(&mut body, TYPE_DROP_COLLECTION);
                format::put_u32(&mut body, *collection_id);
            }
        }
        body
    }

    pub(crate) fn decode(body: &[u8]) -> Result<Self> {
        let mut r = Reader::new(body);
        let record = match r.u8()? {
            TYPE_UPSERT => {
                let collection_id = r.u32()?;
                let id = r.u64()?;
                let dim = r.u32()? as usize;
                let vector = r.f32_vec(dim)?;
                let metadata = read_metadata(&mut r)?;
                WalRecord::Upsert {
                    collection_id,
                    id,
                    vector,
                    metadata,
                }
            }
            TYPE_DELETE => WalRecord::Delete {
                collection_id: r.u32()?,
                id: r.u64()?,
            },
            TYPE_CREATE_COLLECTION => WalRecord::CreateCollection {
                collection_id: r.u32()?,
                name: r.string()?,
                dim: r.u32()?,
                metric: metric_from_u8(r.u8()?)?,
            },
            TYPE_DROP_COLLECTION => WalRecord::DropCollection {
                collection_id: r.u32()?,
            },
            t => return Err(corrupted(format!("unknown WAL record type: {t}"))),
        };
        if !r.is_empty() {
            return Err(corrupted("trailing bytes in WAL record"));
        }
        Ok(record)
    }
}

/// fsync のタイミング。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPolicy {
    /// sync() 呼び出しごとに必ず fsync (既定)
    Always,
    /// n 回の sync() 呼び出しに 1 回 fsync。
    /// クラッシュで直近最大 n−1 件の確認応答済み書き込みを失い得る
    EveryN(u32),
    /// group commit: 並行する書き込みの fsync を 1 回にまとめる (todo 505)。
    /// fsync 完了までは呼び出し元に Ok が返らないため、確認応答済みの
    /// 書き込みは常に永続 (Always と同じ耐久性で、並行時のみ高速)
    Batch,
}

/// WAL ファイル名: `<seq:020>.wal`
pub fn wal_file_name(seq: u64) -> String {
    format!("{seq:020}.wal")
}

/// wal ディレクトリ内のファイルを seq 昇順で列挙する。
pub fn list_wal_files(dir: &Path) -> Result<Vec<(u64, PathBuf)>> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(stem) = name.strip_suffix(".wal") {
            if let Ok(seq) = stem.parse::<u64>() {
                files.push((seq, path));
            }
        }
    }
    files.sort_by_key(|(seq, _)| *seq);
    Ok(files)
}

/// group commit の進行状態 (SyncPolicy::Batch)。
#[derive(Default)]
struct BatchState {
    /// 追記済み (未 fsync を含む) のレコード数
    appended: u64,
    /// fsync で永続化済みのレコード数
    synced: u64,
    /// いずれかのスレッドが fsync 実行中か
    syncing: bool,
}

/// WAL ファイルの共有ハンドル。SyncToken が WalWriter より長生きできるよう
/// Arc で共有する (rotate 後も未完了の fsync 待ちを解決できる)。
struct WalFile {
    file: File,
    batch: std::sync::Mutex<BatchState>,
    cond: std::sync::Condvar,
}

/// Batch モードの fsync 待ちチケット。`wait()` は自分の書き込みが
/// 永続化されるまでブロックする (leader-follower group commit:
/// 最初に到達したスレッドが fsync を実行し、後続はその完了に相乗りする)。
pub struct SyncToken {
    file: Arc<WalFile>,
    seq: u64,
}

impl SyncToken {
    /// 自分の書き込みを含む fsync の完了を待つ。Ok = 永続化済み。
    pub fn wait(self) -> Result<()> {
        let mut batch = self.file.batch.lock().expect("lock poisoned");
        loop {
            if batch.synced >= self.seq {
                return Ok(());
            }
            if !batch.syncing {
                // 自分が leader になり、この時点までの追記をまとめて fsync する
                batch.syncing = true;
                let target = batch.appended;
                drop(batch);
                let result = self.file.file.sync_data();
                batch = self.file.batch.lock().expect("lock poisoned");
                batch.syncing = false;
                if result.is_ok() {
                    batch.synced = batch.synced.max(target);
                }
                self.file.cond.notify_all();
                result?;
            } else {
                batch = self.file.cond.wait(batch).expect("lock poisoned");
            }
        }
    }
}

/// WAL の追記ライタ。
pub struct WalWriter {
    inner: Arc<WalFile>,
    policy: SyncPolicy,
    unsynced: u32,
}

use std::sync::Arc;

impl WalWriter {
    /// 新しい WAL ファイルを作成する (既存ファイルはエラー)。
    pub fn create(path: &Path, policy: SyncPolicy) -> Result<Self> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&MAGIC_WAL)?;
        file.sync_data()?;
        Ok(Self {
            inner: Arc::new(WalFile {
                file,
                batch: std::sync::Mutex::new(BatchState::default()),
                cond: std::sync::Condvar::new(),
            }),
            policy,
            unsynced: 0,
        })
    }

    /// レコードを追記する。sync() (Batch では token の wait()) まで永続化は保証されない。
    pub fn append(&mut self, record: &WalRecord) -> Result<()> {
        let mut frame = Vec::new();
        put_frame(&mut frame, &record.encode());
        (&self.inner.file).write_all(&frame)?;
        self.inner.batch.lock().expect("lock poisoned").appended += 1;
        Ok(())
    }

    /// ポリシーに従って fsync する。
    ///
    /// - Always / EveryN: この呼び出し内で完結し、None を返す
    /// - Batch: fsync せずトークンを返す。呼び出し側は**ロックの外で**
    ///   `token.wait()` を呼ぶこと (そこで group commit される)
    pub fn sync(&mut self) -> Result<Option<SyncToken>> {
        match self.policy {
            SyncPolicy::Always => {
                self.inner.file.sync_data()?;
                Ok(None)
            }
            SyncPolicy::EveryN(n) => {
                self.unsynced += 1;
                if self.unsynced >= n {
                    self.inner.file.sync_data()?;
                    self.unsynced = 0;
                }
                Ok(None)
            }
            SyncPolicy::Batch => {
                let seq = self.inner.batch.lock().expect("lock poisoned").appended;
                Ok(Some(SyncToken {
                    file: Arc::clone(&self.inner),
                    seq,
                }))
            }
        }
    }
}

/// WAL の読み取り結果。
pub struct WalReplay {
    pub records: Vec<WalRecord>,
    /// 完全なフレーム列の終端オフセット。これ以降は部分書き込みとして
    /// 呼び出し側が truncate してよい
    pub valid_len: u64,
}

/// WAL 読み取り。
pub struct WalReader;

impl WalReader {
    /// ファイル全体を読み、完全なレコード列と有効長を返す。
    ///
    /// 末尾の部分書き込み・CRC 不一致ではエラーにせず、そこで停止する
    /// (docs/design/storage.md §2)。magic 不一致は Corrupted。
    pub fn read_all(path: &Path) -> Result<WalReplay> {
        let buf = std::fs::read(path)?;
        if buf.len() < MAGIC_WAL.len() || buf[..MAGIC_WAL.len()] != MAGIC_WAL {
            return Err(corrupted(format!("bad WAL magic in {}", path.display())));
        }
        let mut records = Vec::new();
        let mut pos = MAGIC_WAL.len();
        while let Frame::Ok { body, consumed } = format::read_frame(&buf[pos..]) {
            // フレームは完全なので decode 失敗は真の破損 (エラーにする)
            records.push(WalRecord::decode(body)?);
            pos += consumed;
        }
        Ok(WalReplay {
            records,
            valid_len: pos as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hamane_core::MetaValue;

    fn sample_records() -> Vec<WalRecord> {
        let mut meta = Metadata::new();
        meta.insert("lang".into(), MetaValue::Str("ja".into()));
        vec![
            WalRecord::CreateCollection {
                collection_id: 1,
                name: "docs".into(),
                dim: 3,
                metric: Metric::Cosine,
            },
            WalRecord::Upsert {
                collection_id: 1,
                id: 42,
                vector: vec![1.0, 0.0, 0.0],
                metadata: meta,
            },
            WalRecord::Delete {
                collection_id: 1,
                id: 42,
            },
            WalRecord::DropCollection { collection_id: 1 },
        ]
    }

    fn write_wal(path: &Path, records: &[WalRecord]) {
        let mut w = WalWriter::create(path, SyncPolicy::Always).unwrap();
        for rec in records {
            w.append(rec).unwrap();
            w.sync().unwrap();
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(wal_file_name(1));
        let records = sample_records();
        write_wal(&path, &records);

        let replay = WalReader::read_all(&path).unwrap();
        assert_eq!(replay.records, records);
        assert_eq!(replay.valid_len, std::fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn truncation_at_every_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(wal_file_name(1));
        let records = sample_records();
        write_wal(&path, &records);
        let full = std::fs::read(&path).unwrap();

        // フレーム境界を求める: 各プレフィックスで読めるレコード数は
        // 「切り詰め位置までに完全に収まるフレーム数」に一致するはず
        let mut boundaries = vec![MAGIC_WAL.len()];
        {
            let mut pos = MAGIC_WAL.len();
            while let Frame::Ok { consumed, .. } = format::read_frame(&full[pos..]) {
                pos += consumed;
                boundaries.push(pos);
            }
        }

        let cut_path = dir.path().join("cut.wal");
        for cut in MAGIC_WAL.len()..=full.len() {
            std::fs::write(&cut_path, &full[..cut]).unwrap();
            let replay = WalReader::read_all(&cut_path).unwrap();
            let expected = boundaries.iter().filter(|b| **b <= cut).count() - 1;
            assert_eq!(replay.records.len(), expected, "cut={cut}");
            assert_eq!(replay.records[..], records[..expected]);
            // valid_len は最後の完全フレームの終端
            assert_eq!(
                replay.valid_len as usize,
                *boundaries.iter().rfind(|b| **b <= cut).unwrap()
            );
        }
    }

    #[test]
    fn corrupted_frame_stops_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(wal_file_name(1));
        let records = sample_records();
        write_wal(&path, &records);

        // 2 番目のフレームの body を 1 バイト破壊
        let mut buf = std::fs::read(&path).unwrap();
        let mut pos = MAGIC_WAL.len();
        if let Frame::Ok { consumed, .. } = format::read_frame(&buf[pos..]) {
            pos += consumed;
        }
        buf[pos + format::FRAME_HEADER_LEN] ^= 0xFF;
        std::fs::write(&path, &buf).unwrap();

        let replay = WalReader::read_all(&path).unwrap();
        assert_eq!(replay.records.len(), 1); // 1 番目だけ読める
        assert_eq!(replay.valid_len as usize, pos);
    }

    #[test]
    fn bad_magic_is_corrupted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.wal");
        std::fs::write(&path, b"NOTMAGIC").unwrap();
        assert!(WalReader::read_all(&path).is_err());
    }

    #[test]
    fn list_wal_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for seq in [3u64, 1, 2] {
            WalWriter::create(&dir.path().join(wal_file_name(seq)), SyncPolicy::Always).unwrap();
        }
        std::fs::write(dir.path().join("junk.txt"), b"x").unwrap();
        let files = list_wal_files(dir.path()).unwrap();
        assert_eq!(
            files.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }
}
