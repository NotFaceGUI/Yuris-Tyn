# v494 命令处理器表(取证产物,2026-10-16)

> **用途**:`docs/audit-todo-02.md` 的 T4/T9/T23 等「引擎语义取证」任务的**地址地图**。
> 此前挂号的证据缺口是「处理器表在 `.data` 未初始化区、静态扫不出内容,须先找初始化器」——
> 本文即该初始化器的定位结果与逐条 dump。

## 1. 表在哪、谁填的(Confirmed)

| 项 | 值 | 证据 |
|---|---|---|
| 表基址 | `0x7aae40`(步长 4) | `MOV ESI, dword ptr [ECX*4 + 0x7aae40]`@`0x451b09`(REA `ev_06ab134759008b191523fb8529a245e69ec8a50df61541c3343fa081134b1365`) |
| 表大小 | `0x1dc` 字节 = **119 项** | 初始化器 memset 循环上界 `iVar4 < 0x1dc` |
| 默认值(未显式赋值项) | `&LAB_00459c2c`(共享空实现) | 同上 memset |
| **初始化器** | **`FUN_0045cd30`**(0x45cd30,1728 字节) | REA `ev_d65f06ce7310700c579882ccfd2c2725647f6fbe9201e6dae18d5124e75e3d1b`(伪代码) |
| 调用时机 | `FUN_00464fa4`@0x4613e4 的启动序列(在载 YSCM/YSLB 之前) | `FUN_00464fa4` 伪代码:先 `FUN_0045cd30()`,再 `FUN_0045d3f0`/`FUN_00465108`… |

**勘误留痕**:任务书与 `docs/audit-todo-02.md` 曾把 `0x802ba0` 记作「分派表」——
本次取证确认 **`0x802ba0` 是表达式 opcode 表**(30 项,`FUN_00448110` 等按表达式下标调用它),
**命令处理器表是 `0x7aae40`**。两者用途不同,不得混称。

## 2. 执行链(Confirmed)

```text
帧驱动(0x4613e4 一层)
  └─ FUN_00464fa4          每任务「一步」:取当前任务 → 写 pc(task+0x3c) → 调分派 → 回写记录
       └─ FUN_0045177d     命令分派:CALL [handler[cmd_index]];并做 pc 推进/让出判定(0x45177d..0x451d19)
            └─ handler[...] 具体处理器(见 §3)
```

## 3. 逐条映射(v494,按 `docs/opcode/yscm-commands-494.tsv` 名表对齐)

只列**非共享 stub** 的项(`FUN_0044658c = return 0` 是编译期命令/no-op 的填充值)。

| idx | 命令 | 处理器 | 备注 |
|---|---|---|---|
| 0 | ALIAS | `LAB_00429f44` | |
| 1 | CG | `LAB_0042a4a8` | |
| 2 | CGACT | `LAB_0042c438` | |
| 3 | CGEND | `LAB_0043d2ac` | |
| 4 | CGINFO | `LAB_0043d5dc` | |
| 5 | CLIPACT | `LAB_0043e248` | |
| 6 | CLIPINFO | `LAB_0043e388` | |
| 7 | CSV | `LAB_0043e4fc` | |
| 8 | CSVINFO | `LAB_0043e51c` | |
| 10 | DIALOG | `DAT_0043fb38` | |
| 11 | ELSE | `LAB_004403b4` | |
| 12 | EMOTE | `FUN_00440468` | |
| 14 | END | `LAB_00440ed8` | |
| 15 | ERROR | `LAB_004411f8` | |
| 16 | ERRORINFO | `LAB_00463320` | |
| 17 | F_FLT | `LAB_00463300` | |
| 21 | FILEINFO | `LAB_00441234` | |
| 22 | FLASH | `LAB_00442748` | **T23 取证入口** |
| 23 | FLASHEND | `LAB_00443660` | |
| 24 | FLASHINFO | `LAB_00443cb4` | |
| 25 | FLT | `LAB_00443d04` | |
| 26 | FONT | `LAB_0044413c` | |
| 27 | FONTINFO | `LAB_00444278` | |
| 28 | FPS | `LAB_00444b4c` | |
| 29–42 | G_* | `LAB_004455e8` | G_FLT 起共 14 项(多命令同址) |
| 43 | GO | `LAB_00445670` | |
| 44 | IF | `LAB_00445804` | |
| 45 | IFBLEND | `LAB_00446110` | |
| 46 | IFBREAK | `LAB_00446208` | |
| 47 | IFCONTINUE | `LAB_00446248` | |
| 48 | IFEND | `LAB_004462d0` | |
| 49 | INPUT | `LAB_00446358` | |
| 50 | INT | `LAB_004463a4` | |
| 51 | LABEL | `LAB_0044645c` | |
| 52 | LABELINFO | `FUN_0044658c` | **stub** |
| 53 | LET | `LAB_00446590` | |
| 54 | LOAD | `LAB_00446724` | |
| 55 | LOOP | `LAB_00446ed0` | |
| 56 | LOOPBREAK | `LAB_00447d3c` | |
| 57 | LOOPCONTINUE | `LAB_00447ed8` | |
| 58 | LOOPEND | `LAB_00447f90` | |
| 59 | MACRO | `LAB_00448024` | |
| 61 | **MATH** | **`FUN_00448110`** | 运算派发(表达式表 `0x802ba0`) |
| 62 | MATH3D | `FUN_0044874c` | |
| 63 | MENU | `LAB_00449c44` | |
| 64 | MENUACT | `LAB_0044a268` | |
| 65 | MENUEND | `LAB_0044a408` | |
| 66 | MENUINFO | `LAB_0044a52c` | |
| 67 | MIDI | `LAB_0044a878` | **T23 取证入口** |
| 68 | MIDIEND | `LAB_0044a898` | |
| 69 | MIDIINFO | `LAB_0044a8b8` | |
| 70 | MOUSE | `LAB_0044a8d8` | |
| 71 | MOVIE | `LAB_0044af78` | **T23 取证入口** |
| 72 | MOVIEEND | `LAB_0044b844` | |
| 73 | MOVIEINFO | `LAB_0044b898` | |
| 74 | PLUG | `LAB_0044bb84` | |
| 75 | PREP | `FUN_0044658c` | **stub**(编译期) |
| 76 | REGINFO | `LAB_0044bba4` | |
| 77 | RETURN | `LAB_0044bf78` | |
| 78 | RETURNBREAK | `FUN_0044658c` | **stub** |
| 79 | **RETURNCODE** | **`LAB_0044c20c`** | T4:见 §4 |
| 80 | S_FLT | `FUN_0044658c` | **stub** |
| 81 | S_INT | `FUN_0044658c` | **stub** |
| 82 | S_STR | `FUN_0044658c` | **stub** |
| 84 | SAVE | `LAB_0044c2d8` | |
| 85 | SAVEEND | `LAB_0044e034` | |
| 86 | SAVEINFO | `LAB_0044e05c` | |
| 87 | SOUND | `LAB_0044e07c` | |
| 88 | SOUNDEND | `LAB_0044f19c` | |
| 89 | SOUNDINFO | `LAB_0044f350` | |
| 90 | STR | `LAB_0044ff6c` | |
| 91 | SYSTEM | `LAB_004500d0` | |
| 92 | SYSTEMINFO | `LAB_00450478` | |
| 93 | TASK | `LAB_00450588` | |
| 94 | TASKEND | `LAB_00452070` | |
| 95 | TASKINFO | `LAB_00452270` | |
| 96 | TEXT | `LAB_004532bc` | |
| 97 | TEXTINFO | `LAB_00453740` | |
| 98 | TRAY | `LAB_004538c0` | |
| 99 | VAR | `LAB_00453de8` | |
| 100 | VARACT | `LAB_00453e08` | |
| 101 | VARINFO | `LAB_00455e10` | |
| 102 | WAIT | `LAB_004568ac` | |
| 103 | WINDOW | `DAT_00456938` | |
| 104 | WINDOWEND | `LAB_0045840c` | |
| 105 | WINDOWINFO | `LAB_0045848c` | |
| 106 | WORD | `LAB_00458ae4` | |
| 107 | YMV | `LAB_00458c7c` | **T23 取证入口** |
| 108 | YMVEND | `LAB_00459800` | |
| 109 | YMVINFO | `LAB_00459934` | |
| 110 | `_` | `LAB_00456588` | |

**判别力自查**:本表把 `RETURNCODE` 对到 `0x44c20c`、`MATH3D` 对到 `0x44874c`、
`TASK` 对到 `0x450588` —— 三者与 `PROGRESS.md` 里独立得出的地址一致(旧挂号
「MATH3D 处理体 `0x448110`」实为 `FUN_00448110`=**MATH** 主入口,`MATH3D` 是它后面的
独立函数;同为本次一并勘误)。

## 4. T4:RETURNCODE v494 处理器体(U15 解除依据)

`0x44c20c`(96+120 字节,REA `ev_b248a7b7…` / `ev_aa1fcb9e…`)反汇编要点:

```asm
0x44c20c  PUSH ESI
0x44c20d  MOV ECX, [0x8029f4]        ; 当前任务对象
0x44c213  MOV EAX, [ECX+0x30]        ; 记录数组
0x44c216  MOV EDX, [0x802a08]
0x44c21c  MOV ESI, [EAX+EDX*4]
0x44c21f  MOV ESI, [ESI]             ; 调用点标记值
0x44c221  MOV [ECX+0x0c], ESI        ; → 任务 +0x0c
0x44c224  MOV EAX, [EAX+EDX*4]
0x44c227  MOV EAX, [EAX+4]
0x44c22a  MOV [ECX+0x10], EAX        ; → 任务 +0x10
0x44c22d  MOV EAX, [0x802ad8]        ; 事件队列对象
0x44c234  MOVSX EDX, byte [EAX+0x1c]
0x44c238  TEST EDX, EDX
0x44c23a  JZ  0x44c2a5               ; 队列未启用 → 跳过
…         << 追加字节 0x43('C') 到任务缓冲(task+0x64 写游标 / +0x3c 基址) >>
…         << 其他分支追加 '0'/'1'/'5' 等;末尾 task+0x36 = 1 >>
0x44c2a5  MOV EAX, -1 ; POP ESI ; RET
```

**结论(与 v555 侧结论一致,双版本同判)**:`RETURNCODE` 的副作用**全在任务对象上**
(任务 +0x0c/+0x10 记录调用点标记、事件队列缓冲、+0x36 状态位),
**不改脚本 pc、不动调用栈、不写分派记录**。
⇒ 本内核「推进 pc 的空操作」语义**正确**;U15 解除,`// UNVERIFIED` 可去掉
(call-site 标记值本内核已由 `returncode_hits()` 计数并覆盖测试,不落任务对象)。

## 5. T6:TASK 步进量子(U24 解除依据)

- `FUN_00464fa4`@0x4613e4 调用链:取当前任务 → 写 `task+0x3c = pc` → **调用分派一次** →
  回写 6 个记录字段(`+0x24..+0x38`)。**全程没有指令/命令计数器**。
- 即引擎的「一步」= **一条命令**;让出与否由**命令语义**决定(WAIT/TEXT 等),
  外层帧驱动按任务逐个调用。
- ⇒ 不存在「N 条指令后抢占」的量子;本内核 `TASK_QUANTUM = 65536` 的定位
  (工程失控保护,非引擎常量)**确认**;`YURIS_TASK_QUANTUM` 覆盖保留作对拍开关。
  U24 解除。

## 复现方式

```bash
# 表内容:REA 打开 <game-b> 引擎(sha256 1ff3135d…)后
#   read_bytes 0x7aae40 476          # 初始化前的静态内容(未初始化区,全 0/无意义)
#   procedure_pseudo_code 0x45cd30   # 初始化器:逐条赋值即本文映射
#   inspect_native_instruction 0x451b09  # 表读取点(MOV ESI,[ECX*4+0x7aae40])
# 名表对齐:docs/opcode/yscm-commands-494.tsv(119 条,与表项数一致)
```

**证据等级**:表结构 / 初始化器 / 处理器地址 = **Confirmed**(shipped-artifact,REA);
逐条「非 stub 项」的语义仍按各自任务另行取证(地址仅说明"从哪读起")。
