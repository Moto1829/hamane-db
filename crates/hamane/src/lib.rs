//! hamane-db: 組み込み型ベクトルデータベースエンジンの公開 API。
//!
//! ```
//! use hamane::{Database, CollectionConfig, Metric, Record, Filter};
//!
//! let db = Database::in_memory();
//! let col = db
//!     .create_collection("docs", CollectionConfig { dim: 4, metric: Metric::Cosine })
//!     .unwrap();
//!
//! col.upsert(Record::new(1, vec![0.1, 0.2, 0.3, 0.4]).with_meta("lang", "ja")).unwrap();
//! col.upsert(Record::new(2, vec![0.4, 0.3, 0.2, 0.1]).with_meta("lang", "en")).unwrap();
//!
//! let hits = col
//!     .search(&[0.1, 0.2, 0.3, 0.4])
//!     .k(5)
//!     .filter(Filter::eq("lang", "ja"))
//!     .run()
//!     .unwrap();
//! assert_eq!(hits[0].id, 1);
//! ```
//!
//! 永続化するには `Database::in_memory()` の代わりに `Database::open(path)` を使う。
//! 書き込みは WAL で保護され、クラッシュ後の再 open で復元される。

mod collection;
mod database;

pub use collection::{Collection, CollectionConfig, SearchBuilder, SearchHit};
pub use database::Database;
pub use hamane_core::{Filter, HamaneError, Id, MetaValue, Metadata, Metric, Record, Result};
pub use hamane_index::HnswParams;
pub use hamane_storage::{StoreOptions, SyncPolicy};
