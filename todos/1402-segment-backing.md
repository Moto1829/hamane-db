# 1402: セグメント読み出しの backing 抽象化

- Status: TODO
- Milestone: M14
- Depends: 1401
- Design: docs/design/in-memory.md

## ゴール

`Segment` が mmap 以外のバイト列からも開けるようにする。**この時点では
挙動は一切変わらない** (メモリ経路の生成元はまだ無い) — 純粋な内部リファクタ。

## やること

- [ ] `MappedFile` の中身を `enum Backing { Mapped(Mmap), Owned(Vec<u8>) }` に
      置き換える (型名は `SegmentBytes` などに改名してよい)。
      公開している `content()` / `verify_checksum()` / `u64_at()` の
      シグネチャは変えない
- [ ] `Backing::Owned` を作るコンストラクタを追加し、magic 検証と
      CRC フッタの扱いを `Mapped` と揃える
- [ ] `load_pq` / `load_ivf` / `load_ivfpq` / opq のローダが `&Path` を
      取っているのを、バイト列を受け取る形に分解する
      (パス版は薄いラッパとして残す)
- [ ] `Segment::open(collection_dir, seg_id)` はそのまま維持し、
      内部で新しい構造を使う形にする
- [ ] 1401 で決めたアラインメント方針を実装する。`Owned` で
      `vectors` のデータ部が 4 バイト境界に載ることをテストで確認する

## 完了条件

- [ ] `cargo test --workspace` が green (既存テストは 1 本も変えずに通ること)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` が green
- [ ] `Owned` から開いたセグメントと、同じ内容をファイルに書いて
      `Segment::open` したセグメントで、検索結果が完全一致する単体テスト
      (量子化・索引ありの構成を含む)

## 備考

外向きの挙動が変わらないので、このタスク単体でマージできる。
1403 と並行して進めてもよいが、衝突するのは segment.rs の同じ領域なので
順に片付けるほうが早い。
