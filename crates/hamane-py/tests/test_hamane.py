# hamane Python バインディングの結合テスト (todo 604)。
#
# 実行方法:
#   cd crates/hamane-py
#   python -m venv .venv && source .venv/bin/activate
#   pip install maturin pytest numpy
#   maturin develop --release
#   pytest tests/

import numpy as np
import pytest

import hamane


def test_crud_roundtrip(tmp_path):
    db = hamane.Database(str(tmp_path / "db"))
    col = db.create_collection("docs", dim=4, metric="l2")

    col.upsert(1, [1.0, 0.0, 0.0, 0.0], meta={"lang": "ja", "year": 2026})
    col.upsert("doc-x", [0.0, 1.0, 0.0, 0.0])
    assert len(col) == 2

    rec = col.get(1)
    assert rec["vector"] == [1.0, 0.0, 0.0, 0.0]
    assert rec["meta"]["lang"] == "ja"
    assert col.get("doc-x") is not None
    assert col.get("missing") is None

    hits = col.search([1.0, 0.0, 0.0, 0.0], k=1)
    assert hits[0]["id"] == 1
    hits = col.search([0.0, 1.0, 0.0, 0.0], k=1)
    assert hits[0]["ext_id"] == "doc-x"

    assert col.delete(1) is True
    assert col.delete(1) is False
    assert len(col) == 1


def test_numpy_batch_and_search(tmp_path):
    db = hamane.Database(str(tmp_path / "db"))
    col = db.create_collection("vecs", dim=8, metric="l2")

    rng = np.random.default_rng(604)
    matrix = rng.random((100, 8), dtype=np.float32)
    ids = list(range(100))
    col.upsert_batch(ids, matrix)
    assert len(col) == 100

    db.flush()

    # numpy クエリで検索。自分自身が最近傍
    hits = col.search(matrix[7], k=1)
    assert hits[0]["id"] == 7
    assert hits[0]["score"] < 1e-5


def test_filter_and_metric(tmp_path):
    db = hamane.Database()  # in-memory
    col = db.create_collection("docs", dim=2)  # cosine (default)

    col.upsert(1, [1.0, 0.0], meta={"even": False})
    col.upsert(2, [0.9, 0.1], meta={"even": True})
    hits = col.search([1.0, 0.0], k=10, filter={"eq": ["even", True]})
    assert [h["id"] for h in hits] == [2]

    with pytest.raises(ValueError):
        col.search([1.0, 0.0], filter={"bogus": []})
    with pytest.raises(ValueError):
        col.upsert(3, [1.0])  # dimension mismatch


def test_persistence(tmp_path):
    path = str(tmp_path / "db")
    db = hamane.Database(path)
    col = db.create_collection("docs", dim=2, metric="l2")
    col.upsert("persist-me", [1.0, 2.0])
    db.flush()
    del col
    del db

    db = hamane.Database(path)
    col = db.collection("docs")
    assert col.get("persist-me")["vector"] == [1.0, 2.0]


def test_batch_search_and_threshold(tmp_path):
    """バッチ検索 (todo 1501) と閾値 (todo 1502)。"""
    db = hamane.Database(str(tmp_path / "db"))
    col = db.create_collection("vecs", dim=4, metric="l2")

    rng = np.random.default_rng(1501)
    matrix = rng.random((200, 4), dtype=np.float32)
    col.upsert_batch(list(range(200)), matrix)
    db.flush()

    # numpy 行列をそのまま渡せる。結果は入力と同じ順
    queries = matrix[[3, 50, 199]]
    results = col.search_batch(queries, k=2)
    assert len(results) == 3
    assert [r[0]["id"] for r in results] == [3, 50, 199]

    # 1 件ずつ検索した結果と完全一致
    for q, got in zip(queries, results):
        one = col.search(q, k=2)
        assert [h["id"] for h in got] == [h["id"] for h in one]
        assert got[0]["score"] == pytest.approx(one[0]["score"])

    # リストのリストでも渡せる
    as_list = col.search_batch([list(map(float, queries[0]))], k=1)
    assert as_list[0][0]["id"] == 3

    # 閾値: l2 は距離がこれ以下だけ (自分自身は距離 0 なので必ず残る)
    hits = col.search(matrix[7], k=10, threshold=0.0)
    assert [h["id"] for h in hits] == [7]

    # 誰も満たさない閾値なら空 (エラーにはならない)
    assert col.search(matrix[7], k=10, threshold=-1.0) == []
    assert col.search_batch(queries, k=5, threshold=-1.0) == [[], [], []]

    # 空のクエリ列
    assert col.search_batch([], k=5) == []
