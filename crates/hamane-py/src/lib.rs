//! hamane-db の Python バインディング (todo 604, pyo3 + maturin)。
//!
//! ```python
//! import hamane
//! db = hamane.Database("path/to/db")      # または hamane.Database() (in-memory)
//! col = db.create_collection("docs", dim=768, metric="cosine")
//! col.upsert(1, [0.1, ...], meta={"lang": "ja"})
//! col.upsert("uuid-1", vec)                # 文字列 ID
//! col.upsert_batch(ids, matrix)            # matrix は numpy (n, dim) も可
//! hits = col.search(vec, k=10, ef=64, filter={"eq": ["lang", "ja"]})
//! ```

use std::sync::Arc;

use ::hamane as engine;

use numpy::{PyReadonlyArray1, PyReadonlyArray2};
use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};

fn to_py_err(e: engine::HamaneError) -> PyErr {
    PyValueError::new_err(e.to_string())
}

/// Python 値 → RecordId (int or str)。
fn extract_record_id(v: &Bound<'_, PyAny>) -> PyResult<engine::RecordId> {
    if let Ok(n) = v.extract::<u64>() {
        return Ok(engine::RecordId::Num(n));
    }
    if let Ok(s) = v.extract::<String>() {
        return Ok(engine::RecordId::Str(s));
    }
    Err(PyValueError::new_err(
        "id must be a non-negative int or str",
    ))
}

/// Python 値 → MetaValue。
fn extract_meta_value(v: &Bound<'_, PyAny>) -> PyResult<engine::MetaValue> {
    // bool は int のサブクラスなので先に判定する
    if let Ok(b) = v.downcast::<PyBool>() {
        return Ok(engine::MetaValue::Bool(b.is_true()));
    }
    if v.downcast::<PyInt>().is_ok() {
        return Ok(engine::MetaValue::Int(v.extract::<i64>()?));
    }
    if v.downcast::<PyFloat>().is_ok() {
        return Ok(engine::MetaValue::Float(v.extract::<f64>()?));
    }
    if let Ok(s) = v.downcast::<PyString>() {
        return Ok(engine::MetaValue::Str(s.extract::<String>()?));
    }
    Err(PyValueError::new_err(
        "meta values must be str/int/float/bool",
    ))
}

fn extract_metadata(dict: &Bound<'_, PyDict>) -> PyResult<engine::Metadata> {
    let mut meta = engine::Metadata::new();
    for (k, v) in dict.iter() {
        meta.insert(k.extract::<String>()?, extract_meta_value(&v)?);
    }
    Ok(meta)
}

fn meta_to_pydict<'py>(py: Python<'py>, meta: &engine::Metadata) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (k, v) in meta {
        match v {
            engine::MetaValue::Str(s) => dict.set_item(k, s)?,
            engine::MetaValue::Int(i) => dict.set_item(k, i)?,
            engine::MetaValue::Float(f) => dict.set_item(k, f)?,
            engine::MetaValue::Bool(b) => dict.set_item(k, b)?,
        }
    }
    Ok(dict)
}

/// dict 形式のフィルタ (CLI/HTTP と同一表現) → Filter。
/// 例: {"eq": ["lang", "ja"]}, {"and": [f1, f2]}, {"not": f}
fn extract_filter(v: &Bound<'_, PyAny>) -> PyResult<engine::Filter> {
    let dict = v
        .downcast::<PyDict>()
        .map_err(|_| PyValueError::new_err("filter must be a dict"))?;
    if dict.len() != 1 {
        return Err(PyValueError::new_err(
            "filter dict must have exactly one key",
        ));
    }
    let (op, arg) = dict.iter().next().expect("len checked");
    let op: String = op.extract()?;

    let key_value = |arg: &Bound<'_, PyAny>| -> PyResult<(String, engine::MetaValue)> {
        let pair = arg
            .downcast::<PyList>()
            .map_err(|_| PyValueError::new_err("expected [key, value]"))?;
        if pair.len() != 2 {
            return Err(PyValueError::new_err("expected [key, value]"));
        }
        Ok((
            pair.get_item(0)?.extract::<String>()?,
            extract_meta_value(&pair.get_item(1)?)?,
        ))
    };

    match op.as_str() {
        "eq" => key_value(&arg).map(|(k, v)| engine::Filter::eq(k, v)),
        "gt" => key_value(&arg).map(|(k, v)| engine::Filter::gt(k, v)),
        "gte" => key_value(&arg).map(|(k, v)| engine::Filter::gte(k, v)),
        "lt" => key_value(&arg).map(|(k, v)| engine::Filter::lt(k, v)),
        "lte" => key_value(&arg).map(|(k, v)| engine::Filter::lte(k, v)),
        "in" => {
            let pair = arg
                .downcast::<PyList>()
                .map_err(|_| PyValueError::new_err("expected [key, [values]]"))?;
            if pair.len() != 2 {
                return Err(PyValueError::new_err("expected [key, [values]]"));
            }
            let key: String = pair.get_item(0)?.extract()?;
            let values: Vec<engine::MetaValue> = pair
                .get_item(1)?
                .downcast::<PyList>()
                .map_err(|_| PyValueError::new_err("expected value list"))?
                .iter()
                .map(|item| extract_meta_value(&item))
                .collect::<PyResult<_>>()?;
            Ok(engine::Filter::is_in(key, values))
        }
        "and" | "or" => {
            let filters: Vec<engine::Filter> = arg
                .downcast::<PyList>()
                .map_err(|_| PyValueError::new_err("expected filter list"))?
                .iter()
                .map(|item| extract_filter(&item))
                .collect::<PyResult<_>>()?;
            if op == "and" {
                Ok(engine::Filter::and(filters))
            } else {
                Ok(engine::Filter::or(filters))
            }
        }
        "not" => Ok(engine::Filter::not(extract_filter(&arg)?)),
        other => Err(PyValueError::new_err(format!("unknown filter op: {other}"))),
    }
}

/// ベクトル引数: numpy 1 次元配列 (f32/f64) または list[float]。
fn extract_vector(v: &Bound<'_, PyAny>) -> PyResult<Vec<f32>> {
    if let Ok(arr) = v.extract::<PyReadonlyArray1<f32>>() {
        return Ok(arr.as_slice()?.to_vec());
    }
    if let Ok(arr) = v.extract::<PyReadonlyArray1<f64>>() {
        return Ok(arr.as_slice()?.iter().map(|&x| x as f32).collect());
    }
    v.extract::<Vec<f32>>()
        .map_err(|_| PyValueError::new_err("vector must be a list of floats or a 1-d numpy array"))
}

/// 埋め込み型ベクトルデータベース。
#[pyclass]
struct Database {
    inner: Arc<engine::Database>,
}

#[pymethods]
impl Database {
    /// Database(path=None): path 指定で永続化、省略で in-memory。
    #[new]
    #[pyo3(signature = (path=None))]
    fn new(path: Option<String>) -> PyResult<Self> {
        let inner = match path {
            Some(p) => engine::Database::open(&p).map_err(to_py_err)?,
            None => engine::Database::in_memory(),
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Collection を作成する。metric は "l2" | "cosine" | "dot" (既定 cosine)。
    #[pyo3(signature = (name, dim, metric="cosine"))]
    fn create_collection(&self, name: &str, dim: usize, metric: &str) -> PyResult<Collection> {
        let metric = match metric {
            "l2" => engine::Metric::L2,
            "cosine" => engine::Metric::Cosine,
            "dot" => engine::Metric::Dot,
            other => {
                return Err(PyValueError::new_err(format!("unknown metric: {other}")));
            }
        };
        let col = self
            .inner
            .create_collection(name, engine::CollectionConfig { dim, metric })
            .map_err(to_py_err)?;
        Ok(Collection { inner: col })
    }

    /// 既存の Collection を取得する。
    fn collection(&self, name: &str) -> PyResult<Collection> {
        let col = self
            .inner
            .collection(name)
            .map_err(|e| PyKeyError::new_err(e.to_string()))?;
        Ok(Collection { inner: col })
    }

    /// Collection 名の一覧。
    fn collection_names(&self) -> Vec<String> {
        self.inner.collection_names()
    }

    /// Collection を削除する。
    fn drop_collection(&self, name: &str) -> PyResult<()> {
        self.inner.drop_collection(name).map_err(to_py_err)
    }

    /// memtable をセグメントへ書き出す。
    fn flush(&self, py: Python<'_>) -> PyResult<()> {
        let db = Arc::clone(&self.inner);
        py.allow_threads(move || db.flush()).map_err(to_py_err)
    }

    /// セグメントを統合して上書き・削除を物理適用する。
    fn compact(&self, py: Python<'_>) -> PyResult<()> {
        let db = Arc::clone(&self.inner);
        py.allow_threads(move || db.compact()).map_err(to_py_err)
    }
}

/// ベクトルの集合 (テーブル相当)。
#[pyclass]
struct Collection {
    inner: Arc<engine::Collection>,
}

#[pymethods]
impl Collection {
    /// 1 件挿入する。id は int または str。
    #[pyo3(signature = (id, vector, meta=None))]
    fn upsert(
        &self,
        py: Python<'_>,
        id: &Bound<'_, PyAny>,
        vector: &Bound<'_, PyAny>,
        meta: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<()> {
        let rid = extract_record_id(id)?;
        let vector = extract_vector(vector)?;
        let metadata = meta.map(extract_metadata).transpose()?.unwrap_or_default();
        let mut record = engine::Record::new(rid, vector);
        record.metadata = metadata;
        let col = Arc::clone(&self.inner);
        py.allow_threads(move || col.upsert(record))
            .map_err(to_py_err)
    }

    /// 一括挿入する。vectors は numpy (n, dim) または list[list[float]]。
    /// metas は省略可 (指定時は ids と同じ長さの dict のリスト)。
    #[pyo3(signature = (ids, vectors, metas=None))]
    fn upsert_batch(
        &self,
        py: Python<'_>,
        ids: &Bound<'_, PyList>,
        vectors: &Bound<'_, PyAny>,
        metas: Option<&Bound<'_, PyList>>,
    ) -> PyResult<()> {
        let rids: Vec<engine::RecordId> = ids
            .iter()
            .map(|item| extract_record_id(&item))
            .collect::<PyResult<_>>()?;

        // numpy (n, dim) はゼロコピーで読み、行ごとに Vec 化する
        let rows: Vec<Vec<f32>> = if let Ok(arr) = vectors.extract::<PyReadonlyArray2<f32>>() {
            let view = arr.as_array();
            view.outer_iter().map(|row| row.to_vec()).collect()
        } else if let Ok(arr) = vectors.extract::<PyReadonlyArray2<f64>>() {
            let view = arr.as_array();
            view.outer_iter()
                .map(|row| row.iter().map(|&x| x as f32).collect())
                .collect()
        } else {
            vectors.extract::<Vec<Vec<f32>>>().map_err(|_| {
                PyValueError::new_err("vectors must be a 2-d numpy array or list of lists")
            })?
        };

        if rids.len() != rows.len() {
            return Err(PyValueError::new_err(format!(
                "ids ({}) and vectors ({}) must have the same length",
                rids.len(),
                rows.len()
            )));
        }
        let metadatas: Vec<engine::Metadata> = match metas {
            Some(list) => {
                if list.len() != rids.len() {
                    return Err(PyValueError::new_err(
                        "metas must have the same length as ids",
                    ));
                }
                list.iter()
                    .map(|item| {
                        item.downcast::<PyDict>()
                            .map_err(|_| PyValueError::new_err("meta must be a dict"))
                            .and_then(|d| extract_metadata(d))
                    })
                    .collect::<PyResult<_>>()?
            }
            None => vec![engine::Metadata::new(); rids.len()],
        };

        let records: Vec<engine::Record> = rids
            .into_iter()
            .zip(rows)
            .zip(metadatas)
            .map(|((rid, vector), metadata)| {
                let mut r = engine::Record::new(rid, vector);
                r.metadata = metadata;
                r
            })
            .collect();
        let col = Arc::clone(&self.inner);
        py.allow_threads(move || col.upsert_batch(records))
            .map_err(to_py_err)
    }

    /// 近傍検索。結果は [{"id", "ext_id", "score", "meta"}] のリスト。
    #[pyo3(signature = (vector, k=10, ef=None, filter=None))]
    fn search(
        &self,
        py: Python<'_>,
        vector: &Bound<'_, PyAny>,
        k: usize,
        ef: Option<usize>,
        filter: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Vec<Py<PyDict>>> {
        let query = extract_vector(vector)?;
        let filter = filter.map(extract_filter).transpose()?;
        let col = Arc::clone(&self.inner);
        let hits = py
            .allow_threads(move || {
                let mut builder = col.search(&query).k(k);
                if let Some(ef) = ef {
                    builder = builder.ef(ef);
                }
                if let Some(f) = filter {
                    builder = builder.filter(f);
                }
                builder.run()
            })
            .map_err(to_py_err)?;
        hits.iter()
            .map(|h| {
                let d = PyDict::new(py);
                d.set_item("id", h.id)?;
                d.set_item("ext_id", h.ext_id())?;
                d.set_item("score", h.score)?;
                d.set_item("meta", meta_to_pydict(py, &h.metadata)?)?;
                Ok(d.unbind())
            })
            .collect()
    }

    /// 点参照。見つからなければ None。
    fn get<'py>(
        &self,
        py: Python<'py>,
        id: &Bound<'_, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let rid = extract_record_id(id)?;
        match self.inner.get(rid) {
            Some(rec) => {
                let d = PyDict::new(py);
                d.set_item("vector", rec.vector)?;
                d.set_item("meta", meta_to_pydict(py, &rec.metadata)?)?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    /// 削除。存在した場合 True。
    fn delete(&self, py: Python<'_>, id: &Bound<'_, PyAny>) -> PyResult<bool> {
        let rid = extract_record_id(id)?;
        let col = Arc::clone(&self.inner);
        py.allow_threads(move || col.delete(rid)).map_err(to_py_err)
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }
}

/// hamane Python モジュール。
#[pymodule]
fn hamane(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Database>()?;
    m.add_class::<Collection>()?;
    Ok(())
}
