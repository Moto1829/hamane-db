# 604: Python バインディング

- Status: TODO
- Milestone: M6
- Depends: なし (601 の文字列 ID があると使い勝手が大きく向上)
- Design: —

## ゴール

埋め込みベクトル DB の主要ユーザー層 (Python / ML) から
`pip install hamane` で使えるようにする。

## やること

- [ ] `crates/hamane-py` (pyo3 + maturin)。numpy 配列 (f32) の
      ゼロコピー受け渡し (`numpy` クレートの PyReadonlyArray1/2)
- [ ] API 表面 (Rust API を素直に写像):
  ```python
  db = hamane.Database("path")        # または hamane.Database()  (in-memory)
  col = db.create_collection("docs", dim=768, metric="cosine")
  col.upsert(1, vec, meta={"lang": "ja"})
  col.upsert_batch(ids, matrix, metas)      # numpy (n, dim)
  hits = col.search(vec, k=10, ef=64, filter={"eq": ["lang", "ja"]})
  ```
- [ ] フィルタは CLI と同じ JSON 表現 (dict) を受ける
- [ ] GIL 解放 (`py.allow_threads`) を検索・upsert_batch で
- [ ] pytest による結合テスト + CI に maturin ビルドジョブ追加 (Linux/macOS)

## 完了条件

- `maturin develop` → pytest green
- numpy バッチ upsert 100k 件と検索がサンプルノートブック相当のコードで動く
- ホイールのビルドが CI で通る (公開は別判断)
