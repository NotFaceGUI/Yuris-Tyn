#!/usr/bin/env python3
"""命令**参数名表**生成器(跨版本槽位通用化的数据来源)。

背景:YSTB 命令组的每个参数窗口 `tag` 低字节 = **该游戏 YSCM 参数表的下标**
(槽位)。槽位号逐版本漂移 —— 实测 `CG.FILE`:v555 = 槽 46、v494/495 = 槽 33、
v479 = 槽 29。故「按硬编码槽号读参数」与「按硬编码命令序号分派」是同一类错误。

本脚本把三个来源的参数名表落成 Rust 常量,供 `CommandProfile` 按**参数名**解析槽位。

来源与证据等级:
  555 : 运行期 ysc.ybn(本仓库样本, Confirmed)   -> docs/opcode/yscm-commands-555.json
  495 : SDK 编译器侧 YSCom.ycd(官方, Likely)     -> $YURIS_SIDE_LINE_ROOT/workbench/analysis/ycd-commands.json "new"
  479 : SDK 编译器侧 YSCom.ycd(官方, Likely)     -> 同上 "old_version"

494 运行期表与 SDK 495 仅 VARINFO/SYSTEMMODE 两条命令的参数数不同
(21/36 vs 19/32),其余逐条一致 —— 内置换算复用 495 表,运行期由游戏自身
YSCM 覆盖(该表只是无 YSCM 时的兜底)。

用法:
  python scripts/gen_command_params.py            # 校验并打印差异
  python scripts/gen_command_params.py --write    # 写出 crates/yuris-core/src/command_params.rs
"""
from __future__ import annotations

import argparse
import io
import json
import os
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DROID = Path(os.environ.get("YURIS_SIDE_LINE_ROOT", r"<side-repo>"))
YCD_JSON = DROID / "workbench" / "analysis" / "ycd-commands.json"
V555_JSON = REPO / "docs" / "opcode" / "yscm-commands-555.json"
OUT_RS = REPO / "crates" / "yuris-core" / "src" / "command_params.rs"


def load_555() -> tuple[int, list[tuple[str, list[str]]]]:
    d = json.loads(V555_JSON.read_text(encoding="utf-8"))
    cmds = [(c["name"], [n for n, _t in c["params"]]) for c in d["commands"]]
    return d["_meta"]["engine_version"], cmds


def load_ycd() -> tuple[tuple[int, list[tuple[str, list[str]]]], tuple[int, list[tuple[str, list[str]]]]]:
    d = json.loads(YCD_JSON.read_text(encoding="utf-8"))
    new = [(c["name"], [p["keyword"] for p in c["params"]]) for c in d["new"]["commands"]]
    old = [(c["name"], [p["keyword"] for p in c["params"]]) for c in d["old_version"]["commands"]]
    return (d["new"]["version"], new), (d["old_version"]["version"], old)


def rust_str(s: str) -> str:
    out = s.replace(chr(92), chr(92) * 2).replace('"', chr(92) + '"')
    return '"' + out + '"'


def emit(name: str, cmds: list[tuple[str, list[str]]]) -> str:
    lines = []
    for i, (cn, params) in enumerate(cmds):
        items = ", ".join(rust_str(p) for p in params)
        lines.append(f"    &[{items}], // {i} {cn}")
    body = "\n".join(lines)
    return (
        f"/// {name}(按命令序号;每项 = 该命令的参数名,下标即槽位)。\n"
        f"pub const {name}: [&[&str]; {len(cmds)}] = [\n{body}\n];\n"
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", action="store_true")
    args = ap.parse_args()

    v555, c555 = load_555()
    (v495, c495), (v479, c479) = load_ycd()
    print(f"555: version={v555} commands={len(c555)} params={sum(len(p) for _, p in c555)}")
    print(f"495: version={v495} commands={len(c495)} params={sum(len(p) for _, p in c495)}")
    print(f"479: version={v479} commands={len(c479)} params={sum(len(p) for _, p in c479)}")

    # 槽位漂移摘要(证据:FILE 是黑屏根因)
    for cmd, param in (("CG", "FILE"), ("CG", "MODE"), ("TEXT", "FILE"), ("TEXT", "CLEAR")):
        row = []
        for tag, cmds in (("555", c555), ("495", c495), ("479", c479)):
            m = dict(cmds).get(cmd, [])
            row.append(f"{tag}={m.index(param) if param in m else '-'}")
        print(f"  {cmd}.{param:9} " + "  ".join(row))

    if not args.write:
        print("(--write 未给,仅校验)")
        return 0

    header = (
        "//! 命令参数名表(**生成物,勿手改**)。\n"
        "//!\n"
        "//! 生成器:`scripts/gen_command_params.py`。\n"
        "//!\n"
        "//! 每条 = 该命令的参数名,下标即**槽位**(YSTB 参数窗口 `tag & 0xff`)。\n"
        "//! 槽位号逐版本漂移(实测 `CG.FILE`:v555 槽 46 / v495 槽 33 / v479 槽 29),\n"
        "//! 故取值必须按参数名 → 槽位解析,不得硬编码。\n"
        "//!\n"
        "//! 证据:555 = 样本运行期 `ysc.ybn`(Confirmed);495/479 = 官方 SDK\n"
        "//! `YSCom.ycd`(Likely)。运行期一律以游戏自身 YSCM 为准,本表为兜底。\n"
        "\n"
    )
    out = header + emit(f"V{v555}_COMMAND_PARAMS", c555) + "\n"
    out += emit(f"SDK_{v495}_COMMAND_PARAMS", c495) + "\n"
    out += emit(f"SDK_{v479}_COMMAND_PARAMS", c479)
    OUT_RS.write_text(out, encoding="utf-8", newline="\n")
    print(f"写出 {OUT_RS} ({len(out)} 字节)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
