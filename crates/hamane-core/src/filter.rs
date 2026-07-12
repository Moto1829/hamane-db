use crate::record::{MetaValue, Metadata};

/// メタデータに対するフィルタ条件。
///
/// キーが存在しない場合、比較系の条件はすべて不成立として扱う
/// (`Not` で包んだ場合のみ成立する)。
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Eq(String, MetaValue),
    In(String, Vec<MetaValue>),
    Gt(String, MetaValue),
    Gte(String, MetaValue),
    Lt(String, MetaValue),
    Lte(String, MetaValue),
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

impl Filter {
    pub fn eq(key: impl Into<String>, value: impl Into<MetaValue>) -> Self {
        Filter::Eq(key.into(), value.into())
    }

    pub fn is_in(
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<MetaValue>>,
    ) -> Self {
        Filter::In(key.into(), values.into_iter().map(Into::into).collect())
    }

    pub fn gt(key: impl Into<String>, value: impl Into<MetaValue>) -> Self {
        Filter::Gt(key.into(), value.into())
    }

    pub fn gte(key: impl Into<String>, value: impl Into<MetaValue>) -> Self {
        Filter::Gte(key.into(), value.into())
    }

    pub fn lt(key: impl Into<String>, value: impl Into<MetaValue>) -> Self {
        Filter::Lt(key.into(), value.into())
    }

    pub fn lte(key: impl Into<String>, value: impl Into<MetaValue>) -> Self {
        Filter::Lte(key.into(), value.into())
    }

    pub fn and(filters: impl IntoIterator<Item = Filter>) -> Self {
        Filter::And(filters.into_iter().collect())
    }

    pub fn or(filters: impl IntoIterator<Item = Filter>) -> Self {
        Filter::Or(filters.into_iter().collect())
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(filter: Filter) -> Self {
        Filter::Not(Box::new(filter))
    }

    /// メタデータがこのフィルタを満たすか判定する。
    pub fn matches(&self, meta: &Metadata) -> bool {
        match self {
            Filter::Eq(key, value) => meta.get(key).is_some_and(|v| meta_eq(v, value)),
            Filter::In(key, values) => meta
                .get(key)
                .is_some_and(|v| values.iter().any(|w| meta_eq(v, w))),
            Filter::Gt(key, value) => cmp_numeric(meta, key, value, |o| o > 0.0),
            Filter::Gte(key, value) => cmp_numeric(meta, key, value, |o| o >= 0.0),
            Filter::Lt(key, value) => cmp_numeric(meta, key, value, |o| o < 0.0),
            Filter::Lte(key, value) => cmp_numeric(meta, key, value, |o| o <= 0.0),
            Filter::And(filters) => filters.iter().all(|f| f.matches(meta)),
            Filter::Or(filters) => filters.iter().any(|f| f.matches(meta)),
            Filter::Not(filter) => !filter.matches(meta),
        }
    }
}

/// Int と Float の相互比較を許した等価判定。
fn meta_eq(a: &MetaValue, b: &MetaValue) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn cmp_numeric(meta: &Metadata, key: &str, value: &MetaValue, pred: impl Fn(f64) -> bool) -> bool {
    match (meta.get(key).and_then(MetaValue::as_f64), value.as_f64()) {
        (Some(actual), Some(expected)) => pred(actual - expected),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Metadata {
        let mut m = Metadata::new();
        m.insert("lang".into(), MetaValue::Str("ja".into()));
        m.insert("year".into(), MetaValue::Int(2026));
        m.insert("score".into(), MetaValue::Float(0.5));
        m.insert("public".into(), MetaValue::Bool(true));
        m
    }

    #[test]
    fn eq_and_missing_key() {
        let m = meta();
        assert!(Filter::eq("lang", "ja").matches(&m));
        assert!(!Filter::eq("lang", "en").matches(&m));
        assert!(!Filter::eq("missing", "x").matches(&m));
        assert!(Filter::not(Filter::eq("missing", "x")).matches(&m));
    }

    #[test]
    fn numeric_comparisons_cross_type() {
        let m = meta();
        // Int フィールドを Float 値と比較できる
        assert!(Filter::gt("year", 2025.5).matches(&m));
        assert!(Filter::lte("year", 2026).matches(&m));
        assert!(!Filter::lt("year", 2026).matches(&m));
        assert!(Filter::gte("score", 0.5).matches(&m));
        // 文字列フィールドへの数値比較は不成立
        assert!(!Filter::gt("lang", 1).matches(&m));
    }

    #[test]
    fn in_and_bool() {
        let m = meta();
        assert!(Filter::is_in("lang", ["en", "ja"]).matches(&m));
        assert!(!Filter::is_in("lang", ["en", "fr"]).matches(&m));
        assert!(Filter::eq("public", true).matches(&m));
    }

    #[test]
    fn logical_combinators() {
        let m = meta();
        assert!(Filter::and([Filter::eq("lang", "ja"), Filter::gt("year", 2000)]).matches(&m));
        assert!(!Filter::and([Filter::eq("lang", "ja"), Filter::gt("year", 3000)]).matches(&m));
        assert!(Filter::or([Filter::eq("lang", "en"), Filter::eq("public", true)]).matches(&m));
        // 空の And は全件通し、空の Or は全件落とす
        assert!(Filter::and([]).matches(&m));
        assert!(!Filter::or([]).matches(&m));
    }
}
