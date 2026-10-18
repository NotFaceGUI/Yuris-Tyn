# 04 · 不透明度 / 过渡 / 掩码 / 离屏（A · T · TA · TID · MID · CGACT[COPY]）

> 目标 `yu-ris.exe` sha256 `4f5eefa3…`。等级除注明外 **Confirmed**（反编译实测）。

## 1. `A` 不透明度（手册 0..256）

两条消费路径并存（都在绘制函数 `FUN_00414ba4` 内，Confirmed）：

1. **固定管线侧**：`uVar = (u16)层+0x80; clamp(uVar, 0, 255)` ——
   随后进入 `TFACTOR = A..A 灰度复制`（MODE∈{0,1,2,3,5,6,7,9,100,98}）与顶点色打包（`A<<24`）。
   ⇒ 固定管线世界里的 256 与 255 等值（clamp 语义）。
2. **HLSL 侧**：`SetFloat("YS_AlphaVal", (float)(i16)层+0x80 × 0.00390625)`，常量 0x58ec00 = 1/256 ——
   `A=256` 在此通道 = 恰好 1.0（不经过 clamp）。

**对齐规则**：本仓库内部保留 `A` 的 0..256 原值；渲染时固定管线等价路径取 `min(A,255)/255`、
着色器等价路径取 `A/256`。两者不得混用同一缩放（像素级对拍点，成果 122 的 `A/256` 播放器近似要按此细化）。

## 2. 过渡 `T / TA / TID`（トランジション）

HLSL 通道（ID3DXEffect，`DAT_00801e7c`）每层每部件：

```
门控(Confirmed):
  if 部件计数A==0 或 层+0x84(TA) < 1:   无过渡形态
      若次级槽==0 → 不用 effect(pass=0); 否则 effect->BeginPass(0)   // 单纹理淡入淡出形态
  else:                                  过渡形态
      effect->BeginPass(1)              // 双纹理 cross-fade 形态
uniform(Confirmed):
  YS_Texture1 = 层+0x21c(本层纹理)
  YS_Texture2 = 配对纹理数组第 i 项(TID 的对侧图;仅过渡形态且有配对时设置)
  YS_AlphaVal = A × 1/256
  YS_FadeVal  = T / (256/(256/TA + 128)) × 1/256        // 常量 256.0/128.0/1/256 @0x58ec08/10/00
  YS_FadeVal2 = 第二淡值(配对侧; 反编译噪声重,取值源 Unknown)
每部件结束: effect->CommitChanges(); 全部部件后 effect->End()
```

- `YS_FadeVal` 公式形状 Confirmed（原式
  `(double)(i16)+0x82 / (256.0/(256.0/(double)(i16)+0x84 + 128.0)) * 0.00390625`）；
  `+0x82=T、+0x84=TA` 的指认 **Likely**（与手册参数对位 + 属性交换函数把两者与 A/MODE 成组交换互证）。
  TA 越大 → 分母趋近 256/(1+128)≈1.98 → FadeVal≈T/507（慢）；TA 小 → 快速跨到 1.0：**精度=离散步长**。
- 层侧过渡进度来自 `+0x1e8`（i64，遍历器按 `0.5×v/100` 消费）与 `+0x82` 的关系（谁推进谁）
  归 CG 命令写侧（挂号 G1，见 02 §5）。
- **对齐要求**：过渡 = **双输入合成 pass**（src、dst 两纹理 + FadeVal 阈值混合），
  wgpu 侧需独立的 transition pipeline；不能用"层 A 从 0 拉满"近似（`TID` 的 cross-fade 是两图交叉，不是单图淡入）。
- 掩码 `MID`（手册：指定另一层作为掩码）：与过渡共用 `SetTexture(1, 配对纹理)` 槽位与 stage1 ALPHA 链
  （`AA1=DIFFUSE/AA2=TEXTURE` 的 MODULATE）。掩码取 α 还是亮度 = **Unknown**（需 stage1 配对纹理的生成代码，
  挂号 G9）；在裁定前本仓库掩码路径保持未实装。

## 3. `CGACT[COPY]` / 渲染到纹理 / 截图

三个实证点：

1. **离屏目标创建**：`FUN_0x462e40`（上游 render-semantics §2b，本次复核成立）——
   `CreateTexture(W=[0x80194c], H=[0x801950], 1, D3DUSAGE_RENDERTARGET, Format, POOL_DEFAULT, …)` +
   `SetRenderState(D3DRS_STENCILFUNC=26?…)` 组合；供 `CGACT[COPY]`（把组内合成结果复制到另一层）使用。
2. **帧末捕获**：帧入口第 9 步按 `W*4 + W*4*H` 分配缓冲 + `GetRenderTargetData`（32bpp 布局）——
   引擎内部"上一帧合成结果"的读回通道（`GETSS SetRenderTarget Error 1.`）。
3. **`FUN_00417384`（捕获+CPU 颜色调整）**：`GetRenderTargetData` → `LockRect(0x2000)` →
   **CPU 逐通道 LUT**：以参数 ARGB 各通道相对 0x80 的偏移构建 256 项/通道表
   （`lut[v] = (v·d + d·255)>>7 − (acc>>7)` 形状，中性点 0x80），按 BGRA 逐字节套用，alpha 原样 → `UnlockRect`。
   语义 = 捕获同时做一次亮度/对比类调整（中性 0x80）。对应哪条脚本命令（截图/CGACT 的哪一形态）= Unknown（挂号 G10）。

**对齐要求**：`CGACT[COPY]` 在本仓库 = "把指定组的合成结果落成纹理并赋给另一层"（render-to-texture + 纹理别名），
必须支持组边界（见 02 §2）；读回路径仅在脚本/调试需要时物化（性能超集：可整帧 GPU 内完成，仅对拍时读回）。

## 4. 顶点几何（绘制函数内联，Confirmed）

- 顶点缓冲 = 4 顶点 × 84 字节（`0x54`），`DrawPrimitiveUP(TRIANGLESTRIP, 2)`。
- 位置中间量走 **16.16 定点**：`ROUND(v×65536)` → 取整/拆分 → `×2^-16` 回浮点（常量 1.52587890625e-05 = 1/65536）。
- 旋转变换在 CPU：层旋转角 `×π/180`（常量 0x58ebf8）构建旋转矩阵，对 4 顶点行（步长 0x54×4=0x150）做矩阵乘。
- 顶点布局（浮点槽，来自填充助手 `FUN_0041a308`）：`[0..2]=x,y,z`、`[3]=1.0`、`[4]=打包色(含 A<<24)`、
  `[5..]=UV 对`；**UV 绘制方式**由全局 `DAT_00801334` 选：1=普通 / 2=重复平铺（`u += 1/W` 步进）/ 3=镜像平铺
  （`±1/W、2/W` 折返）—— 即 CG 有**平铺/镜像铺满**两种扩展绘制形态（对齐项，本仓库未实装）。
- FVF 精确标志未裁定（挂号 G5）；84 字节是 CPU 结构尺寸，FVF 声明可为其子集（DrawPrimitiveUP 只要求 stride 吻合）。

## 5. 挂号（续 03 §5）

| # | 项 | 下一步 |
|---|---|---|
| G9 | 掩码 `MID` 的取值通道（α/亮度）、掩码层是否要求同尺寸 | 找 stage1 配对纹理的生成者（`SetTexture(1,…)` 第二实参的来源链） |
| G10 | `FUN_00417384` 对应的脚本命令形态；`+0x1e8` 与 `+0x82` 的推进关系 | CG/CGACT 处理体反编译（同 G1 一样需定向补导出） |
| G11 | HLSL effect 的源/字节码与 profile（决定 11 种着色器模式与过渡着色器的精确公式） | 03 §4 恢复路径 (a)(b)(c) |
| G12 | 平铺/镜像 `DAT_00801334` 的脚本入口（CG 参数名） | 同 G1 |
