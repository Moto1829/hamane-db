# 704: Python バインディングの CI (pytest + wheel)

- Status: DONE (2026-07-15)
- Milestone: M7
- Depends: 604
- Design: todos/604 の残項目 (maturin 未導入で pytest 未実行だった)

## ゴール

hamane-py の pytest を CI で実行し、リグレッションを検出できるようにする。
wheel のビルドが通ることも確認する。

## やること

- [ ] `.github/workflows/ci.yml` に python ジョブを追加 (ubuntu):
      actions/setup-python → pip install maturin pytest numpy →
      maturin develop --release → pytest crates/hamane-py/tests
- [ ] wheel ビルド確認: `maturin build --release` が同ジョブで通る
      (公開はしない)
- [ ] ローカルでも pytest を一度実行して green を確認
      (venv + maturin develop)

## 完了条件

- CI の python ジョブが green
- pytest 4 本がローカルでも green
