#!/usr/bin/env python3
"""P0 · 跨版本命令名对照表生成器。

把四个来源的命令表拉齐,输出:
  - docs/cross-version-commands.md   (人读对照表 + 差集)
  - docs/opcode/yscm-commands-494.json / -495.json / -479.json (机读,供 P1 测试夹具)

四来源与证据等级 (全部按**引擎版本号**命名,不绑定具体游戏):
  555 : 运行期 ysc.ybn (本仓库样本, Confirmed)
  494 : 运行期 ysc.ybn (发行数据实测抽取, Confirmed)
  495 : SDK 编译器侧 YSCom.ycd (证据源, Likely)
  479 : SDK 编译器侧 YSCom.ycd (证据源, Likely)

用法:
  python scripts/probe_cross_version.py            # 打印对账结果
  python scripts/probe_cross_version.py --write    # 另外写出 docs/
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DROID = Path(os.environ.get("YURIS_SIDE_LINE_ROOT", r"<side-repo>"))
ANALYSIS = DROID / "workbench" / "analysis"
V555_JSON = REPO / "docs" / "opcode" / "yscm-commands-555.json"
YCD_JSON = ANALYSIS / "ycd-commands.json"
HANDLERS_495 = ANALYSIS / "opcode_handlers.json"


def load_v555() -> tuple[int, list[str]]:
    d = json.loads(V555_JSON.read_text(encoding="utf-8"))
    return d["_meta"]["engine_version"], [c["name"] for c in d["commands"]]


def load_ycd() -> tuple[tuple[int, list[str]], tuple[int, list[str]]]:
    d = json.loads(YCD_JSON.read_text(encoding="utf-8"))
    new = d["new"]
    old = d["old_version"]
    return (
        (new["version"], [c["name"] for c in new["commands"]]),
        (old["version"], [c["name"] for c in old["commands"]]),
    )


def load_494() -> tuple[int, list[str]]:
    """引擎 494 的运行期 ysc.ybn 命令名(由 crates/yuris-tools --example tmp_yscm_dump 导出)。"""
    tsv = REPO / "docs" / "opcode" / "yscm-commands-494.tsv"
    if not tsv.exists():
        raise SystemExit(
            f"缺少 {tsv}。请先运行:\n"
            "  cargo run --release -p yuris-tools --example tmp_yscm_dump -- "
            "<data.ypf> ff ysc\n再把「全量」段的 `序号<TAB>名` 存为该文件。"
        )
    names = []
    for line in tsv.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        idx, name = line.split("\t", 1)
        assert int(idx) == len(names), f"序号不连续: 期望 {len(names)}, 实得 {idx}"
        names.append(name)
    return 494, names


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", action="store_true", help="写出 docs/ 产物")
    args = ap.parse_args()

    v555_ver, v555 = load_v555()
    (v495_ver, v495), (v479_ver, v479) = load_ycd()
    v494_ver, v494 = load_494()

    print(f"v555({v555_ver}): {len(v555)} 命令")
    print(f"v495({v495_ver}): {len(v495)} 命令")
    print(f"v494({v494_ver}): {len(v494)} 命令")
    print(f"v479({v479_ver}): {len(v479)} 命令")

    # ---- 494 vs 495:同序? ----
    if v494 == v495:
        print("\n[✓] 494(运行期 ysc.ybn) 与 495(SDK 编译器侧令表) 命令名列表**逐条同序完全相同**")
    else:
        print("\n[!] 494 与 495 列表不同:")
        for i, (a, b) in enumerate(zip(v494, v495)):
            if a != b:
                print(f"    idx {i}: 494={a}  495={b}")
        for i in range(min(len(v494), len(v495)), max(len(v494), len(v495))):
            print(f"    idx {i}: 差异 (len {len(v494)} vs {len(v495)})")

    # ---- 差集 ----
    s555, s494, s495, s479 = set(v555), set(v494), set(v495), set(v479)
    only555 = [n for n in v555 if n not in s494]
    only494 = [n for n in v494 if n not in s555]
    print(f"\n仅 v555 有的命令 ({len(only555)}): {only555}")
    print(f"仅 494/495 有的命令 ({len(only494)}): {only494}")
    print(f"共有的命令: {len(s555 & s494)}")

    # ---- 共有命令的序号漂移 ----
    drift = []
    for n in v555:
        if n in s494:
            a, b = v555.index(n), v494.index(n)
            if a != b:
                drift.append((n, a, b, b - a))
    print(f"\n共有命令中序号发生漂移的 ({len(drift)}):")
    for n, a, b, d in drift:
        print(f"    {n:<14} v555={a:<4} 494={b:<4} delta={d:+d}")

    # ---- 0.495 处理器地址 ----
    h495 = json.loads(HANDLERS_495.read_text(encoding="utf-8"))
    print(f"\n0.495 分发表: 基址 {h495['base']}, {len(h495['entries'])} 个实际覆盖槽 (其余走默认)")

    if not args.write:
        return 0

    out_op = REPO / "docs" / "opcode"
    for ver, names in ((v494_ver, v494), (v495_ver, v495), (v479_ver, v479)):
        p = out_op / f"yscm-commands-{ver}.json"
        p.write_text(
            json.dumps(
                {"_meta": {"command_count": len(names), "engine_version": ver}, "commands": [{"name": n} for n in names]},
                ensure_ascii=False,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        print(f"写出 {p.relative_to(REPO)}")

    write_markdown(v555, v494, v495, v479, only555, only494, drift, h495)
    return 0


def write_markdown(v555, v494, v495, v479, only555, only494, drift, h495) -> None:
    L: list[str] = []
    A = L.append
    A("# 跨版本命令名对照表（P0）")
    A("")
    A("> 本表是**命令名 ↔ 引擎序号**的唯一权威对照，供 `VersionProfile` 运行期分派使用。")
    A("> 生成器：`scripts/probe_cross_version.py`（可复现）。")
    A("")
    A("## 1. 数据来源与证据等级")
    A("")
    A("| 版本 | 来源 | 引擎/YSCM version | 命令数 | 证据等级 |")
    A("|---|---|---|---|---|")
    A(f"| **555** | `docs/opcode/yscm-commands-555.json`（运行期 `ysc.ybn`） | 555 | {len(v555)} | **Confirmed** |")
    A(f"| **494** | `docs/opcode/yscm-commands-494.json`（运行期 `ysc.ybn`，实测抽取） | 494 | {len(v494)} | **Confirmed** |")
    A(f"| **495** | SDK 0.495 编译器侧 `YSCom.ycd`（证据源） | 495 | {len(v495)} | Likely（编译器侧命令表；与 v494 运行期表逐条同序，见 §3） |")
    A(f"| **479** | SDK 0.479 编译器侧 `YSCom.ycd`（证据源） | 479 | {len(v479)} | Likely |")
    A("")
    A("**关键结论**：494（运行期 YSCM）与 495（SDK 编译器侧命令表）命令名列表**逐条同序完全相同** ——")
    A("说明 SDK 编译器命令序 == 运行期 YSCM 序，编译器侧表可作运行期表的等价证据。")
    A("")
    A("## 2. 跨版本差集（决定哪些语义能迁、哪些只能挂 Unknown）")
    A("")
    A(f"### 2.1 仅 555 有（{len(only555)} 条）")
    A("")
    A("```text")
    for n in only555:
        A(f"{n}  (v555 idx={v555.index(n)})")
    A("```")
    A("")
    A(f"### 2.2 仅 494/495 有（{len(only494)} 条）")
    A("")
    A("```text")
    for n in only494:
        A(f"{n}  (494 idx={v494.index(n)})")
    A("```")
    A("")
    A(f"### 2.3 两版共有的命令（{len(v555) - len(only555)} 条）中，序号发生漂移的（{len(drift)} 条）")
    A("")
    A("| 命令 | 555 序号 | 494 序号 | 漂移 |")
    A("|---|---|---|---|")
    for n, a, b, d in drift:
        A(f"| `{n}` | {a} | {b} | {d:+d} |")
    A("")
    A("**这是「不可跨版套用序号」的直接证据**：同一命令在两版序号不同，硬编码 `cmd::*` 常量必然错位。")
    A("")
    A("## 3. 全文对照")
    A("")
    A("| 命令 | v555 | 494 | 495 | 479 |")
    A("|---|---:|---:|---:|---:|")

    def cell(names: list[str], n: str) -> str:
        return str(names.index(n)) if n in set(names) else "—"

    allnames = list(dict.fromkeys(v555 + v494 + v479))
    for n in allnames:
        A(f"| `{n}` | {cell(v555, n)} | {cell(v494, n)} | {cell(v495, n)} | {cell(v479, n)} |")
    A("")
    A("## 4. SDK 0.495 分发表（证据侧，供 555 侧定位对照）")
    A("")
    A(f"- 基址 `{h495['base']}`，共 {len(h495['entries'])} 个显式覆盖槽（其余槽走默认处理函数）。")
    A("- 约定：**槽位下标 = YSCM 命令下标本身**（无偏移）。")
    A("  见 SDK 逆向文档 `docs/format-vm-opcodes.md` §1。")
    A("- 555 侧约定不同：处理器表 `0x78b020` 共 121 项，**运行期表索引 = YSCM 下标 + 8**")
    A("  （12+ 锚点吻合，见本仓库 `PROGRESS.md` 成果 30）。")
    A("")
    A("> ⚠️ 上面这条「+8 vs 无偏移」的差异本身就是跨版本差异的实证：")
    A("> **分派只能来自游戏自己的 ysc 表，不能来自任何硬编码常量表。**")
    A("")

    p = REPO / "docs" / "cross-version-commands.md"
    p.write_text("\n".join(L) + "\n", encoding="utf-8")
    print(f"写出 {p.relative_to(REPO)}")


if __name__ == "__main__":
    sys.exit(main())
