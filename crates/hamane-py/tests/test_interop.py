# CLI / ライブラリと Python バインディングの相互運用 E2E (todo 1301 C)。
#
# Rust の CLI (hamane) で作った DB を Python から読み、Python で書いた DB を
# CLI から読めることを確認する。ここが壊れると「Rust で入れたデータが
# Python から見えない」類の事故になる。
#
# CLI バイナリの場所は環境変数 HAMANE_CLI で渡す。未設定なら
# target/{release,debug}/hamane を探し、見つからなければ skip する
# (バインディングだけをテストしたい人の邪魔をしない)。
#
# 実行方法:
#   cargo build -p hamane-cli
#   cd crates/hamane-py && pytest tests/ -q

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

import hamane

REPO_ROOT = Path(__file__).resolve().parents[3]


def find_cli():
    """CLI バイナリを探す (環境変数 → target/release → target/debug → PATH)。"""
    env = os.environ.get("HAMANE_CLI")
    if env and Path(env).exists():
        return env
    for profile in ("release", "debug"):
        candidate = REPO_ROOT / "target" / profile / "hamane"
        if candidate.exists():
            return str(candidate)
    return shutil.which("hamane")


CLI = find_cli()
requires_cli = pytest.mark.skipif(
    CLI is None,
    reason="hamane CLI が見つからない (cargo build -p hamane-cli で作るか HAMANE_CLI を設定)",
)


def run_cli(*args, stdin=None):
    result = subprocess.run(
        [CLI, *args],
        input=stdin,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, f"cli {args} failed: {result.stderr}"
    return result.stdout


@requires_cli
def test_cli_written_db_is_readable_from_python(tmp_path):
    db_path = str(tmp_path / "db")
    run_cli("create", db_path, "docs", "--dim", "4", "--metric", "l2")

    lines = []
    for i in range(50):
        vector = [float(i), i + 0.1, i + 0.2, i + 0.3]
        lines.append(
            json.dumps({"id": i, "vector": vector, "meta": {"lang": "ja" if i % 2 == 0 else "en"}})
        )
    run_cli("insert", db_path, "docs", stdin="\n".join(lines))
    run_cli("flush", db_path)

    # Python から開く
    db = hamane.Database(db_path)
    col = db.collection("docs")
    assert len(col) == 50

    rec = col.get(7)
    # 保存は f32 なので Python の float (f64) と厳密比較はできない
    assert rec["vector"] == pytest.approx([7.0, 7.1, 7.2, 7.3], rel=1e-6)
    assert rec["meta"]["lang"] == "en"

    hits = col.search([7.0, 7.1, 7.2, 7.3], k=3)
    assert hits[0]["id"] == 7
    assert hits[0]["score"] < 1e-3

    # メタデータフィルタも同じ意味で効く
    hits = col.search([7.0, 7.1, 7.2, 7.3], k=5, filter={"eq": ["lang", "ja"]})
    assert all(h["meta"]["lang"] == "ja" for h in hits)


@requires_cli
def test_python_written_db_is_readable_from_cli(tmp_path):
    db_path = str(tmp_path / "db")
    db = hamane.Database(db_path)
    col = db.create_collection("docs", dim=4, metric="l2")
    for i in range(30):
        col.upsert(i, [float(i), i + 0.1, i + 0.2, i + 0.3], meta={"lang": "ja"})
    db.flush()
    del col
    del db  # ロックを解放してから CLI を起動する

    info = json.loads(run_cli("info", db_path))
    assert info["collections"][0]["len"] == 30

    out = json.loads(
        run_cli("search", db_path, "docs", "--vector", "[10.0,10.1,10.2,10.3]", "--k", "3")
    )
    assert out["hits"][0]["id"] == 10


@requires_cli
def test_cli_backup_is_readable_from_python(tmp_path):
    db_path = str(tmp_path / "db")
    backup_path = str(tmp_path / "backup")
    run_cli("create", db_path, "docs", "--dim", "2", "--metric", "l2")
    lines = [json.dumps({"id": i, "vector": [float(i), 0.0]}) for i in range(20)]
    run_cli("insert", db_path, "docs", stdin="\n".join(lines))
    run_cli("backup", db_path, backup_path)

    db = hamane.Database(backup_path)
    col = db.collection("docs")
    assert len(col) == 20
    assert col.get(5)["vector"] == pytest.approx([5.0, 0.0])
