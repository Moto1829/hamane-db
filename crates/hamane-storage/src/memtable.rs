//! インメモリの書き込みバッファ (docs/design/storage.md §6)。
//!
//! WAL 反映済みの upsert と削除マーカーを保持し、閾値超過でセグメントへ
//! フラッシュされる。削除マーカーは自分より古いセグメントの行を無効化する
//! tombstone としてフラッシュ時に書き出される。

use std::collections::{HashMap, HashSet};

use hamane_core::{Id, Metadata};

/// 正規化・検証済みのレコード本体。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    pub vector: Vec<f32>,
    pub metadata: Metadata,
}

impl StoredRecord {
    fn approx_bytes(&self) -> usize {
        let meta: usize = self
            .metadata
            .iter()
            .map(|(k, v)| {
                k.len()
                    + match v {
                        hamane_core::MetaValue::Str(s) => s.len(),
                        _ => 8,
                    }
            })
            .sum();
        self.vector.len() * 4 + meta
    }
}

/// v0 のスナップショットは clone (docs/design/query.md §1)。
pub type MemtableSnapshot = Memtable;

#[derive(Debug, Clone, Default)]
pub struct Memtable {
    upserts: HashMap<Id, StoredRecord>,
    deletes: HashSet<Id>,
    bytes: usize,
}

impl Memtable {
    pub fn new() -> Self {
        Self::default()
    }

    /// upsert を反映する。同 id の削除マーカーは打ち消される。
    pub fn upsert(&mut self, id: Id, record: StoredRecord) {
        self.deletes.remove(&id);
        self.bytes += record.approx_bytes();
        if let Some(old) = self.upserts.insert(id, record) {
            self.bytes -= old.approx_bytes();
        }
    }

    /// 削除を反映する。id が古いセグメントに存在するかは確認しない
    /// (tombstone が空振りしても無害)。
    pub fn delete(&mut self, id: Id) {
        if let Some(old) = self.upserts.remove(&id) {
            self.bytes -= old.approx_bytes();
        }
        self.deletes.insert(id);
    }

    pub fn get(&self, id: Id) -> Option<&StoredRecord> {
        self.upserts.get(&id)
    }

    /// この memtable 内で削除マーカーが立っているか。
    pub fn is_deleted(&self, id: Id) -> bool {
        self.deletes.contains(&id)
    }

    /// upsert 済みレコード数 (削除マーカーは数えない)。
    pub fn len(&self) -> usize {
        self.upserts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.deletes.is_empty()
    }

    /// ベクトル + メタデータの概算バイト数 (フラッシュ閾値判定用)。
    pub fn approx_bytes(&self) -> usize {
        self.bytes
    }

    /// 検索用走査。`search_flat` にそのまま渡せる形。
    pub fn iter(&self) -> impl Iterator<Item = (Id, &[f32], &Metadata)> {
        self.upserts
            .iter()
            .map(|(id, r)| (*id, r.vector.as_slice(), &r.metadata))
    }

    /// 削除マーカーの走査 (フラッシュ時の tombstone 書き出し用)。
    pub fn deletes(&self) -> impl Iterator<Item = Id> + '_ {
        self.deletes.iter().copied()
    }

    /// スナップショットを取得する (v0 は clone)。
    pub fn snapshot(&self) -> MemtableSnapshot {
        self.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(dim: usize) -> StoredRecord {
        StoredRecord {
            vector: vec![1.0; dim],
            metadata: Metadata::new(),
        }
    }

    #[test]
    fn upsert_delete_transitions() {
        let mut mt = Memtable::new();
        mt.upsert(1, rec(4));
        assert!(mt.get(1).is_some());
        assert!(!mt.is_deleted(1));

        mt.delete(1);
        assert!(mt.get(1).is_none());
        assert!(mt.is_deleted(1));
        assert_eq!(mt.len(), 0);

        // 再 upsert で削除マーカーが打ち消される
        mt.upsert(1, rec(4));
        assert!(mt.get(1).is_some());
        assert!(!mt.is_deleted(1));
        assert_eq!(mt.deletes().count(), 0);
    }

    #[test]
    fn delete_without_upsert_leaves_tombstone() {
        let mut mt = Memtable::new();
        mt.delete(99); // 古いセグメントにあるかもしれない id
        assert!(mt.is_deleted(99));
        assert_eq!(mt.deletes().collect::<Vec<_>>(), vec![99]);
        assert!(!mt.is_empty()); // tombstone だけでもフラッシュ対象
    }

    #[test]
    fn bytes_accounting() {
        let mut mt = Memtable::new();
        assert_eq!(mt.approx_bytes(), 0);
        mt.upsert(1, rec(100)); // 400 バイト
        assert_eq!(mt.approx_bytes(), 400);
        mt.upsert(1, rec(10)); // 置き換えで 40 バイトに
        assert_eq!(mt.approx_bytes(), 40);
        mt.delete(1);
        assert_eq!(mt.approx_bytes(), 0);
    }
}
