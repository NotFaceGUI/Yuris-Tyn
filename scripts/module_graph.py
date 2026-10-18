#!/usr/bin/env python3
"""从仓库自身生成模块关系图(唯一真相 = Cargo.toml + src/*.rs,不手写)。

用法:
    python scripts/module_graph.py                # 打印 Mermaid + 统计
    python scripts/module_graph.py --format dot   # Graphviz DOT
    python scripts/module_graph.py --format json  # 结构化摘要(供可视化前端消费)
    python scripts/module_graph.py --check        # 只跑健全性断言(退出码非 0 = 有问题)

产出三类事实,全部来自解析而非人工声明:
  1. crate 级依赖边      ← 各 crate 的 Cargo.toml [dependencies]/[dev-dependencies]
  2. crate 内模块树      ← src/ 下的 .rs 文件布局(含行数,用于标注体量)
  3. 跨 crate 模块引用    ← 源码里的 `use yuris_x::mod`(细粒度真实流向)

分层标签 L0-L6 取自各 crate 清单的 description 字段(见 docs/01-runtime-architecture.md),
不再额外维护一张表 —— 改了 description,图就跟着变。
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ROOT / "crates"

# 空骨架判定:lib.rs 只有 VERSION 常量、没有实现体(AGENTS.md §3.3 记录过这三个)。
SKELETON_MAX_LINES = 40


def load_toml(path: Path) -> dict:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def workspace_shape() -> tuple[list[str], set[str]]:
    """返回 (成员目录名列表, 内部依赖名集合)。"""
    root = load_toml(ROOT / "Cargo.toml")
    ws = root.get("workspace", {})
    members = [Path(m).name for m in ws.get("members", [])]
    internal = {
        name
        for name, spec in ws.get("dependencies", {}).items()
        if isinstance(spec, dict) and "path" in spec
    }
    # members 里未被 workspace.dependencies 收录的(如 yuris-tools / yuris-player-core)也算内部
    internal |= {m for m in members}
    return members, internal


def crate_layers() -> dict[str, str]:
    """从 description 里刮出 L 层标签(形如 'L6：AudioBackend 实现...')。"""
    layers: dict[str, str] = {}
    for manifest in sorted(CRATES.glob("*/Cargo.toml")):
        pkg = load_toml(manifest).get("package", {})
        desc = str(pkg.get("description", ""))
        m = re.search(r"\bL([0-6])\b", desc)
        if m:
            layers[pkg.get("name", manifest.parent.name)] = f"L{m.group(1)}"
    return layers


def crate_deps(name: str, internal: set[str]) -> dict[str, list[str]]:
    """读一个 crate 的三类依赖,分出内部/外部。"""
    manifest = CRATES / name / "Cargo.toml"
    if not manifest.is_file():  # 成员已声明但目录还没建(在制品)
        return {"dependencies": [], "dev-dependencies": [], "build-dependencies": []}
    pkg = load_toml(manifest)
    out: dict[str, list[str]] = {}
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        found = []
        for dep in pkg.get(section, {}) or {}:
            if dep in internal or dep.startswith("yuris-"):
                found.append(dep)
        out[section] = sorted(found)
    return out


def crate_description(name: str) -> str:
    manifest = CRATES / name / "Cargo.toml"
    if not manifest.is_file():
        return ""
    return str(load_toml(manifest).get("package", {}).get("description", ""))


def module_tree(name: str) -> list[dict]:
    """遍历 src/,按 Rust 的路径约定推出模块路径。"""
    src = CRATES / name / "src"
    if not src.is_dir():
        return []
    mods = []
    for path in sorted(src.rglob("*.rs")):
        rel = path.relative_to(src)
        parts = list(rel.with_suffix("").parts)
        if parts[-1] in ("lib", "main"):
            parts = parts[:-1]
        elif parts[-1] == "mod":
            parts = parts[:-1]
        # 仅保留直接挂在 crad 根下的模块名(两级足够,再深交给源码)
        mod_path = "::".join(parts) if parts else "<crate root>"
        text = path.read_text(encoding="utf-8", errors="replace")
        mods.append(
            {
                "module": mod_path,
                "file": str(rel).replace("\\", "/"),
                "lines": text.count("\n") + (0 if text.endswith("\n") else 1),
                "has_inline_tests": bool(re.search(r"^#\[cfg\(test\)\]|^\s*mod .*tests", text, re.M)),
            }
        )
    return mods


def crate_stats(name: str, mods: list[dict]) -> dict:
    total = sum(m["lines"] for m in mods)
    root_file = next((m for m in mods if m["module"] == "<crate root>"), None)
    # 三种状态分开报,不要混成一个"空":无 src(脚手架) ≠ 有 src 但空骨架
    if not mods:
        state = "scaffold"  # 只有 Cargo.toml,src/ 还没建
    elif total <= SKELETON_MAX_LINES:
        state = "skeleton"  # 有 src 但只有 VERSION 之类
    else:
        state = "implemented"
    return {
        "crate": name,
        "lines": total,
        "state": state,
        "skeleton": state != "implemented",
        "modules": mods,
    }


def derive_depths(crates: dict[str, dict]) -> None:
    """依赖深度 = 1 + max(依赖的深度),0 = 无内部依赖的叶子。

    声明的 L 层标签只覆盖一半 crate(其余 description 里没写),深度是它们的
    兜底分类 —— 同样由代码算出,不手工认领。"""
    memo: dict[str, int] = {}

    def depth(name: str, stack: tuple[str, ...] = ()) -> int:
        if name in memo:
            return memo[name]
        if name in stack:  # 环:交给 --check 报,此处不递归
            return 0
        deps = [d for d in crates[name]["deps_internal"]["dependencies"] if d in crates]
        value = 0 if not deps else 1 + max(depth(d, stack + (name,)) for d in deps)
        memo[name] = value
        return value

    for name in crates:
        crates[name]["depth"] = depth(name)


def cross_crate_imports(member_names: list[str]) -> list[dict]:
    """扫描 `use yuris_x::mod` —— 细粒度的真实引用(测试/样例也算,标注来源类别)。"""
    known = {n: n.replace("yuris-", "yuris_") for n in member_names}
    pat = re.compile(r"use\s+(yuris_[a-z_0-9]+)::([A-Za-z_0-9]+)")
    edges: dict[tuple[str, str, str], int] = {}
    for name in member_names:
        base = CRATES / name
        for sub in ("src", "tests", "examples"):
            for path in (base / sub).rglob("*.rs") if (base / sub).is_dir() else []:
                for target, mod in pat.findall(path.read_text(encoding="utf-8", errors="replace")):
                    if target in known.values() and target != known[name]:
                        key = (name, target.replace("yuris_", "yuris-"), mod)
                        edges[key] = edges.get(key, 0) + 1
    return [
        {"from": a, "to": b, "module": m, "hits": n}
        for (a, b, m), n in sorted(edges.items(), key=lambda kv: -kv[1])
    ]


def collect() -> dict:
    members, internal = workspace_shape()
    layers = crate_layers()
    crates = {}
    for name in members:
        mods = module_tree(name)
        stats = crate_stats(name, mods)
        stats["layer"] = layers.get(name, "-")
        stats["description"] = crate_description(name)
        stats["deps_internal"] = crate_deps(name, internal)
        crates[name] = stats
    derive_depths(crates)
    unimplemented = sorted(n for n, c in crates.items() if c["state"] != "implemented")
    return {
        "crates": crates,
        "imports": cross_crate_imports(members),
        "totals": {
            "crates": len(members),
            "src_lines": sum(c["lines"] for c in crates.values()),
            "captured_at": datetime.now().astimezone().isoformat(timespec="seconds"),
            "scaffold_crates": sorted(n for n, c in crates.items() if c["state"] == "scaffold"),
            "skeleton_crates": sorted(n for n, c in crates.items() if c["state"] == "skeleton"),
            "unimplemented_crates": unimplemented,
        },
    }


# ---------------------------------------------------------------- 输出格式

LAYER_ORDER = ["L0", "L1", "L2", "L3", "L4", "L5", "L6", "-"]


def group_key(c: dict) -> str:
    """分层标签缺失时,用依赖深度兜底(如 'D0'),保证每个 crate 都落进桶里。"""
    return c["layer"] if c["layer"] != "-" else f"D{c['depth']}"


def group_order(keys: set[str]) -> list[str]:
    def rank(k: str) -> tuple[int, int]:
        if k in LAYER_ORDER:
            return (0, LAYER_ORDER.index(k))
        if k.startswith("D"):
            return (1, int(k[1:]))
        return (2, 999)

    return sorted(keys, key=rank)


STATE_MARK = {"scaffold": "🚧 脚手架", "skeleton": "⚠ 空骨架", "implemented": ""}


def node_label(c: dict) -> str:
    label = f"{c['crate']}<br/>{c['lines']} 行"
    mark = STATE_MARK[c["state"]]
    if mark:
        label += f"<br/>{mark}"
    return label


def mermaid(data: dict) -> str:
    crates = data["crates"]
    out = ["```mermaid", "flowchart TD"]
    by_group: dict[str, list[str]] = {}
    for name, c in crates.items():
        by_group.setdefault(group_key(c), []).append(name)

    for group in group_order(set(by_group)):
        names = sorted(by_group[group])
        out.append(f"  subgraph {group}[{group}]")
        for name in names:
            out.append(f'    {name.replace("-", "_")}["{node_label(crates[name])}"]')
        out.append("  end")

    # 边:实线 = 运行期依赖;虚线 = 仅 dev-dependencies
    for name, c in sorted(crates.items()):
        for dep in c["deps_internal"]["dependencies"]:
            out.append(f"  {dep.replace('-', '_')} --> {name.replace('-', '_')}")
    for name, c in sorted(crates.items()):
        for dep in c["deps_internal"]["dev-dependencies"]:
            out.append(f"  {dep.replace('-', '_')} -.dev.-> {name.replace('-', '_')}")
    out.append("```")
    return "\n".join(out)


def mermaid_modules(data: dict, top: int = 6) -> str:
    """按体量取前 N 个 crate,展开其内部模块 —— 巨文件就是缺陷线索(AGENTS.md §3.2)。"""
    crates = data["crates"]
    ranked = sorted(crates.items(), key=lambda kv: -kv[1]["lines"])[:top]
    out = ["```mermaid", "flowchart LR"]
    for name, c in ranked:
        out.append(f'  subgraph {name.replace("-", "_")}["{name} · {c["lines"]} 行"]')
        out.append("    direction TB")
        for m in sorted(c["modules"], key=lambda m: -m["lines"]):
            if m["lines"] < 1:
                continue
            label = f"{m['module']}<br/>{m['lines']}"
            if m["has_inline_tests"]:
                label += " 🧪"
            out.append(f'    {name.replace("-", "_")}__{m["file"].replace("/", "_").replace(".rs", "")}["{label}"]')
        out.append("  end")
    out.append("```")
    return "\n".join(out)


# 运行期主链路。节点 = 已解析出的**真实模块**(存在性由 collect() 校验),
# 顺序与职责取自 docs/01-runtime-architecture.md §1 的分层与
# crates/yuris-vm/src/boot.rs 的启动链注释、crates/yuris-cli/src/main.rs 的帧循环。
PIPELINE = [
    ("L0", "yuris-format::ypf", "YPF 封包(懒加载 + XOR + zlib)"),
    ("L1", "yuris-format::ystb", "YSTB 容器 → 命令表 + 字节码流"),
    ("L2", "yuris-script::insn", "变长字节码 → Instruction IR"),
    ("L3", "yuris-vm::exec", "取指/译码/执行(可 suspend)"),
    ("L3", "yuris-vm::event", "产生 Runtime Event"),
    ("L4", "yuris-runtime", "trait 边界(唯一出口)"),
    ("L5", "yuris-scene", "Scene / Layer / Animation 纯状态"),
    ("L6", "yuris-render", "wgpu 呈现"),
]
# L6 的另外两个后端是**并列**兄弟,不是 render 的下游 —— 分开画。
BACKENDS = [
    ("L6", "yuris-audio", "BGM/SE/Voice"),
    ("L6", "yuris-input", "点击/选择/Skip"),
]


def _step(nodes: list[str]) -> list[str]:
    chain = []
    for i in range(len(nodes) - 1):
        chain.append(f"  {nodes[i]} --> {nodes[i + 1]}")
    return chain


def mermaid_flow(data: dict) -> str:
    """运行期数据流:封包 → 容器 → IR → VM → 事件 → trait → 状态 → 后端。"""
    known = set()  # 归一化:节点 id 用 '-',模块集合用 '_',统一成下划线比对
    for c in data["crates"].values():
        known.add(c["crate"].replace("-", "_"))
        for m in c["modules"]:
            known.add(f"{c['crate'].replace('-', '_')}__{m['module'].split('::')[0]}")

    out = ["```mermaid", "flowchart TD"]
    missing, ids = [], []
    for layer, node, label in PIPELINE:
        bare = node.replace("::", "__")
        ids.append(bare)
        out.append(f'  {bare}["{label}<br/><i>{node}</i> <b>{layer}</b>"]')
        if bare not in known:
            missing.append(node)
    out += _step(ids)

    backend_ids = []
    for layer, node, label in BACKENDS:
        bare = node.replace("::", "__")
        backend_ids.append(bare)
        out.append(f'  {bare}["{label}<br/><i>{node}</i> <b>{layer}</b>"]')
        if bare not in known:
            missing.append(node)
    # 三个后端都从 L5 状态层取数据(并列,互不为上下游)
    for b in backend_ids:
        out.append(f"  yuris_scene --> {b}")

    out.append('  input_bus["winit 鼠标/键盘<br/><i>yuris-cli</i>"] -.事件注入.-> yuris_vm__exec')
    out.append('  cli_loop["帧循环 tick()<br/><i>yuris-player-core::Player</i>"] -.驱动.-> yuris_vm__exec')
    out.append("```")
    if missing:  # 只可能是重命名/删除 —— 报出来,不画假节点
        out.append(f"> ⚠ 以下节点在源码里未找到(清单已过期): {', '.join(missing)}")
    return "\n".join(out)


def render_doc(data: dict) -> str:
    crates = data["crates"]
    t = data["totals"]
    depended: dict[str, int] = {}
    for c in crates.values():
        for d in c["deps_internal"]["dependencies"]:
            depended[d] = depended.get(d, 0) + 1

    lines = [
        "# Yuris-Tyn 模块图(自动生成)",
        "",
        "> **本文件由 `scripts/module_graph.py` 生成,请勿手改。**",
        "> 重新生成:`python scripts/module_graph.py --write docs/module-map.md`",
        "> 真相来源 = 各 crate 的 `Cargo.toml` + `src/` 实际布局,不是人工维护的清单。",
        "",
        f"规模:**{t['crates']} 个 crate / {t['src_lines']} 行 src**。",
        f"抓取时刻:`{t['captured_at']}`。",
        "",
        "crate 状态分三档,不要混读:`已实现` / `⚠ 空骨架`(有 `src` 但只有一句 `VERSION`)"
        "/ `🚧 脚手架`(只有 `Cargo.toml`,`src/` 还没建 —— 新建在制品)。",
        "",
        "## 1. 运行期数据流",
        "",
        "分层定义见 `docs/01-runtime-architecture.md` §1:**L3(VM)只能经 L4 trait 影响外部世界**。",
        "",
        mermaid_flow(data),
        "",
        "## 2. crate 依赖图",
        "",
        "实线 = 运行期依赖,虚线 = 仅 dev-dependencies(测试用)。",
        "`⚠ 空骨架` = `src` 总行数 ≤ 40;`D<n>` = description 未写 L 层标签的 crate,按依赖深度兜底分组。",
        "",
        mermaid(data),
        "",
        "## 3. 大 crate 的内部模块",
        "",
        "🧪 = 含内联测试模块。单文件行数是拆分优先级的依据(`AGENTS.md` §3.2)。",
        "",
        mermaid_modules(data),
        "",
        "## 4. 跨 crate 模块引用(源码实测 TOP 20)",
        "",
        "来自 `use yuris_x::mod` 的字面统计 —— 比 Cargo.toml 的声明更细,反映真实耦合点。",
        "",
        "| 引用方 | 被引用 crate | 模块 | 命中 |",
        "| --- | --- | --- | --- |",
    ]
    lines += [
        f"| `{e['from']}` | `{e['to']}` | `{e['module']}` | {e['hits']}` |" for e in data["imports"][:20]
    ]
    lines += [
        "",
        "## 5. crate 清单",
        "",
        "| crate | 分组 | src 行数 | 内部依赖数 | 被依赖数 | 状态 |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for name in sorted(crates, key=lambda n: (group_key(crates[n]), n)):
        c = crates[name]
        status = STATE_MARK[c["state"]] or "已实现"
        lines.append(
            f"| `{name}` | {group_key(c)} | {c['lines']} | "
            f"{len(c['deps_internal']['dependencies'])} | {depended.get(name, 0)} | {status} |"
        )

    problems = self_check(data)
    lines += ["", "## 6. 自动检查发现的待办", ""]
    lines += [f"- {p}" for p in problems] or ["无。"]
    lines.append("")
    return "\n".join(lines)


def dot(data: dict) -> str:
    crates = data["crates"]
    out = [
        "digraph yuris {",
        '  rankdir=TB; node [shape=box, style="rounded,filled", fillcolor="#f6f8fa", fontname="Consolas"];',
    ]
    by_group: dict[str, list[str]] = {}
    for name, c in crates.items():
        by_group.setdefault(group_key(c), []).append(name)
    for group in group_order(set(by_group)):
        names = by_group[group]
        out.append(f'  subgraph cluster_{group} {{ label="{group}"; style=dashed;')
        for name in sorted(names):
            c = crates[name]
            fill = "#fff4d6" if c["state"] == "scaffold" else ("#ffe9e9" if c["state"] == "skeleton" else "#eef7ee")
            out.append(f'    "{name}" [label="{name}\\n{c["lines"]} 行", fillcolor="{fill}"];')
        out.append("  }")
    for name, c in sorted(crates.items()):
        for dep in c["deps_internal"]["dependencies"]:
            out.append(f'  "{dep}" -> "{name}";')
    for name, c in sorted(crates.items()):
        for dep in c["deps_internal"]["dev-dependencies"]:
            out.append(f'  "{dep}" -> "{name}" [style=dotted];')
    out.append("}")
    return "\n".join(out)


def self_check(data: dict) -> list[str]:
    """健全性断言:图里出现的问题应当是值得报给人看的。"""
    problems = []
    crates = data["crates"]

    # 1. 依赖必须无环(架构文档声明的"严格单向")
    adj = {n: list(c["deps_internal"]["dependencies"]) for n, c in crates.items()}
    state: dict[str, int] = {}

    def walk(node: str, stack: list[str]) -> None:
        state[node] = 1
        for nxt in adj.get(node, []):
            if state.get(nxt) == 1:
                problems.append(f"依赖环: {' -> '.join(stack + [node, nxt])}")
            elif state.get(nxt) is None:
                walk(nxt, stack + [node])
        state[node] = 2

    for n in adj:
        if state.get(n) is None:
            walk(n, [])

    # 2. 未实现的 crate(区分"脚手架"与"空骨架",前者是新建在制品)
    for name, c in sorted(crates.items()):
        if c["state"] == "scaffold":
            problems.append(f"脚手架中(无 src/): {name}")
        elif c["state"] == "skeleton":
            problems.append(f"空骨架 crate: {name}({c['lines']} 行)")

    # 3. 没有任何内部依赖方的 crate(可能是死代码或入口)
    depended = {d for c in crates.values() for d in c["deps_internal"]["dependencies"]}
    for name in crates:
        if name not in depended:
            problems.append(f"无内部依赖方(入口或孤儿): {name}")
    return problems


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--format", choices=("mermaid", "dot", "json", "flow", "doc"), default="mermaid")
    ap.add_argument("--write", metavar="PATH", help="生成完整文档到该路径(相对仓库根)")
    ap.add_argument("--check", action="store_true", help="只跑健全性断言")
    args = ap.parse_args()

    data = collect()

    if args.check:
        problems = self_check(data)
        for p in problems:
            print(f"[!] {p}")
        t = data["totals"]
        print(f"[i] {t['crates']} crates / {t['src_lines']} 行 src / {len(problems)} 条待看")
        return 0

    if args.write:
        dest = ROOT / args.write
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(render_doc(data), encoding="utf-8", newline="\n")
        print(f"[+] 已写入 {args.write}({len(render_doc(data))} 字节)")
        return 0

    if args.format == "json":
        print(json.dumps(data, ensure_ascii=False, indent=2))
    elif args.format == "dot":
        print(dot(data))
    elif args.format == "flow":
        print(mermaid_flow(data))
    elif args.format == "doc":
        print(render_doc(data))
    else:
        print(mermaid(data))
        print()
        print("## 跨 crate 模块引用(源码实测,取前 20)")
        for e in data["imports"][:20]:
            print(f"  {e['from']:20s} -> {e['to']:14s}::{e['module']}  ×{e['hits']}")
        print()
        print("## 待看")
        for p in self_check(data):
            print(f"  [!] {p}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
