//! クラッシュ耐性テスト (todos/210, docs/design/storage.md §2, §5–6)。
//!
//! どの時点でプロセスが死んでも「再 open で ack 済み操作の prefix が復元され、
//! 半端な状態が見えない」ことをファイル操作でクラッシュ状態を再現して検証する。

use std::path::Path;

use hamane_core::{Metadata, Metric};
use hamane_storage::format::{read_frame, Frame, MAGIC_WAL};
use hamane_storage::wal::wal_file_name;
use hamane_storage::{SegmentWriter, Store, StoreOptions, StoredRecord};

fn rec(v: Vec<f32>) -> StoredRecord {
    StoredRecord {
        vector: v,
        metadata: Metadata::new(),
    }
}

fn open(dir: &Path) -> Store {
    Store::open(dir, StoreOptions::default()).unwrap()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// WAL バイト列の完全フレーム数を数える。
fn count_complete_frames(buf: &[u8]) -> usize {
    let mut pos = MAGIC_WAL.len();
    let mut n = 0;
    while pos <= buf.len() {
        match read_frame(&buf[pos..]) {
            Frame::Ok { consumed, .. } => {
                pos += consumed;
                n += 1;
            }
            Frame::Torn => break,
        }
    }
    n
}

/// WAL を末尾から全バイト位置で切り詰め、それぞれで
/// 「完全フレーム分の操作 prefix」だけが復元されることを確認する。
#[test]
fn wal_truncation_recovers_exact_prefix() {
    let src = tempfile::tempdir().unwrap();
    const N: u64 = 8;
    {
        let store = open(src.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        for i in 0..N {
            store
                .upsert(info.collection_id, i, rec(vec![i as f32]))
                .unwrap();
        }
    }
    // アクティブ WAL は seq=1 (open 時に作成)
    let wal_rel = Path::new("wal").join(wal_file_name(1));
    let full = std::fs::read(src.path().join(&wal_rel)).unwrap();

    let work = tempfile::tempdir().unwrap();
    for cut in MAGIC_WAL.len()..=full.len() {
        let dst = work.path().join(format!("cut-{cut}"));
        copy_dir(src.path(), &dst);
        let truncated = &full[..cut];
        std::fs::write(dst.join(&wal_rel), truncated).unwrap();

        let frames = count_complete_frames(truncated);
        let store = open(&dst);
        if frames == 0 {
            // create_collection すら届いていない
            assert!(store.collection_names().is_empty(), "cut={cut}");
        } else {
            let info = store.collection_info("docs").unwrap();
            let view = store.view(info.collection_id).unwrap();
            let expected_upserts = frames - 1; // フレーム 1 個目は CreateCollection
            assert_eq!(view.live_len(), expected_upserts, "cut={cut}");
            // 復元されるのは必ず操作列の prefix
            for i in 0..expected_upserts as u64 {
                assert_eq!(view.get(i).unwrap().vector, vec![i as f32], "cut={cut}");
            }
        }
    }
}

/// フラッシュの「セグメント rename 後・manifest 書き込み前」のクラッシュ:
/// 孤児セグメントは掃除され、WAL から状態が復元される。
#[test]
fn orphan_segment_is_cleaned_and_wal_wins() {
    let dir = tempfile::tempdir().unwrap();
    let cid = {
        let store = open(dir.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        store.upsert(info.collection_id, 1, rec(vec![1.0])).unwrap();
        store.flush().unwrap(); // seg 0 が正規に存在
        store.upsert(info.collection_id, 2, rec(vec![2.0])).unwrap();
        info.collection_id
    };

    // クラッシュ再現: 次の seg_id で孤児セグメントを作る (manifest 未更新)
    let col_dir = dir.path().join("collections").join(cid.to_string());
    let mut orphan = hamane_storage::Memtable::new();
    orphan.upsert(99, rec(vec![99.0]));
    SegmentWriter::write(&col_dir, 1, &orphan.snapshot(), None).unwrap();

    let store = open(dir.path());
    let view = store.view(cid).unwrap();
    assert_eq!(view.segments.len(), 1, "orphan segment must be removed");
    assert!(view.get(99).is_none());
    assert_eq!(view.get(2).unwrap().vector, vec![2.0]); // WAL から復元

    // 掃除済みなので同じ seg_id での次のフラッシュが成功する
    store.flush().unwrap();
    let view = store.view(cid).unwrap();
    assert_eq!(view.live_len(), 2);
}

/// フラッシュの「manifest 切り替え後・旧 WAL 削除前」のクラッシュ:
/// 旧 WAL は無視・削除され、二重適用が起きない。
#[test]
fn stale_wal_after_manifest_switch_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let cid = {
        let store = open(dir.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        store.upsert(info.collection_id, 1, rec(vec![1.0])).unwrap();
        // フラッシュ前の WAL (seq=1) を退避
        let wal_path = dir.path().join("wal").join(wal_file_name(1));
        let stale = std::fs::read(&wal_path).unwrap();
        store.flush().unwrap(); // manifest.wal_seq = 1、seq=1 は削除される
                                // クラッシュ再現: 旧 WAL が消される前に落ちた状態
        std::fs::write(&wal_path, &stale).unwrap();
        info.collection_id
    };

    let store = open(dir.path());
    let view = store.view(cid).unwrap();
    assert_eq!(view.live_len(), 1);
    assert!(view.memtable.is_empty(), "stale WAL must not be replayed");
    // 旧 WAL は掃除されている
    assert!(!dir.path().join("wal").join(wal_file_name(1)).exists());
}

/// フラッシュ途中の `.tmp` セグメント残骸は無害に掃除される。
#[test]
fn tmp_segment_dir_is_cleaned() {
    let dir = tempfile::tempdir().unwrap();
    let cid = {
        let store = open(dir.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        store.upsert(info.collection_id, 1, rec(vec![1.0])).unwrap();
        store.flush().unwrap();
        info.collection_id
    };
    let col_dir = dir.path().join("collections").join(cid.to_string());
    std::fs::create_dir(col_dir.join("seg-000009.tmp")).unwrap();

    let store = open(dir.path());
    assert!(!col_dir.join("seg-000009.tmp").exists());
    assert_eq!(store.view(cid).unwrap().live_len(), 1);
}

/// ランダムな 1 バイト破壊: manifest はエラーになり、WAL は prefix 復元になる。
/// どちらの場合も panic しない。
#[test]
fn single_byte_corruption_never_panics() {
    // WAL の破壊: 各バイトを反転 → open は成功し、prefix が読める
    let src = tempfile::tempdir().unwrap();
    {
        let store = open(src.path());
        let info = store.create_collection("docs", 1, Metric::L2).unwrap();
        for i in 0..4u64 {
            store
                .upsert(info.collection_id, i, rec(vec![i as f32]))
                .unwrap();
        }
    }
    let wal_rel = Path::new("wal").join(wal_file_name(1));
    let full = std::fs::read(src.path().join(&wal_rel)).unwrap();
    let work = tempfile::tempdir().unwrap();
    // 全バイトだと遅いので間引く (先頭・境界付近を含む)
    for pos in (MAGIC_WAL.len()..full.len()).step_by(7) {
        let dst = work.path().join(format!("flip-{pos}"));
        copy_dir(src.path(), &dst);
        let mut buf = full.clone();
        buf[pos] ^= 0xFF;
        std::fs::write(dst.join(&wal_rel), &buf).unwrap();
        // panic せず開けること (破壊位置以降は失われてよい)
        let store = open(&dst);
        if let Ok(info) = store.collection_info("docs") {
            let view = store.view(info.collection_id).unwrap();
            let n = view.live_len() as u64;
            for i in 0..n {
                assert_eq!(view.get(i).unwrap().vector, vec![i as f32]);
            }
        }
    }

    // manifest の破壊 → open はエラー (panic しない)
    let dst = work.path().join("manifest-flip");
    copy_dir(src.path(), &dst);
    let current = std::fs::read_to_string(dst.join("CURRENT")).unwrap();
    let mpath = dst.join(current.trim());
    let mut buf = std::fs::read(&mpath).unwrap();
    let mid = buf.len() / 2;
    buf[mid] ^= 0xFF;
    std::fs::write(&mpath, &buf).unwrap();
    assert!(Store::open(&dst, StoreOptions::default()).is_err());
}
