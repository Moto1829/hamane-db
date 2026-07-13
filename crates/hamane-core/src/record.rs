use std::collections::BTreeMap;

/// 内部レコード ID (u64)。
pub type Id = u64;

/// 文字列 ID の内部採番が始まる値 (上位ビット)。
/// u64 ID と文字列 ID を同一 collection で混在させる場合、
/// u64 側はこの値未満を使うこと (採番との衝突を避けるため)。
pub const EXT_ID_BASE: Id = 1 << 63;

/// 文字列 ID を保持する予約メタデータキー。
/// このキーはユーザーが直接設定しないこと (文字列 ID API が自動管理する)。
pub const EXT_ID_META_KEY: &str = "_ext_id";

/// 公開 API のレコード ID。u64 (内部 ID 直接) または文字列 (外部 ID)。
///
/// 文字列 ID は collection ごとの辞書で内部 u64 に対応づけられる。
/// 対応は `_ext_id` メタデータとして永続化され、再 open 時に再構築される。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RecordId {
    /// 内部 u64 ID をそのまま使う
    Num(Id),
    /// 文字列 ID (辞書経由で内部 ID に解決される)
    Str(String),
}

impl From<u64> for RecordId {
    fn from(v: u64) -> Self {
        RecordId::Num(v)
    }
}
impl From<u32> for RecordId {
    fn from(v: u32) -> Self {
        RecordId::Num(v as u64)
    }
}
impl From<i32> for RecordId {
    /// 整数リテラル (`Record::new(1, ...)`) の利便のため。負値は不正。
    fn from(v: i32) -> Self {
        debug_assert!(v >= 0, "record id must be non-negative");
        RecordId::Num(v as u64)
    }
}
impl From<&str> for RecordId {
    fn from(v: &str) -> Self {
        RecordId::Str(v.to_owned())
    }
}
impl From<String> for RecordId {
    fn from(v: String) -> Self {
        RecordId::Str(v)
    }
}

/// メタデータの値。JSON のスカラー相当。
#[derive(Debug, Clone, PartialEq)]
pub enum MetaValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl MetaValue {
    /// 数値比較用。Int/Float を f64 に揃える。数値以外は None。
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            MetaValue::Int(v) => Some(*v as f64),
            MetaValue::Float(v) => Some(*v),
            _ => None,
        }
    }
}

impl From<&str> for MetaValue {
    fn from(v: &str) -> Self {
        MetaValue::Str(v.to_owned())
    }
}
impl From<String> for MetaValue {
    fn from(v: String) -> Self {
        MetaValue::Str(v)
    }
}
impl From<i64> for MetaValue {
    fn from(v: i64) -> Self {
        MetaValue::Int(v)
    }
}
impl From<i32> for MetaValue {
    fn from(v: i32) -> Self {
        MetaValue::Int(v as i64)
    }
}
impl From<f64> for MetaValue {
    fn from(v: f64) -> Self {
        MetaValue::Float(v)
    }
}
impl From<bool> for MetaValue {
    fn from(v: bool) -> Self {
        MetaValue::Bool(v)
    }
}

/// レコードに付随するメタデータ。
pub type Metadata = BTreeMap<String, MetaValue>;

/// 挿入・更新の単位。
#[derive(Debug, Clone)]
pub struct Record {
    /// レコード ID (u64 または文字列)
    pub id: RecordId,
    /// ベクトル本体
    pub vector: Vec<f32>,
    /// 付随メタデータ
    pub metadata: Metadata,
}

impl Record {
    /// レコードを作る。id は `u64` / `&str` / `String` を受け付ける。
    pub fn new(id: impl Into<RecordId>, vector: Vec<f32>) -> Self {
        Self {
            id: id.into(),
            vector,
            metadata: Metadata::new(),
        }
    }

    /// メタデータを 1 件追加する (ビルダー)。
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<MetaValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}
