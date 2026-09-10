# 1405: in-memory モードの露出とドキュメント更新

- Status: TODO
- Milestone: M14
- Depends: 1404
- Design: docs/design/in-memory.md

## ゴール

1404 で使えるようになった in-memory モードを CLI / HTTP サーバから使える
ようにし、ドキュメントの制約記述を実態に合わせる。

## やること

- [ ] `hamane-cli`: 現状は全サブコマンドが `Database::open(&db)` 固定。
      `--in-memory` を足すかどうかを決める (単発プロセスの CLI で
      揮発 DB を使う意味が薄いなら**足さない判断でもよい**。その場合は
      理由をこの todo に書き残す)
- [ ] `hamane-server`: `--in-memory` を追加する (テスト用途・キャッシュ用途で
      意味がある)。`--replicate-from` との併用は拒否する
- [ ] `crates/hamane-py`: `Database(path=None)` は既に in-memory なので、
      オプション指定の口を足すか検討する
- [ ] `docs/spec/src/limits.md`「in-memory モードの制約」を書き直す。
      解消される項目 (索引が作られない / 量子化が効かない /
      セグメント並列検索が働かない / `StoreOptions` を渡せない /
      flush・compact が no-op) を落とし、残る制約
      (backup 不可・レプリケーション不可・メモリ上限) だけにする
- [ ] `docs/spec/src/getting-started.md` の「同じ API での in-memory 利用」から
      性能差の警告を外し、`in_memory_with_options` の例に差し替える
- [ ] `docs/spec/src/introduction.md` / `data-model.md` の 1 行言及を更新
- [ ] `docs/spec/src/configuration.md` に in-memory での設定の効き方を追記
- [ ] サンプルを 1 本追加する (`crates/hamane/examples/`)。
      揮発 DB + 量子化構成で索引が効いていることを示す
- [ ] `docs/benchmarks.md` に in-memory と tmpfs 版 `open` の比較を載せる
      (メモリ上セグメントに追加コストが無いことの確認)

## 完了条件

- [ ] `mdbook build docs/spec` が通り、リンクが解決する
- [ ] `cargo build --workspace --examples` が green (CI に入っている)
- [ ] limits.md に**実態と食い違う記述が残っていない**

## 備考

limits.md の「in-memory モードの制約」節は M14 の直前
(2026-09-10、PR #8) に追加したもの。M14 はこの節を小さくするための作業。
