"""numpy / pandas から hamane-db を使う (todo 1302)。

実行:
    cd crates/hamane-py
    python -m venv .venv && source .venv/bin/activate
    pip install maturin pytest numpy pandas
    maturin develop --release
    python examples/numpy_pandas.py

ポイント:
- `upsert_batch(ids, matrix)` は **(n, dim) の float32 numpy 配列**を直接受け取る
  (Python のリストに変換しないぶん速い)
- `search(vector, k=...)` のクエリも numpy 配列でよい
- メタデータは dict。pandas の 1 行をそのまま渡す形にすると扱いやすい
"""

import shutil
import tempfile
import time
from pathlib import Path

import numpy as np

import hamane

DIM = 64
N = 50_000


def main() -> None:
    tmp = Path(tempfile.mkdtemp(prefix="hamane-example-py-"))
    try:
        db = hamane.Database(str(tmp / "db"))
        col = db.create_collection("docs", dim=DIM, metric="cosine")

        # 1. numpy 行列をまとめて投入する
        rng = np.random.default_rng(1302)
        # float32 で渡すのが必須 (float64 だと変換コストがかかる)
        matrix = rng.random((N, DIM), dtype=np.float32)
        ids = list(range(N))

        started = time.perf_counter()
        col.upsert_batch(ids, matrix)
        db.flush()
        print(f"{N} 件を投入: {time.perf_counter() - started:.2f}s")

        # 2. numpy 配列でそのまま検索できる
        query = matrix[42]
        started = time.perf_counter()
        hits = col.search(query, k=5)
        print(f"検索: {(time.perf_counter() - started) * 1000:.2f}ms")
        for hit in hits:
            print(f"  id={hit['id']} score={hit['score']:.4f}")
        assert hits[0]["id"] == 42

        # 3. メタデータつきで入れて絞り込む
        col2 = db.create_collection("articles", dim=DIM, metric="cosine")
        try:
            import pandas as pd
        except ImportError:
            print("\n(pandas 未インストールなので DataFrame の例は省略)")
            return

        # pandas の DataFrame から流し込む典型例。
        # ベクトル列は np.stack で (n, dim) の float32 にまとめる
        df = pd.DataFrame(
            {
                "doc_id": [f"doc-{i}" for i in range(1000)],
                "lang": np.where(np.arange(1000) % 2 == 0, "ja", "en"),
                "year": 2020 + np.arange(1000) % 5,
                "embedding": list(rng.random((1000, DIM), dtype=np.float32)),
            }
        )
        vectors = np.stack(df["embedding"].to_numpy()).astype(np.float32)

        started = time.perf_counter()
        # 文字列 ID + メタデータは 1 件ずつ (batch は id とベクトルのみ)
        for row, vector in zip(df.itertuples(index=False), vectors):
            col2.upsert(
                row.doc_id,
                vector,
                meta={"lang": row.lang, "year": int(row.year)},
            )
        db.flush()
        print(f"\nDataFrame から {len(df)} 件: {time.perf_counter() - started:.2f}s")

        hits = col2.search(vectors[3], k=5, filter={"eq": ["lang", "ja"]})
        print("lang=ja に絞った検索:")
        for hit in hits:
            print(f"  {hit['ext_id']} score={hit['score']:.4f} meta={hit['meta']}")

        # 4. 結果を DataFrame に戻す
        result = pd.DataFrame(
            [
                {"doc_id": h["ext_id"], "score": h["score"], **h["meta"]}
                for h in hits
            ]
        )
        print("\n結果の DataFrame:")
        print(result.to_string(index=False))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
