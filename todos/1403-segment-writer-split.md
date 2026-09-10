# 1403: SegmentWriter のバイト列生成と書き出しの分離

- Status: TODO
- Milestone: M14
- Depends: 1401 (1402 と同じファイルを触るので順に進めると衝突しない)
- Design: docs/design/in-memory.md

## ゴール

`SegmentWriter::write` を「セグメントのバイト列を作る」部分と
「ディレクトリへ落とす」部分に割る。索引構築 (HNSW / IVF) と量子化
(SQ8 / PQ / OPQ) は**前者に入る**ので、これによりメモリ上でも
ディスクと同じ索引付きセグメントが作れるようになる。

## 現状

`SegmentWriter::write(collection_dir, seg_id, memtable, index)` は
各ファイルを `Vec<u8>` に組み立てては `write_file_with_crc` に渡す、を
繰り返す。`.tmp` に全部書いて fsync → rename → 親ディレクトリ fsync で確定する。
つまり**バイト列は既に全部メモリ上で揃っている**。

## やること

- [ ] バイト列一式を表す型を用意する (vectors / ids / meta / tombstones と、
      任意の hnsw / sq8 / pq / ivf / ivfpq / opq)
- [ ] `SegmentWriter::build(seg_id, memtable, index) -> (バイト列一式, SegmentMeta)`
      を切り出す。索引構築・量子化学習はここに含める
- [ ] `SegmentWriter::write` は `build` の結果を `.tmp` へ書いて rename する
      だけにする。**fsync と rename の順序・親ディレクトリ fsync は現状維持**
      (クラッシュ一貫性を落とさない)
- [ ] `build` の結果から `Segment` を組み立てる経路を用意する (1402 の
      `Owned` backing を使う)
- [ ] 決定的であることを維持する: 行順は id 昇順、HNSW の seed は seg_id 由来

## 完了条件

- [ ] `cargo test --workspace` が green
- [ ] 同じ memtable から `build` → メモリ上 Segment と、
      `write` → `Segment::open` したものが、**バイト単位で一致する**テスト
      (`build_threads: 1` で決定的にした上で)
- [ ] クラッシュ耐性テスト (210) が引き続き green

## 備考

コンパクション経路も同じ `SegmentWriter::write` を通るので、ここを直せば
フラッシュとコンパクションの両方がメモリ経路に乗る。
