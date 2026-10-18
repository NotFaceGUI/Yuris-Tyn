#!/usr/bin/env python3
"""把 yuris-vm `run_main` 的命令臂拆成 exec/* 方法。

设计要点:
- **字符串/注释感知 mask**:字符串、注释、字符字面量里的花括号/continue 不算代码。
- **只转换目标为外层 while 的 `continue`**:嵌套 for/while/loop 内的 continue 保持原样
  (用 brace 栈判断:当前所在块是否由 for/while/loop 打开)。
- `return Ok(VmSuspend::…)` → `return Ok(Action::Suspend(VmSuspend::…))`。
- 顶层 `continue;` → `return Ok(Action::Keep);`(pc 已由处理器改好)。
"""
import os
import re

SRC = "crates/yuris-vm/src/lib.rs"
OUTDIR = "crates/yuris-vm/src/exec"

with open(SRC, encoding="utf-8") as f:
    src = f.read()


def code_mask(s):
    mask = bytearray(len(s))
    i, n = 0, len(s)
    while i < n:
        c = s[i]
        if c == '/' and i + 1 < n and s[i + 1] == '/':
            j = s.find('\n', i)
            i = n if j < 0 else j
            continue
        if c == '/' and i + 1 < n and s[i + 1] == '*':
            j = s.find('*/', i + 2)
            i = n if j < 0 else j + 2
            continue
        # raw string: r"..." / br"..." / r#"..."#
        m = re.match(r'(?:b?r|rb)(#*)"', s[i:])
        if m:
            closer = '"' + '#' * len(m.group(1))
            j = s.find(closer, i + len(m.group(0)))
            i = n if j < 0 else j + len(closer)
            continue
        if c == 'b' and i + 1 < n and s[i + 1] == '"':
            i += 1
            c = '"'
        if c == '"':
            i += 1
            while i < n:
                if s[i] == '\\':
                    i += 2
                    continue
                if s[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        if c == "'":
            m = re.match(r"'(?:\\.|[^'\\])'", s[i:])
            if m:
                i += m.end()
                continue
            i += 1
            continue
        mask[i] = 1
        i += 1
    return mask


def is_word_boundary(s, i):
    return i == 0 or not (s[i - 1].isalnum() or s[i - 1] == '_')


def transform_suspends(body):
    """return Ok(VmSuspend…) → return Ok(Action::Suspend(VmSuspend…));返回新串与计数。"""
    edits = []
    mask = code_mask(body)
    i, n = 0, len(body)
    needle = 'return Ok(VmSuspend'
    while i < n:
        if mask[i] and body.startswith(needle, i) and is_word_boundary(body, i):
            j = i + len(needle)
            # 从 `VmSuspend` **之后**开始扫描,所以 `return Ok(` 的 `(` 未计入 ——
            # 用 pd=1 补上它,否则括号深度偏 -1,永远等不到 depth==0 的 `;`
            # (实测踩到:Go 臂的 `return Ok(VmSuspend::Error(e.to_string()));`)。
            pd = 1
            bd = sq = 0
            while j < n:
                if mask[j]:
                    ch = body[j]
                    if ch == '(':
                        pd += 1
                    elif ch == ')':
                        pd -= 1
                    elif ch == '{':
                        bd += 1
                    elif ch == '}':
                        bd -= 1
                    elif ch == '[':
                        sq += 1
                    elif ch == ']':
                        sq -= 1
                    elif ch == ';' and pd == 0 and bd == 0 and sq == 0:
                        break
                j += 1
            if j >= n:
                raise SystemExit('suspends: no terminator at ' + str(i))
            edits.append((i, i + len(needle), 'return Ok(Action::Suspend(VmSuspend'))
            edits.append((j, j, ')'))
            i = j
            continue
        i += 1
    for a, b, rep in sorted(edits, key=lambda e: -e[0]):
        body = body[:a] + rep + body[b:]
    return body, len(edits) // 2


def transform_continue_break(body):
    """顶层 continue/break → Action;嵌套循环内的保持。"""
    mask = code_mask(body)
    stack = []
    cont, brk = [], []
    i, n = 0, len(body)
    while i < n:
        if not mask[i]:
            i += 1
            continue
        c = body[i]
        if c == '{':
            j = i - 1
            while j >= 0 and not (mask[j] and body[j] in ';{}'):
                j -= 1
            seg = ''.join(body[k] for k in range(j + 1, i) if mask[k])
            stack.append(bool(re.search(r'\b(for|while|loop)\b', seg)))
            i += 1
            continue
        if c == '}':
            if stack:
                stack.pop()
            i += 1
            continue
        if c.isalpha() or c == '_':
            m = re.match(r'[A-Za-z_][A-Za-z0-9_]*', body[i:])
            word = m.group(0)
            end = i + len(word)
            if word in ('continue', 'break') and is_word_boundary(body, i):
                k = end
                while k < n and body[k] in ' \t\r\n':
                    k += 1
                if k < n and body[k] == ';' and mask[k] and not any(stack):
                    (cont if word == 'continue' else brk).append((i, k + 1))
                i = k
                continue
            i = end
            continue
        i += 1
    for a, b in sorted(cont, key=lambda e: -e[0]):
        body = body[:a] + 'return Ok(Action::Keep);' + body[b:]
    for a, b in sorted(brk, key=lambda e: -e[0]):
        body = body[:a] + 'return Ok(Action::Break);' + body[b:]
    return body, len(cont), len(brk)


def main():
    lines = src.split('\n')
    i_run = next(k for k, l in enumerate(lines) if re.match(r'^    fn run_main', l))
    i_while = next(k for k in range(i_run, len(lines)) if re.match(r'^        while self\.pc', lines[k]))
    arm_re = re.compile(r'^                (CmdKind::(\w+)|_) => ')
    # match 块的**真结尾**:从 `let kind = …` 起做花括号深度跟踪,归零处即闭合。
    # (用「第一个 12 空格 `}`」是错的 —— 循环体内部也有同缩进的 `}`;实测踩到。)
    i_kind0 = next(k for k in range(i_while, len(lines))
                   if re.match(r'^            let kind = self\.kind_of', lines[k]))
    span = '\n'.join(lines[i_kind0:])
    mask = code_mask(span)
    depth = 0
    seen_open = False
    i_close = None
    for off, ch in enumerate(span):
        if not mask[off]:
            continue
        if ch == '{':
            depth += 1
            seen_open = True
        elif ch == '}':
            depth -= 1
            if seen_open and depth == 0:
                i_close = i_kind0 + span[:off].count('\n')
                break
    if i_close is None:
        raise SystemExit('找不到 match 块结尾')
    print(f"match 闭合行 @{i_close+1}: {lines[i_close]!r}")
    # 关键:臂必须落在 match 块内。`_ => continue` 之类同缩进的行在**其他函数**
    # 里也会出现(实测 line 4304),不设上界会把它们当臂 → 兜底臂错位。
    starts = [(k, (m.group(2) if m.group(2) else '_'))
              for k, l in enumerate(lines) if i_while < k < i_close and (m := arm_re.match(l))]
    # 陷阱:臂**内部**的嵌套 `match` 也可能有恰好 16 空格缩进的 `_ => {`。
    # 兜底臂是编译顺序上的**最后一个**(行号最大者),其余 `_` 剔除。
    real_fallback = max(k for k, n in starts if n == '_')
    starts = [(k, n) for (k, n) in starts if n != '_' or k == real_fallback]
    print(f"arms(去幻影后)={len(starts)}")
    print(f"run_main @{i_run+1} while @{i_while+1} match_close @{i_close+1}")

    spans = []
    for idx, (st, name) in enumerate(starts):
        end = starts[idx + 1][0] - 1 if idx + 1 < len(starts) else i_close - 1
        spans.append((st, end, name))

    BUCKETS = {
        'flow': ['Go', 'Gosub', 'ReturnCode', 'ReturnBreak', 'Return', 'If', 'IfBlend',
                 'Else', 'IfEnd', 'Loop', 'LoopBreak', 'LoopContinue', 'LoopEnd', 'Wait', 'End'],
        'vars': ['Let', 'VarInfo', 'VarAct', 'LabelInfo'],
        'media': ['Cg', 'CgInfo', 'CgAct', 'CgEnd', 'Text', 'Word', 'Sound'],
        'io': ['Load', 'Save'],
        'misc': ['_'],
    }
    bucket_of = {n: b for b, ns in BUCKETS.items() for n in ns}

    methods = {b: [] for b in BUCKETS}
    replacements = []
    tot_sus = tot_cont = tot_brk = 0
    for st, end, name in spans:
        m = arm_re.match(lines[st])
        head_rest = lines[st][m.end():]
        if head_rest.strip() == '{':
            # 块形臂:`=> { … }` —— 去掉两侧花括号,取中间语句
            body = '\n'.join(lines[st + 1:end])
        else:
            # 表达式形臂(如 Go:`=> match self.exec_go(&windows)? { … },`)
            # head_rest 已经含开括号,必须把该臂的关括号(最后一行的 `},`)补回来,
            # 否则花括号失衡 → transform 的深度跟踪失效(实测踩到)。
            closer = lines[end].rstrip()
            if closer.endswith(','):
                closer = closer[:-1]
            if not closer.endswith('}'):
                raise SystemExit(f'arm {name}: 表达式臂的收尾行不是闭括号: {closer!r}')
            body = head_rest + '\n' + '\n'.join(lines[st + 1:end] + [closer])
        body, ns = transform_suspends(body)
        body, nc, nb = transform_continue_break(body)
        tot_sus += ns
        tot_cont += nc
        tot_brk += nb
        ded = [l[12:] if l.startswith(' ' * 12) else l for l in body.split('\n')]
        fn = 'cmd_' + (name.lower() if name != '_' else 'fallback')
        meth = (f"    /// 处理 `{name}`(从 `run_main` 拆出;语义逐行照搬)。\n"
                f"    pub(crate) fn {fn}(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {{\n"
                + '\n'.join(ded) + "\n    }\n")
        methods[bucket_of[name]].append(meth)
        call = f"                {('CmdKind::' + name) if name != '_' else '_'} => self.{fn}(g, windows),"
        replacements.append((st, end, [call]))

    print(f"转换: suspends={tot_sus} outer_continue={tot_cont} outer_break={tot_brk}")

    new_lines = list(lines)
    for st, end, rep in sorted(replacements, key=lambda r: -r[0]):
        new_lines[st:end + 1] = rep

    i_kind = next(k for k in range(i_while, len(new_lines))
                  if re.match(r'^            let kind = self\.kind_of', new_lines[k]))
    i_close2 = next(k for k in range(i_kind, len(new_lines)) if new_lines[k] == '            }')
    dispatch = [
        "            let kind = self.kind_of(g.command_type);",
        "            // 命令分派已拆至 `exec/`(命令族一个方法;见该模块文档)。",
        "            // 控制流经 `Action` 回传:Keep = pc 已改好;Suspend = 让出。",
        "            match self.dispatch(kind, g, &windows)? {",
        "                Action::Keep => {}",
        "                Action::Break => break,",
        "                Action::Suspend(s) => return Ok(s),",
        "            }",
    ]
    new_lines[i_kind:i_close2 + 1] = dispatch
    with open(SRC, 'w', encoding='utf-8', newline='') as f:
        f.write('\n'.join(new_lines))

    HDR = '''//! {title}(从 `run_main` 巨型 match 拆出;语义逐行照搬,未改行为)。
//!
//! 拆分背景见 [`crate::exec`] 模块文档。**控制流约定**:方法体里不再出现
//! 「目标为外层 while」的 `continue` / `return Ok(VmSuspend…)` —— 分别替换为
//! `return Ok(Action::Keep)` / `return Ok(Action::Suspend(…))`,由 `run_main`
//! 的循环统一执行(语义 1:1)。

use yuris_core::{{Error, Result}};
use yuris_format::ystb::{{CommandGroup, CommandSlot}};
use yuris_value::Value;

use crate::event::{{VmEvent, VmSuspend}};
use crate::exec::Action;
use crate::GroupVm;

'''
    TITLES = {'flow': '流程控制族', 'vars': '变量与查询族', 'media': '媒体族(CG/文本/声音)',
              'io': '存档族', 'misc': '兜底与声明族'}
    os.makedirs(OUTDIR, exist_ok=True)
    for b, ms in methods.items():
        with open(f"{OUTDIR}/{b}.rs", 'w', encoding='utf-8', newline='') as f:
            f.write(HDR.format(title=TITLES[b]))
            for meth in ms:
                f.write('\n' + meth)
        nlines = sum(m.count('\n') for m in ms)
        print(f"exec/{b}.rs: {len(ms)} 方法, {nlines} 行")


if __name__ == '__main__':
    main()
