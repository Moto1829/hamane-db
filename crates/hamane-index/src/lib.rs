//! ベクトルインデックス実装。
//!
//! - `search_flat`: 全走査の正確検索
//! - `hnsw`: 近似最近傍 (HNSW)。構築・探索・直列化

mod flat;
pub mod hnsw;

pub use flat::search_flat;
pub use hnsw::{
    search_hnsw, HnswBuilder, HnswGraph, HnswParams, HnswView, SliceSource, VectorSource,
};

use hamane_core::Id;

/// 検索結果 1 件。score はメトリック本来の値
/// (L2 は距離 = 小さいほど近い、Cosine/Dot は類似度 = 大きいほど近い)。
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: Id,
    pub score: f32,
}
