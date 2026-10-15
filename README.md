# Delicium

Windows 桌面角色运行时原型：透明 Overlay 常驻桌面，角色为 Delicium 原生格式的 2D 参数化模型与 3D 骨骼模型，不依赖现成游戏引擎（直接使用 wgpu / winit / egui / bevy_ecs）。第一阶段只面向 Windows，优先做好性能与表现质量，不承担既有外部模型格式的兼容。

## 第一阶段范围

- 窗口：透明 Overlay + 独立控制窗。默认仅角色几何区域接收点击，区域外穿透；可切换为整窗交互。控制窗始终保持可交互。
- 2D：原生参数（min / max / default）、rotation 与 warp 变形器、底色贴图与静态本地 polygon 遮罩。
- 3D：原生骨骼动画、FK、GPU 蒙皮、底色贴图、粗糙度/金属度基础材质、方向光与深度。
- 统一模拟：60 Hz 固定步长（16.666667 ms），每帧最多 4 个子步，渲染侧插值。
- 实例：最多 64 个；半径 2.5 以内的最近邻触发交互，支持同类与 2D/3D 跨类型响应。
- 资产：同一模型共享一份静态资产，各实例姿态与位置独立。
- 自主游走：普通启动时角色在场景可见水平范围内按静止/行走状态机自主移动，可由 `--no-wander` 关闭；只做水平方向，不含竖直游走、程序化步态与避障。
- 本地加载、重载、替换；失败时保留原有实例，不破坏当前画面。
- 默认启动 4 个实例（2 个 2D + 2 个 3D），示例为自制验证资产，不是最终美术质量。

## 构建与运行

前置条件：Windows 上的 Rust MSVC 工具链，构建依赖机器上已有的 Visual Studio 构建环境。

PowerShell（不要求修改全局 PATH，按绝对路径调用 cargo；若 cargo 已在 PATH 中可直接写 `cargo`）：

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
```

## 命令行

与 `src/app/cli.rs` 一致：

| 参数 | 说明 |
| --- | --- |
| `--model PATH` | 加载自包含的 `.dmodel.json` 模型，可重复，最多 64 个；不提供时加载两个内置示例 |
| `--backend auto\|dx12\|vulkan` | 选择 GPU 后端，默认 auto |
| `--overlay-size WIDTHxHEIGHT` | 显式有界场景尺寸（逻辑像素），允许 320x240 到 3840x2160；不提供时场景覆盖主显示器（`--smoke-test` 与 `--bench` 归一化为有界 960x640） |
| `--exit-after SECONDS` | 0.1 到 600 秒后自动退出 |
| `--smoke-test` | 使用内置示例运行交互冒烟序列，不能与 `--model` 合用；默认 12 秒退出，显式 `--exit-after` 不得少于 10 秒 |
| `--no-wander` | 关闭角色自主水平游走（普通启动默认开启）；`--smoke-test` 与 `--bench` 默认归一化为关闭游走（`--bench-wander` 除外） |
| `--bench 4\|16\|32\|64` | 显式启用内置双原生模型的本地基准；与 `--model`、`--smoke-test` 互斥 |
| `--bench-mode pixel\|interactive` | 基准的角色轮廓/整窗模式，默认 pixel；须与 `--bench` 合用 |
| `--bench-seconds SECONDS` | 预热 10 秒后测量 0.1 到 300 秒，默认 30 秒 |
| `--bench-reloads COUNT` | 交替替换同一实例 1 到 100 次，仅限 4 实例 pixel；与显式 `--bench-seconds` 互斥 |
| `--bench-wander` | 基准在漫游开启下运行（默认关闭，保持既有基线口径）；须与 `--bench` 合用；与 `--no-wander` 互斥 |
| `-h`, `--help` | 显示帮助 |

角色命中区域默认启用；场景空白区域穿透，角色可直接拾取和拖动。控制窗在两种场景输入模式下均保持可交互。

普通启动时角色在场景可见水平范围内自主游走，`--no-wander` 关闭；`--smoke-test` 与 `--bench` 默认归一化为静止，使其固定负载保持确定；`--bench-wander` 显式让基准在漫游开启下运行。

普通启动（未给 `--overlay-size`）时场景客户区覆盖主显示器物理矩形、无边距偏移：相机保持 640 逻辑像素高的原像素比例（竖直世界跨度 = 4.5 × 物理高 ÷ (640 × 缩放)，下界仍为 -0.9），扩大的是可移动区域而不是放大角色；控制窗锚定显示器右上（右边距 50 逻辑像素、顶边距 50 逻辑像素）。给出显式 `--overlay-size` 时保持原 960x640 风格的有界窗口与偏移布局。本轮只覆盖主显示器，多显示器跨度与拖过控制窗不作承诺；拖拽使用 winit 原生鼠标捕获，指针离开场景客户区（如主显示器边缘外）会取消拖拽。

## 控制窗

- `Add 2D` / `Add 3D`：新增一个示例角色。
- 角色列表：显示 `id: 名称 (类型)`，选中后显示位置并可用 x/y 拖动框调整。
- 邻接状态：显示是否正在响应附近角色。
- `Reload selected` / `Remove selected`：重载或移除选中角色。
- 本地 `.dmodel.json` 路径输入框 + `Load as new` / `Replace selected`：以新实例加载，或替换当前选中实例。
- 把一个模型文件拖放到任意一个窗口，都会新增一个角色。
- 角色命中区域复选框：勾选时，仅角色网格投影轮廓及边缘余量接收点击；取消勾选时，整个场景窗口接收点击。拖拽期间临时使用整窗区域，释放后恢复轮廓区域。
- 拖动会暂停被拖角色的游走，释放后恢复；释放时重新抽取一段静止时长，角色不会立刻走开。
- 命中区域使用 2D 变形网格与 3D 蒙皮网格的投影三角形并集，保留耳朵、四肢间隙；带 8px 轴向边缘余量，不是严格按 alpha、遮罩或抗锯齿逐像素命中。几何变化相对上次建区超过 4px，或拓扑、窗口尺寸变化时更新；游走使角色持续位移，达到该阈值的重建更频繁，但 8px 余量与这套缓存判定规则本身不变。
- 区域构建失败时报告错误并退回整窗穿透，不逐帧重试；切换复选框可重新尝试。
- 状态行（Frame ms | tick | interactions | pending）、GPU 适配器信息。
- 加载错误显示在控制窗内，可用 `Dismiss error` 关闭；`Quit` 退出程序。

## 模型格式

`format` 固定为 `delicium.model`，文件扩展名约定为 `.dmodel.json`。支持严格的 `0.1`（原颜色路径）与 `0.2`（增加内嵌 PNG、UV 与基础材质），未知版本和字段拒绝；`0.1` 不接受 `0.2` 的新字段。文档完全自包含：不含文件路径、URL、脚本或可执行内容，不引用外部资源。

顶层信封：

```json
{
  "format": "delicium.model",
  "format_version": "0.1",
  "name": "demo",
  "kind": "sprite2d",
  "sprite2d": { "...": "..." }
}
```

`kind` 为 `sprite2d` 时必须有 `sprite2d` 载荷且禁止 `skeleton3d`，反之亦然。可选的 `interaction` 声明能力绑定（2D 的 `gaze_parameter`，3D 的 `head_joint`），名称必须可解析，否则为加载错误。

普通启动与 `Add 2D` / `Add 3D` 使用 `assets/examples/puppet2d-textured.dmodel.json` 与 `assets/examples/robot3d-textured.dmodel.json`。原 `puppet2d.dmodel.json` / `robot3d.dmodel.json` 保留为 `0.1` 示例、冻结图像基线和 benchmark/reload 工作负载；两对来源使用独立缓存槽，按需加载。

### 2D（`sprite2d`）

- `parameters`：标量参数，按名称唯一。
- `layers`：按绘制顺序排列；几何来源是显式 `vertices` + `indices`，或 `grid`（`bounds` + `columns` + `rows`，生成网格）。两者互斥。`0.2` 每层可用 `texture` 引用图片，`color` 作为 tint；无贴图时仍为原纯色路径。
- `uvs`（`0.2`）：每顶点一对 `[u,v]`。显式贴图网格必须提供；grid 未提供时按初始网格生成。美术 UV 随网格旋转/warp 保持绑定，与由变形后位置计算的 polygon mask UV 分离；纹理和 UV 不在每帧重新上传。
- `deformers`：`type` 为 `"rotation"`（`pivot` + `max_angle_radians`）或 `"warp"`（`bounds` + 控制网格 `grid` + 位移 `offsets`），可用 `parent` 串联，由 `parameter` 驱动。
- `mask`：可选的本地 polygon 遮罩（`points`，隐式闭合）。
- `animations`：驱动参数的循环动画，channel 按参数唯一，key 时间严格递增。

### 3D（`skeleton3d`）

- `vertices`：绑定空间顶点，含 `position` / `normal` / `color` 与 4 个关节索引 + 4 个权重；权重非负，编译时归一化。
- `indices`：三角形列表。
- `uvs` / `texture`（`0.2`）：直接位于 `skeleton3d`，不是 `mesh` 子对象；贴图时 UV 数量与 `vertices` 一致，UV 随 GPU 蒙皮保留。
- `material`（`0.2`）：单个整体基础材质，`roughness` / `metallic` 范围 `[0,1]`，默认 `0.6` / `0`；`alpha_mode` 为 `"opaque"`（默认）或 `"cutout"`；cutout 的 `alpha_cutoff` 默认 `0.5`、范围 `(0,1]`，opaque 文件不可声明该字段。新材质用固定方向光的 GGX/Schlick 表面响应；贴图未声明材质时采用默认材质，无贴图也无材质时保留原 Lambert 数学路径。
- 新 `opaque` 要求图片与顶点 alpha 均为 1；`cutout` 对图片 alpha × 顶点 alpha 比较 cutoff，低于阈值丢弃且不写深度，通过后不透明输出。首轮不支持一般半透明 3D 排序或多材质子网格。
- `joints`：按名称引用父关节，声明顺序任意（编译器会重排），含 `translation` / `rotation`（四元数）/ `scale`。
- `animations`：关节动画；每个 channel 指定 `joint` 与 `interpolation`（`"linear"` 或 `"step"`），并分别给出 translation / rotation / scale 轨道。

向量约定：2D 点与偏移为 `[x, y]`，warp 边界为 `[x, y, width, height]`；3D 为右手系、Y 轴向上，旋转写作四元数 `[x, y, z, w]`；颜色为直通 RGBA，取值 `[0, 1]`。名称最长 128 个字符。

### 图片、UV 与颜色（`0.2`）

顶层可声明 `images: [{"id":"paint","png_base64":"..."}]`，`texture` 精确引用 ID。ID 唯一且必须被使用；缺失引用、重复 ID、未使用图片拒绝。仅接受标准 Base64 的普通 PNG，不接受 data URI、外部文件或 URL；支持不高于 8-bit 的灰度、palette、RGB/RGBA 和透明信息展开，拒绝 16-bit 与 APNG，检查 CRC、完整帧及结束数据。

UV 有限且位于 `[0,1]`，左上为 `[0,0]`，clamp + linear 采样。底色字节按 sRGB 解释，不执行 ICC 转换。解码在线程 loader 完成：RGB 先转线性空间乘 alpha，再编码为 sRGB；GPU 使用 `Rgba8UnormSrgb`，2D 乘 tint 与遮罩而不重复乘图片 alpha，3D 安全恢复 straight 底色再光照。原 literal 颜色路径不被全局重新解释。

可证明全透明的层、或整个 cutout 模型均低于阈值时，不贡献命中轮廓；局部贴图透明孔与 cutout 孔仍按三角几何命中，不是严格 alpha 点击穿透。

### 预算（原型档位）

预算在创建任何 GPU 资源之前检查，且可调整，属于原型档位而非永久格式限制。默认值：载荷 8 MiB；顶点 50 000、索引 150 000；关节 128；层 64、参数 64；变形器 128；单个遮罩 64 点；网格单轴最长 64；单个 channel 4 096 key、单个动画 1 024 channel、模型 32 个动画、动画 key 总计 262 144。

图片默认预算为每模型 8 张、宽高各不超过 2048、单张最终 RGBA8 不超过 16 MiB、每模型合计 32 MiB，单次 PNG decoder 内部 limits 为 16 MiB。2048²×4 正好 16 MiB；原 8 MiB JSON/Base64 文件预算仍可能先限制低压缩率图片。维度、checked 字节乘法及单张/累计预算在输出大缓冲分配前检查；PNG 库 limits 是 best effort，不包括 caller 输出缓冲，也不代表全进程硬内存上限。

loader 保留两条容量 4 的有界队列和应用 pending 上限 4。仅按结果队列 4 份加 worker 1 份估算，编译结果 RGBA 载荷的保守包络为 5×32 MiB=160 MiB，应用 pending 另有总数约束；这不是进程峰值，不含现存资产、JSON/Base64、decoder 内部缓冲、单图 Vec 转 Arc 临时复制或 GPU 资源。临时解码缓冲随函数返回释放，实例与帧共享不可变图像。

渲染器模型纹理逻辑预算为 128 MiB，包含所有已登记图片及当前候选，替换时不提前减去旧资产。满预算下即使候选更小也可能拒绝替换，旧画面保持；成功卸载归还预算。同模型图片由多层、多实例共享，资产持有纹理，不使用全局去重缓存。纹理逻辑字节不包括固定 fallback、遮罩、几何和帧缓冲，不等于驱动实际显存。

### 本地路径检查

文件加载会拒绝以 `\\` 或 `//` 开头的路径。这只是一个本地路径检查，并不等于完整的文件系统沙箱，不构成对映射路径或链接路径的全面禁止。

## 测试与检查

```powershell
& "$HOME/.cargo/bin/cargo.exe" test
& "$HOME/.cargo/bin/cargo.exe" test --test gpu_render
& "$HOME/.cargo/bin/cargo.exe" clippy --all-targets -- -D warnings
& "$HOME/.cargo/bin/cargo.exe" fmt --check
& "$HOME/.cargo/bin/cargo.exe" run --release -- --smoke-test
```

`cargo test` 覆盖模型管线、模拟和坐标映射；`tests/gpu_render.rs` 通过真实 GPU 离屏渲染检查透明、遮罩、绘制顺序和蒙皮。Windows 下的 lib 测试还将同一动画帧中所有 alpha 非零的 GPU 像素与真实 GDI 区域逐点比对，覆盖区域重建、缓存复用和未超过 4px 的累计漂移。

原生区域采用流式平衡 `RGN_OR` 合并，只改变合并次序，不改变几何、填充规则或缓存阈值；不会一次创建全部多边形句柄。Windows 测试以原线性算法为参考，检查新旧区域的 `RGN_XOR` 是否为空，并包含能识别区域差异的反例、10,512 多边形句柄峰值与错误释放检查；冻结 GPU 几何也执行精确区域等价断言。

Windows 合成与输入验证（需要窗口的脚本会启动自己的纯色背景和应用窗口；带截图的脚本仅截图应用区域，`windows_wander.ps1` 设计上不截图）：

```powershell
& "$HOME/.cargo/bin/cargo.exe" build
pwsh -NoProfile -File tests/windows_desktop.ps1
pwsh -NoProfile -File tests/windows_smoke.ps1
pwsh -NoProfile -File tests/windows_pixel_hit.ps1
pwsh -NoProfile -File tests/windows_wander.ps1
```

`windows_desktop.ps1` 验证普通启动的桌面范围：场景客户区与主显示器物理矩形精确相等、控制窗在屏内并按右上锚点定位、桌面模式拖拽越过旧有界窗口右边缘（12 步物理拖拽，世界位移与像素比一致）、释放后区域与空白穿透恢复、无 fallback；该脚本设计上不截图。`windows_smoke.ps1` 覆盖控制窗操作与完整 GUI 生命周期（显式 `--overlay-size 960x640` 保持有界布局）；`windows_pixel_hit.ps1` 专门检查默认轮廓模式下的空白/耳间/腿间穿透、角色拖拽、释放恢复、窗口层级与模式切换（同样显式有界）。这三个既有回归脚本显式传入 `--no-wander` 保持固定锚点；`windows_wander.ps1` 单独覆盖普通启动的自主水平游走、拖动期间暂停、释放后恢复，以及游走下的命中区域与穿透行为。两套截图脚本在客户区等于主显示器矩形时拒绝截图，防止把桌面范围误当成有界场景捕获；`windows_wander.ps1` 不截图。几何单测负责冻结帧的区域覆盖，动画截图仅验证角色存在和透明合成，不能替代同姿态无裁剪证明。

验证日志和应用区域截图保留在每次新建的 `.artifacts/` 子目录；测试背景与应用在结束时关闭，鼠标释放并恢复光标。四套脚本均支持 `-ValidateOnly`，只编译测试框架而不启动窗口。

### 贴图与基础材质验收（2026-10-14）

- fmt、离线 locked clippy（全部 target，warnings 为错误）、debug/release 构建通过。
- 88 项核心/集成测试通过，包含严格双版本、PNG/Base64、UV/材质/预算、透明命中以及旧模型/模拟回归；冻结 25 帧 GPU/GDI 区域覆盖继续通过。
- 8 项 GPU 测试在 NVIDIA RTX 5070 Laptop 的 Vulkan 后端通过，无失败或忽略。包含旧图像回归、UV/蒙皮、sRGB 线性预乘、cutout 深度孔、100 轮纹理生命周期、共享与静态上传计数、128 MiB 旧资源加候选预算及失败保旧。
- 5 组 roughness/metallic 指定像素与独立 f64 参考一致，含 roughness=0 端点；不是只检查材质字段存在或画面非黑。

两套 Windows GUI 使用同次新 debug 构建全部通过：`windows_smoke.ps1` 52 项、`windows_pixel_hit.ps1` 44 项，无命中区域 fallback。窗口端使用 AMD Radeon DX12 硬件；真实场景截图确认两类新贴图可见，增删、拖拽、旧文件加载/重载及失败保留回归通过。

离线日志：`.artifacts/texture-verify-20261014-162436-5e67ef/logs/`；GUI 包装日志：`.artifacts/texture-verify-20261014-164447-2adc9cee/`；场景截图及应用日志：`.artifacts/windows-20261014-164544-9603/`、`.artifacts/windows-pixel-20261014-164618-6595/`。

### 桌面范围验收（2026-10-14）

- fmt、离线 locked clippy（全部 target，warnings 为错误）、debug/release 构建通过。
- 93 项核心/集成测试通过（lib 50、model_pipeline 15、simulation 9、render_coordinates 6、textured_model 13），包含新增的 LEGACY 逐值一致（bit 级相等）、`desktop_view` 公式（2560×1600@1.5 → span 7.5、像素比 213.3）与区域缓存 view 失效测试；8 项 GPU 测试在 NVIDIA RTX 5070 Laptop / Vulkan 通过。
- 三套 GUI 使用同次新 debug 构建按序通过：`windows_desktop.ps1` 51 项断言（客户区 == 主显示器物理矩形、控制窗屏内右上锚点、12 步拖拽越过旧有界右边缘且 Δworld 4.767 与像素比一致、释放后区域与穿透恢复、无 fallback），`windows_smoke.ps1` 52 项、`windows_pixel_hit.ps1` 44 项与既有有界基线一致；`windows_bench.ps1 -ValidateOnly` 55 项自检通过（未跑基准用例）。
- 实测场景视图：span 7.5（= 4.5 × 1600 ÷ (640 × 1.5)，与有界 960 物理高的像素比相同），bottom -0.9。

验证工件：`.artifacts/span-verify-20261014-235749/`（logs + results）；GUI 运行目录：`.artifacts/windows-desktop-20261015-000002-2114/`、`.artifacts/windows-20261015-000015-6981/`、`.artifacts/windows-pixel-20261015-000035-9863/`。

收尾定点审查（窗口/输入归属、坐标与 DPI、capture 守卫、区域缓存）未发现运行时缺陷；3 项已确认发现仅在验收脚本 `windows_desktop.ps1` 内——控制窗边缘余量按物理像素硬编码、`span > 4.5` 前提、拖拽目标布置隐含显示器宽度下限，均为非 1.5 缩放或小屏配置下的误报失败。已改为按 DPI 与显示器宽度自适应（余量上界 70×scale；拖动目标 `min(legacyRight+300, Right-20)` 且断言越过旧边缘，条件不足时 fail-closed），同一二进制复跑 51 项通过：`.artifacts/desktop-harness-rerun-20261015-004754/`（应用运行目录 `windows-desktop-20261015-004825-7184`）。

## 本地性能与资源基线

先构建 release，再运行专用脚本（默认使用 `target/release/delicium.exe`）：

```powershell
& "$HOME/.cargo/bin/cargo.exe" build --release --locked --offline
pwsh -NoProfile -File tests/windows_bench.ps1 -ValidateOnly
pwsh -NoProfile -File tests/windows_bench.ps1 -Quick
pwsh -NoProfile -File tests/windows_bench.ps1
pwsh -NoProfile -File tests/windows_bench.ps1 -Wander
```

`-ValidateOnly` 只检查原生绑定和结果协议，不启动应用；`-Quick` 是 4 实例 pixel 的短接线检查，仍预热 10 秒。默认完整运行按顺序测试 4/16/32/64 实例的 pixel/interactive 模式（各测量 30 秒）、64 实例 pixel 持续 300 秒，以及 4 实例 pixel 同一目标交替 2D/3D 替换 100 次。替换必须逐次加载成功并在主场景提交中出现新资产，最后稳定观察 10 秒。`-Wander` 让每个用例附加 `--bench-wander`，在自主游走开启下运行同一负载，目录前缀变为 `windows-bench-wander-`；终态 config 与 run plan 均记录 wander 溯源，默认（不带 `-Wander`）保持既有静止口径。

基准只使用内置示例，初始两种模型各半、各共享一份静态资产，保持真实动画、最近邻互动与固定步长模拟。高数量有重叠和过绘，不代表任意复杂模型的负载。脚本拒绝已有 Delicium 实例，只采样和管理自己启动的进程，不模拟输入、不截图、不联网；请在测试期间保持实例、输入模式、窗口尺寸和 DPI 不变。

每次生成新的 `.artifacts/windows-bench-*` 目录，保留配置、阶段事件、终态结果、约 1Hz 的进程资源样本及 JSON/CSV/Markdown 汇总。资源采样从自有子进程的可见主窗就绪后开始，记录就绪时间，30 秒未就绪则失败；就绪后查询失败仍保留 null 与错误码并使场景失败，不填成 0。普通运行不启用采样或额外阶段计时。

测量口径：

- 场景与控制窗回调、模拟、几何、原生区域构造/安装、acquire、编码/上传、submit、present 均为主机侧墙钟；等待或驱动背压可能包含其中。回调与子阶段有包含关系，不能相加当作总 CPU 时间。
- p50/p95/p99 从完整有效样本用 nearest-rank 计算；未执行阶段为 count=0、耗时 null，不填成 0ms。帧间隔另行统计，包含 surface skip。
- 提交率是主场景提交/呈现请求次数除以真实测量时长，不是显示器实际呈现帧率。`measured_seconds` 是实际测量阶段时长之和：普通场景为 measure，替换场景为 reload_baseline + reload + settle。事件循环在回调边界结束阶段，可以略超目标时长；校验要求不短于目标且与阶段和一致，不要求精确等于目标值。
- 进程 CPU 核秒除以墙钟得到平均使用核数；PrivateBytes、WorkingSet、句柄、GDI/USER 是进程资源。采样缓冲预先分配并触碰，场景固定容量 65,536、控制窗 131,072，测量中不扩容，容量溢出使结果无效；`allocated_bytes` 按实际容量与样本大小报告。控制窗容量增加用于容纳快速场景的 300 秒持续测量，对照旧基线时需单列固定测量缓冲增量，不误归为区域算法内存。
- GPU 执行时间与实际 WDDM 显存未测量，`gpu_execution_ms=null`。GPU 资产数、遮罩逻辑字节及 GrowBuffer 容量只是应用资源诊断，不是显存。
- 资产期望集合为存活实例引用与最新 examples 缓存的并集。本替换布局前两次可从 2 增至 3、4，此后保留 4 正常；内存不回落到起点也不能单凭一轮数据判定泄漏。

### 2026-10-14 基线结果

完整 10 组测量通过，原始 stdout 终态、阶段时长、计数、资产诊断和资源 CSV 已核对。配置为 release、AMD Radeon(TM) Graphics 集成 GPU、DX12、Fifo、PreMultiplied、Bgra8UnormSrgb；场景逻辑尺寸 960×640、物理尺寸 1440×960，控制窗物理尺寸 570×1020，两窗 DPI scale 均为 1.5。以下结果只对应这组配置与内置压力场景。

该基线及下节优化对照均为 `--no-wander` 等价条件下的静止负载：`--bench` 把游走归一化为关闭，角色全程固定不动。2026-10-15 已用同一构建补做关闭/开启两轮对照（见下文「2026-10-15 漫游开启对照」）；本节数字仍为关闭口径。

| 实例 | pixel 提交/s | interactive 提交/s | pixel 场景回调 p50 ms | 原生建区/安装 p50 / p95 ms | 平均使用核数 pixel / interactive |
| --- | --- | --- | --- | --- | --- |
| 4 | 54.06 | 53.40 | 5.889 | 4.765 / 5.472 | 0.369 / 0.128 |
| 16 | 34.20 | 53.23 | 28.087 | 24.908 / 28.581 | 0.987 / 0.147 |
| 32 | 19.72 | 53.53 | 50.047 | 43.978 / 46.654 | 0.999 / 0.173 |
| 64 | 9.67 | 53.63 | 103.440 | 91.450 / 95.650 | 0.994 / 0.203 |

平均使用核数取各自 measure 阶段的进程 CPU 增量除以采样墙钟，不包含启动/预热；不是按 24 个逻辑核归一后的百分比。interactive 不执行几何命中/原生建区阶段，其场景回调 p50 从 0.426ms 增至 1.438ms。

原生区域构造、合并及安装（不是单独 `SetWindowRgn`）的平均墙钟占 pixel 场景回调平均墙钟约九成。16/32/64 实例在测量期间每次场景尝试都重建区域，缓存复用为 0；4 实例为 1398 次重建、224 次复用。首要优化方向是降低原生区域构造/合并成本和不必要的整体重建，而不是先改模型格式或推断 GPU 瓶颈；本轮未实施优化。

64 实例 pixel 的 30.083 秒测量实际执行 1164 个模拟步、丢弃 641 步。持续测量 300.053 秒时提交率为 9.69/s，执行 11632 步、丢弃 6372 步，约占应执行步数的 35%；其他 8 组丢步为 0。因此这里的 PASS 表示测量完整有效，不表示 64 实例轮廓模式已经达到实时性能目标。

资源与替换观察：

- 全部 681 条进程资源样本约 1Hz，查询错误为 0；场景/控制采样均未达到各 65,536 条容量，没有区域失败、surface skip 或场景错误。
- 64 实例持续场景，measure 内前后各 10 秒窗口的 PrivateBytes 均值为 383.79→385.68 MiB（+1.89 MiB），WorkingSet 为 420.44→423.51 MiB（+3.07 MiB），平均使用核数约 1.00→1.00。句柄数首末为 368→358、峰值 369；GDI 峰值 15、末值 13，USER 为 20。
- 100 次交替替换的请求、成功及新资产帧提交确认均为 100，唯一请求/资产各 100，始终是同一目标实例。CPU/GPU/期望资产数一致，从初始 2 到首次替换的 3、第二次的 4，之后保持 4，符合存活引用与 examples 缓存的保留策略。
- 替换前 5 秒 baseline 与结束后 10 秒 settle 各阶段资源均值：PrivateBytes 371.35→374.46 MiB，WorkingSet 409.46→412.73 MiB；并非进程出生/退出的首末样本。替换场景合计 measured_seconds=39.915，包含 baseline、替换和 settle，提交率 53.76/s。这些变化不能单独证明或排除泄漏；实际显存与 GPU 执行时间仍未测量。

原始结果：`.artifacts/windows-bench-20261014-001248-1518-811d755b/`（`summary.md/json/csv` 与逐场景 `stdout.log`、`result.json`、`resources.csv/json`）；协调日志：`.artifacts/perf-baseline-20261014-001242-56061/01-full-baseline.log`。此前脚本启动采样/时长契约失败的产物另行保留，没有改写为通过。

### 2026-10-14 平衡合并优化结果

流式平衡合并已完成，同一配置的单次完整 10 组对照通过。轮廓集合、8px 余量、4px 成功基线缓存和模拟追帧上限保持不变；没有通过降低更新频率或粗化几何换取收益。

| 实例 | 原生建区 p50 ms 基线 → 优化后 | p50 降低 | 优化后 p95 ms | pixel 提交/s 基线 → 优化后 | 本轮 interactive 提交/s |
| --- | --- | --- | --- | --- | --- |
| 4 | 4.765 → 4.071 | 14.6% | 5.362 | 54.06 → 52.92 | 52.89 |
| 16 | 24.908 → 13.956 | 44.0% | 17.834 | 34.20 → 53.06 | 52.90 |
| 32 | 43.978 → 27.627 | 37.2% | 32.418 | 19.72 → 27.63 | 53.16 |
| 64 | 91.450 → 52.045 | 43.1% | 54.107 | 9.67 → 15.30 | 53.15 |

4 实例虽原生耗时下降，但本轮提交率未提升；16 实例已接近当前整窗对照的提交水平。优化后 pixel 场景回调 p50 为 5.299/17.356/34.302/64.051ms；16/32/64 仍约使用一个 CPU 核，原生阶段占其回调平均墙钟约 81%。64 实例仍不是高刷新率运行水平，提交率也不等于显示帧率。

64 实例短测 30.059 秒：1798 个模拟步、丢 5 步（原为 1164/641）。持续测量 300.027 秒：15.24 次提交/s、17913 个模拟步、丢 88 步（原为 9.69/s、11632/6372），丢步占应执行步数约 35.39%→0.49%；没有调整固定步长或追帧上限。

正确性验证：71 项核心/集成测试、3 项 GPU 测试、25 帧区域覆盖（8 次复用/17 次重建）、新旧区域精确等价和 10,512 多边形单次构建自有 HRGN 峰值不超过 15 的测试通过；fmt/clippy、debug/release 构建通过，模块顺序修正后的 9 项原生区域窄回归通过。两套原 GUI 回归分别 52/44 项通过，独立定点审查未发现缺陷。

资源与替换：

- 680 条资源样本与 666 份诊断 snapshot 已核对；没有查询错误、区域失败、scene error/skip、采样溢出或进程干扰。固定采样缓冲实际为 16,252,928 字节（15.5 MiB），比基线多 1.5 MiB，仅基准模式分配；本轮最大 control_used 为 9,542。
- 持续场景 measure 内首尾各 10 秒窗口的 PrivateBytes 均值 384.91→388.15 MiB（+3.24），WorkingSet 422.07→424.78 MiB（+2.71），CPU 约 1.00 核；进程句柄首末 368→358、峰 369，GDI 末 23/峰 27，USER 20。平衡合并增加瞬时自有区域句柄，而非一次持有全部多边形；与旧运行比较绝对内存时须单列固定测量缓冲增量，单轮趋势不能证明或排除泄漏。
- 100 次交替替换均完成且由真实新资产帧提交确认，唯一请求/资产各 100；CPU/GPU/期望资产仍 2→3→4，此后 4。替换前 5 秒 baseline 与后 10 秒 settle 的 PrivateBytes 均值 372.71→375.79 MiB，WorkingSet 410.76→412.77 MiB，非等长窗口；实际 GPU 执行时间和显存仍未测。

本轮原始结果：`.artifacts/windows-bench-20261014-100634-5162-85112620/`；协调日志与主代理逐项核实的对照数据：`.artifacts/perf-final-20261014-100627-54792/01-full-baseline.log`、`.artifacts/perf-final-20261014-100627-54792/main-native-comparison.json`。原基线和失败产物保持不变。

### 2026-10-15 漫游开启对照

自主游走加入后的首轮对照测量：同一 release 二进制、同一机器会话，先跑关闭态（`windows_bench.ps1` 默认）完整 10 组，再跑开启态（`-Wander`，等价于每个用例加 `--bench-wander`）完整 10 组；两轮均 `pass=true cases=10`，产物目录前缀分别为 `windows-bench-` 与 `windows-bench-wander-`，run plan 与逐用例终态 config 的 `wander` 溯源逐项为 false/true。只测量、未优化；不改阈值、不新增计数器。

| 实例 | pixel 提交/s 关→开 | native 建区 p50 ms 关→开 | native p95 ms 关→开 | 场景回调 p50 ms 关→开 | 重建/复用 关→开 | 丢弃模拟步 关→开 |
| --- | --- | --- | --- | --- | --- | --- |
| 4 | 53.56 → 54.16 | 3.737 → 3.866 | 4.221 → 4.327 | 4.907 → 5.066 | 1385/222 → 1432/193 | 0 → 0 |
| 16 | 54.61 → 50.21 | 13.420 → 14.788 | 16.095 → 18.564 | 16.652 → 18.465 | 1639/0 → 1507/0 | 0 → 0 |
| 32 | 30.58 → 25.86 | 25.724 → 29.583 | 27.161 → 35.273 | 31.691 → 36.747 | 918/0 → 776/0 | 0 → 0 |
| 64 | 15.54 → 13.13 | 51.262 → 58.559 | 53.316 → 72.324 | 63.118 → 73.251 | 467/0 → 394/0 | 0 → 225 |
| 64 持续 300s | 15.43 → 12.61 | 51.739 → 59.858 | 54.096 → 78.414 | 63.523 → 74.826 | 4628/0 → 3784/0 | 51 → 2868 |
| 4 替换 100 次 | 53.48 → 53.52 | 3.580 → 4.516 | 4.078 → 6.899 | 4.670 → 5.862 | 1913/221 → 1974/162 | 0 → 0 |

整窗 interactive 对照的提交/s 基本不变（52.96/53.16/53.00/53.39 → 53.16/53.22/53.46/53.12）：整窗路径不执行几何与原生区域工作。其回调 p50 在 16/32/64 由 0.72/0.99/1.48ms 升至 0.80/1.11/1.73ms（+11%~+17%），4 实例略降（0.537→0.513）；该差异不单独归因，候选因素包括游走改变实例空间分布从而影响最近邻互动次数，以及背靠背第二轮的热/环境漂移，本轮数据不足以拆分。

观察（只记录，未实施优化）：

- 游走行走速度 0.25 世界单位/s；该有界口径下 1 世界单位约 213.3 物理像素，4 实例帧间隔（p50 16.9ms）中游走位移约 0.9 物理像素，低于 4px 缓存阈值，64 实例帧间隔（p50 74ms）下约 4 物理像素、与阈值同量级。4 实例的缓存复用没有像预期那样塌缩：复用 222→193 次，复用占比 13.8%→11.9%（pixel）与 10.4%→7.6%（替换场景）；关闭态每次场景尝试已有 86% 触发重建（1385/1607），该口径下缓存大多由动画变形而非位移跨过 4px 阈值。
- 16/32/64 关闭态本就每次尝试都重建、复用为 0；开启后重建次数同比例减少（-8%~-18%，与提交率降幅一致），因为单次原生建区更贵、帧更长，每秒提交与重建机会随之变少。单次原生 p50/p95 上升（64：51.3→58.6 / 53.3→72.3 ms，+14%/+36%）。该上升不单独归因：候选因素是游走改变角色空间分布使并集轮廓过程更贵，以及背靠背第二轮的热/环境漂移（后者在整窗对照也有 +11%~+17% 的同向信号）。
- 64 实例质量代价最明显：短测丢弃 225 步（关闭态 0），持续 300s 丢弃 2868 步（关闭态 51），持续丢弃占应执行步数约 15.9%（关闭态约 0.3%），提交率下降 18.2%；高实例数轮廓路径在游走下的原生区域成本仍是首要瓶颈，与关闭态结论一致且恶化。
- 资源侧无明显差异：两轮 680+680 条样本、669+668 份诊断 snapshot 全部无查询错误/采样溢出/区域失败。持续场景 measure 首尾 10 秒窗口 PrivateBytes 386.77→387.90 MiB（开）对 385.97→387.93 MiB（关），句柄与 CPU 核数同量级。4 实例替换 100/100/100/100 均完成，两轮资产守恒一致。GPU 执行时间与显存仍未测量。
- 方法学注记：两轮同会话背靠背、先关后开，第二轮偏热属已知偏差；两轮 `RUST_LOG=warn` 相同，子进程 IO 姿态一致。本轮只覆盖 bench 960×640 有界口径，不覆盖 desktop-span 铺满口径的漫游负载。

原始结果：关闭态 `.artifacts/windows-bench-20261015-185533-7523-f906f84f/`、开启态 `.artifacts/windows-bench-wander-20261015-191048-3740-31e488e9/`；协调目录 `.artifacts/perf-wander-20261015-185529/`（`01-wander-off.log`、`02-wander-on.log`、`00-validate-only.log`、`main-wander-comparison.json`、`verify-wander-comparison.py`）。harness 自检 `-ValidateOnly` 由 55 增至 57 项（新增 wander 失配 fixture 与用例参数冻结检查）。

最终离线全门通过（同目录 `03-full-gate.log`，`EXIT_CODE=0`）：fmt、locked/offline clippy（全 target，warnings 为错误）、107 项非 GPU 测试（lib 56 + model_pipeline 15 + simulation 17 + render_coordinates 6 + textured_model 13，`--test-threads=1`）、8 项 GPU 测试（NVIDIA RTX 5070 Laptop / Vulkan，无忽略）、debug 与 release 构建。

## 当前状态与范围外内容

- 2026-10-13 本地轮廓验收已通过：fmt、离线 locked clippy、49 项单元/集成测试、3 项 GPU 回归测试、构建，以及两套真实 Windows GUI 脚本。轮廓覆盖测试在硬件 GPU 上检查了 25 个同帧动画样本（8 次缓存复用、17 次重建），另验证视口边界重建和 3 个缓存漂移样本的全部可见像素。`windows_smoke.ps1` 保留增删、拖拽、加载、重载与失败保留覆盖；`windows_pixel_hit.ps1` 验证空白/耳间/腿间物理点击穿透、默认模式拖拽与释放恢复、窗口层级及模式切换。
- 2026-10-14 性能/资源基线与平衡原生区域合并优化的完整 10 组对照均通过；64 实例 pixel 提交率由约 9.7/s 提升至 15.3/s，持续模拟丢步由约 35.39% 降至 0.49%，高实例数仍有性能限制，结果见上文。
- 2026-10-15 漫游开启对照完成：同一构建关闭/开启两轮各 10 组通过（`--bench-wander`/`-Wander`，run plan 与终态 config 双层溯源）；整窗吞吐不变，轮廓路径 16/32/64 提交率下降约 8%-18%，64 实例持续丢弃模拟步约 0.3%→15.9%；资源侧无差异，4 实例复用未塌缩；结果见上文，优化留后续。
- 2026-10-14 桌面范围（普通启动铺满主显示器物理矩形、控制窗右上锚点、拖拽越旧有界边缘）实施完成并全门验证通过：93 项核心/集成、8 项 GPU、三套 GUI（51/52/44）与 bench 自检 55 项，见上文。只覆盖主显示器；多显示器跨度、拖过控制窗、性能基线重测留后续。
- 无 GLB 或其他外部格式导入。
- 自主游走只覆盖水平方向：竖直游走、程序化步态（行走动画与位移不同步驱动）与避障不在范围内。
- 无联网、商店、账号、支付、SQLite；联网需求属于后续工作。
- 无可视化编辑器。
- 无 IK、物理、morph；基础 GGX/Schlick 材质尚无 IBL、阴影、法线贴图、mipmap/各向异性，不是最终 PBR 管线。
- 遮罩是静态本地多边形，当前不随变形器变形。
- 非 Windows 平台不在第一阶段承诺内（Windows 首发）。
- 示例模型是自制验证资产，不代表最终美术质量。
