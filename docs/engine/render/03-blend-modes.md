# 03 · 描画模式 `MODE=0..18`（混合模式）——二进制规格

> 消费点：图层绘制函数 `FUN_00414ba4`（`decomp/00414ba4`，逐层每部件调用）。
> 手册给出名字（0=通常 … 18=ピンライト，19 种）；**本文给的是引擎实际下发的 D3D 状态序列**。
> 等级：§2 状态序列 **Confirmed**（反编译原值，逐条可复核）；§3 的 D3D 枚举解读与 wgpu 等价式 **Likely**（须对拍后转正）；
> §4 的模式 4/8/10..18 **Unknown**（见该节挂号）。

## 1. 分派结构（Confirmed）

```
绘制函数参数(经寄存器/栈, 反编译有错位但分派值完整):
  MODE = 传入参数(调用链自 层+0x87 读出)
  if MODE ∈ {0,1,2,3,5,6,7,9,100,0x62(=98)}:
      TFACTOR = (A<<24)|(A<<16)|(A<<8)|A        // A = clamp 后的层不透明度(0..255)
  else:
      TFACTOR = 0xFF000000                       // 不透明黑
  // 固定管线分支只覆盖 {0,1,2,3,5,6,7,9}; 4/8/10..18 无 case ⇒ 可编程(着色器)路径
```

- `100` 与 `98(0x62)` 不是脚本可达的 `MODE` 值：是引擎内部绘制种类（文字/系统层族）复用同一绘制函数的传参 ——
  它们与 0..18 共享"TFACTOR=alpha 灰度"策略（Likely：内部层把不透明度写进 TFACTOR/顶点色）。
- `SetRenderState(60 /*TEXTUREFACTOR*/, TFACTOR)` 在 MODE=0 与 MODE=7 分支内出现
  （其余模式的 TFACTOR 由公共路径/前置调用设置——绘制函数开头也有一处 `RS(0x3c, …)`）。

## 2. 八种固定管线模式：引擎下发的原始状态序列（Confirmed）

记法：`TSS(stage, state, value)` = `SetTextureStageState`；`RS(state, value)` = `SetRenderState`。
每层每部件先执行公共前缀（Confirmed）：

```
SetPixelShader 相关复位调用(vtable#107, 错误串 "D3D SetPixelShader Error.")   // 固定管线前清着色器; vtable 序号与串名的出入见 §5 挂号
SetTexture(2, NULL) → SetTexture(0, 层+0x21c) → SetTexture(1, 配对纹理或 NULL)
```

| MODE | 手册名 | TSS 序列（原值） | RS 序列（原值） |
|---|---|---|---|
| 0 | 通常 | TSS(0,1,2) TSS(0,2,2) TSS(1,1,2) TSS(1,2,2) TSS(2,1,1) TSS(0,4,4) TSS(0,5,2) TSS(0,6,3) TSS(1,4,4) TSS(1,5,2) TSS(1,6,3) TSS(stage?,4,1) | RS(60,TFACTOR) RS(19,5) RS(20,6) RS(171,1) |
| 1 | 加算 | TSS(0,1,2) TSS(0,2,2) TSS(1,1,1) TSS(0,4,4) TSS(0,5,2) TSS(0,6,0) TSS(1,4,1) | RS(19,5) RS(20,2) RS(171,1) |
| 2 | 減算 | TSS(0,1,4) TSS(0,2,0) TSS(0,3,2) TSS(1,1,1) TSS(0,4,4) TSS(0,5,0) TSS(0,6,2) TSS(1,4,1) | RS(19,5) RS(20,2) RS(171,3) |
| 3 | 乗算 | `SetTexture(1,DAT_00801d98)` RS(60,0xFFFFFFFF) TSS(0,1,4) TSS(0,2,0) TSS(0,3,2) TSS(1,1,16) TSS(1,2,1) TSS(1,3,2) TSS(0,4,4) TSS(0,5,0) TSS(0,6,2) TSS(1,4,2) TSS(1,5,1) | RS(19,1) RS(20,3) RS(171,1) |
| 5 | スクリーン | `SetTexture(1,DAT_00801d9c)` TSS(0,1,4) TSS(0,2,2) TSS(0,3,0) TSS(1,1,16) TSS(1,2,1) TSS(1,3,2) TSS(0,4,4) TSS(0,5,2) TSS(0,6,0) TSS(1,4,2) TSS(1,5,1) | RS(19,10) RS(20,2) RS(171,1) |
| 6 | 比較(明) | TSS(0,1,4) TSS(0,2,2) TSS(0,3,0) TSS(1,1,1) TSS(0,4,4) TSS(0,5,2) TSS(0,6,0) TSS(1,4,1) | RS(19,2) RS(20,2) RS(171,5) |
| 7 | 比較(暗) | RS(60,0xFFFFFFFF) TSS(0,1,4) TSS(0,2,2) TSS(0,3,0) TSS(1,1,16) TSS(1,2,1) TSS(1,3,3) TSS(0,4,4) TSS(0,5,2) TSS(0,6,0) TSS(1,4,2) TSS(1,5,1) | RS(19,2) RS(20,2) RS(171,4) |
| 9 | 除外 | TSS(0,1,4) TSS(0,2,2) TSS(0,3,0) TSS(1,1,1) TSS(0,4,4) TSS(0,5,2) TSS(0,6,0) TSS(1,4,1) | RS(19,10) RS(20,4) RS(171,1) |

TSS `state` 位 = D3D9 标准值：1=COLOROP、2=COLORARG1、3=COLORARG2、4=ALPHAOP、5=ALPHAARG1、6=ALPHAARG2。

## 3. 枚举解读与 wgpu 等价式（**Likely——实现对拍通过前一律 `// UNVERIFIED`**）

D3D9 常量对照：`D3DTOP` 1=DISABLE 2=SELECTARG1 4=MODULATE 16=MODULATEALPHA_ADDCOLOR；
`D3DTA` 0=TFACTOR 1=CURRENT 2=DIFFUSE 3=TEXTURE；
`D3DBLEND` 1=ZERO 2=ONE 3=SRCCOLOR 4=INVSRCCOLOR 5=SRCALPHA 6=INVSRCALPHA 9=DESTCOLOR 10=INVDESTCOLOR；
`D3DBLENDOP` 1=ADD 2=SUBTRACT 3=REVSUBTRACT 4=MIN 5=MAX。
混合总式：`out = src × SRCBLEND ⊕op dst × DESTBLEND`。

| MODE | 解读后的混合 | wgpu `BlendComponent` 等价 |
|---|---|---|
| 0 通常 | src=顶点色系（颜色链见 ⚠），α=diffuse.a×tex.a；`out = src·α + dst·(1−α)` | `{src: SrcAlpha, dst: OneMinusSrcAlpha, op: Add}`（再乘顶点 α=xA/255） |
| 1 加算 | `out = src·α + dst` | `{SrcAlpha, One, Add}` |
| 2 減算 | TFACTOR 参与调制；`out = dst − src·α` | `{SrcAlpha, One, ReverseSubtract}`（wgpu `ReverseSubtract`） |
| 3 乗算 | `out = dst × src_color`（辅助纹理+两段调制实现"α 调制后乘"） | wgpu 无 SRCCOLOR 因子 ⇒ **需 fragment shader 实现**（`out = dst*src`），或退而 `Dst`×… 见 §5 挂号 |
| 5 スクリーン | `out = src·(1−dst) + dst`（经典 screen） | fragment shader（`1-(1-src)(1-dst)`），wgpu 因子 `OneMinusDst`+`One` 可近似 alpha 项 |
| 6 比較(明) | `out = max(src·?, dst)`（ADD+MAX） | fragment shader `max(src,dst)`（`BlendOperation::Max` 仅在无乘系数时可用） |
| 7 比較(暗) | `out = min(src·?, dst)`（MIN） | 同上 `min` |
| 9 除外 | `out = src·(1−dst) + dst·(1−src)` | fragment shader（difference/exclusion 家族） |

⚠ **MODE 0 颜色链待汇编复核（当前最大不确定点）**：按字面解读，MODE 0 的颜色阶段是
`COLOROP=SELECTARG1(COLORARG1=DIFFUSE)` ⇒ 顶点色直通、贴图 RGB 不进颜色 —— 与"通常模式显示贴图"矛盾。
合理解释是顶点 DIFFUSE 携带已调制的层色（顶点填充助手 `FUN_0041a308` 每顶点写 5+ 个浮点、
其中第 5 槽按 `A<<24` 打包 —— 见 01 §6 / 03 §4），即**引擎把逐层颜色调制搬到了 CPU 顶点色里**。
在汇编级确认 FVF 与顶点 DIFFUSE 内容之前，本仓库**不得**把 MODE 0 简化成"纯纹理 α 混合"了事；
对拍基准见 `05-parity-matrix.md` §4。

## 4. 模式 4（除算）、8（差分）、10..18（Overlay..PinLight）——可编程路径（**Unknown，禁止猜**）

事实（Confirmed）：
1. 固定管线分支没有这些 case；
2. 它们的 TFACTOR 固定 `0xFF000000`；
3. 引擎持有 `ID3DXEffect`（HLSL）与 `SetPixelShader` 封装（错误串 `SetPixelShader Error.` @0x80b808、
   `D3D SetPixelShader Error.` @0x80bc20），uniform 池含 `YS_Texture3/YS_TexSize/YS_TexDiffU/YS_ShaderTest1`
   （后三者只被着色器路径引用）—— 双重采样（本层+背景）正是 Overlay/Light 系公式（`dst` 项）的必要条件。

⇒ 结论：这 11 种模式的运算本体在着色器里。**着色器源/字节码的落点未定位**（创建函数不在 3698 个导出 .c 中，
`[] Shader Init Error.` @0x80b592 与 `YS_RES` @0x807bac 为入口线索），在此之前：
- 本仓库对这些模式**必须显式 `Unimplemented`**（铁律 2），不得用 Photoshop 公式顶替；
- 恢复路径按优先级：(a) REA 定位 effect 创建点（搜 `D3DXCreateEffect*` 导入 thunk 的交叉引用）；
  (b) 运行时 trace：引擎跑 ERIS 样例 + 枚举 MODE 0..18 截屏对拍（`engine_trace.py` 同思路）；
  (c) 从 `ysres` 资源/内嵌资源段提取 fx。

## 5. 挂号

| # | 项 | 状态 |
|---|---|---|
| G4 | `FUN_00414ba4` 开头 vtable#107 调用的精确身份（SetPixelShader 复位 vs SetPixelShaderConstantB）与错误串不一致处 | 读 `0x4171a0` 附近汇编 8 条指令即可裁定 |
| G5 | MODE 0 颜色链（§3 ⚠）：FVF 精确标志（`D3D SetFVF Error.` @0x80b334 xref）、顶点 DIFFUSE 的 RGB 来源 | 汇编级核验 + 实机对拍 |
| G6 | `DAT_00801d98` / `DAT_00801d9c` 两张辅助纹理的内容（乗算/スクリーン用） | 从创建点（`CreateTexture`/`D3DXCreateTexture*` 调用者）取尺寸与填充代码 |
| G7 | 11 种着色器模式本体 | §4 恢复路径 |
| G8 | `TSS(2,1,1)`（stage2 COLOROP=DISABLE）之外的 stage2 用途（反编译 stage 序号有错位） | 汇编核验 |

## 6. 引擎 D3D 错误串全表（状态封装点索引，Confirmed）

`0x80b334` SetFVF · `0x80b348` RS_CULLMODE · `0x80b360` RS_ZENABLE · `0x80b378` RS_ZWRITEENABLE ·
`0x80b394` RS_ZFUNC · `0x80b3a8` RS_LIGHTING · `0x80b3c0` RS_ALPHABLENDENABLE · `0x80b3e0` RS_DITHERENABLE ·
`0x80b454` SAMP_ADDRESSU · `0x80b46c` SAMP_ADDRESSV · `0x80b6a4` Clear · `0x80b864` RS_TEXTUREFACTOR ·
`0x80b880` TSS_COLOROP · `0x80b898` TSS_COLORARG1 · `0x80b8b0` TSS_ALPHAOP · `0x80b964` Present ·
`0x80b978` BeginScene · `0x80b990` EndScene · `0x80bc20` D3D SetPixelShader · `0x80bc84` TSS_ALPHAARG1 ·
`0x80bc9c` TSS_ALPHAARG2 · `0x80bcb4/0x80bccc/0x80bce4` 第二组 COLOROP/COLORARG2/ALPHAOP ·
`0x80bd64` YS_D3DDevice · `0x80bd90/0x80bdac/0x80bdc8` SAMP_MAG/MIN/MIPFILTER · `0x80b808` SetPixelShader。
（每串 xref = 一个状态封装点；z/clipping 族串存在说明引擎也设置 ZEnable/ZWrite/Cull —— 2D 层绘制路径未用到，
初始化态对齐时再取。）
