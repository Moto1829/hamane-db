//! hamane-db のコア型定義と距離計算カーネル。
//!
//! 上位クレート (`hamane-index`, `hamane-storage`, `hamane`) が共有する
//! 型 (Record, Metadata, Filter, Metric, エラー) をここに集約する。

mod error;
mod filter;
mod metric;
mod record;
pub mod sq8;

pub use error::HamaneError;
pub use filter::Filter;
pub use metric::{dot, dot_scalar, l2_squared, l2_squared_scalar, normalize, Metric};
pub use record::{Id, MetaValue, Metadata, Record, RecordId, EXT_ID_BASE, EXT_ID_META_KEY};

/// 全クレート共通の Result 型。
pub type Result<T> = std::result::Result<T, HamaneError>;
