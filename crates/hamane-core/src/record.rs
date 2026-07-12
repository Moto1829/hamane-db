use std::collections::BTreeMap;

/// レコード ID。v0 では u64 固定 (文字列 ID は将来対応)。
pub type Id = u64;

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
    pub id: Id,
    pub vector: Vec<f32>,
    pub metadata: Metadata,
}

impl Record {
    pub fn new(id: Id, vector: Vec<f32>) -> Self {
        Self {
            id,
            vector,
            metadata: Metadata::new(),
        }
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<MetaValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}
