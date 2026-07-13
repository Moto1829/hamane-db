//! hamane-db の永続化層。
//!
//! WAL・不変セグメント・manifest によるクラッシュ一貫性のある永続化を提供する。
//! オンディスクフォーマットの仕様は docs/design/storage.md を参照。

pub mod format;
pub mod manifest;
pub mod memtable;
pub mod segment;
pub mod store;
pub mod wal;

pub use manifest::{CollectionEntry, Manifest, SegmentEntry};
pub use memtable::{Memtable, MemtableSnapshot, StoredRecord};
pub use segment::{Segment, SegmentMeta, SegmentWriter, Sq8View};
pub use store::{CollectionInfo, LiveView, SegmentStats, Store, StoreOptions};
pub use wal::{SyncPolicy, WalReader, WalRecord, WalWriter};
