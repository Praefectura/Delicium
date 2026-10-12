# Delicium

Windows 桌面角色运行时原型：透明 Overlay 常驻桌面，角色为 Delicium 原生格式的 2D 参数化模型与 3D 骨骼模型，不依赖现成游戏引擎（直接使用 wgpu / winit / egui / bevy_ecs）。第一阶段只面向 Windows，优先做好性能与表现质量，不承担既有外部模型格式的兼容。

## 第一阶段范围

- 窗口：透明 Overlay + 独立控制窗。Overlay 可切换点击穿透；控制窗始终保持可交互，不随 Overlay 一起穿透。
- 2D：原生参数（min / max / default）、rotation 与 warp 变形器、静态本地 polygon 遮罩。
- 3D：原生骨骼动画、FK、GPU 蒙皮、基础方向光与深度。
- 统一模拟：60 Hz 固定步长（16.666667 ms），每帧最多 4 个子步，渲染侧插值。
- 实例：最多 64 个；半径 2.5 以内的最近邻触发交互，支持同类与 2D/3D 跨类型响应。
- 资产：同一模型共享一份静态资产，各实例姿态与位置独立。
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
| `--overlay-size WIDTHxHEIGHT` | 场景逻辑像素尺寸，默认 960x640，允许 320x240 到 3840x2160 |
| `--exit-after SECONDS` | 0.1 到 600 秒后自动退出 |
| `--smoke-test` | 使用内置示例运行交互冒烟序列，不能与 `--model` 合用；默认 12 秒退出，显式 `--exit-after` 不得少于 10 秒 |
| `-h`, `--help` | 显示帮助 |

场景处于点击穿透状态时，控制窗仍然可交互。

## 控制窗

- `Add 2D` / `Add 3D`：新增一个示例角色。
- 角色列表：显示 `id: 名称 (类型)`，选中后显示位置并可用 x/y 拖动框调整。
- 邻接状态：显示是否正在响应附近角色。
- `Reload selected` / `Remove selected`：重载或移除选中角色。
- 本地 `.dmodel.json` 路径输入框 + `Load as new` / `Replace selected`：以新实例加载，或替换当前选中实例。
- 把一个模型文件拖放到任意一个窗口，都会新增一个角色。
- `Scene click-through` 开关控制 Overlay 是否点击穿透。
- 状态行（Frame ms | tick | interactions | pending）、GPU 适配器信息。
- 加载错误显示在控制窗内，可用 `Dismiss error` 关闭；`Quit` 退出程序。

## 模型格式

`format` 固定为 `delicium.model`，`format_version` 固定为 `0.1`，文件扩展名约定为 `.dmodel.json`。文档完全自包含：不含文件路径、URL、脚本或可执行内容，不引用外部资源；任意位置出现未知字段都会被拒绝，新版本文件不会被半加载。

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

完整可用示例见 `assets/examples/puppet2d.dmodel.json` 与 `assets/examples/robot3d.dmodel.json`（编译进可执行文件，未传 `--model` 时默认加载）。

### 2D（`sprite2d`）

- `parameters`：标量参数，按名称唯一。
- `layers`：按绘制顺序排列；每层为纯色，几何来源是显式 `vertices` + `indices`，或 `grid`（`bounds` + `columns` + `rows`，生成网格）。两者互斥。
- `deformers`：`type` 为 `"rotation"`（`pivot` + `max_angle_radians`）或 `"warp"`（`bounds` + 控制网格 `grid` + 位移 `offsets`），可用 `parent` 串联，由 `parameter` 驱动。
- `mask`：可选的本地 polygon 遮罩（`points`，隐式闭合）。
- `animations`：驱动参数的循环动画，channel 按参数唯一，key 时间严格递增。

### 3D（`skeleton3d`）

- `vertices`：绑定空间顶点，含 `position` / `normal` / `color` 与 4 个关节索引 + 4 个权重；权重非负，编译时归一化。
- `indices`：三角形列表。
- `joints`：按名称引用父关节，声明顺序任意（编译器会重排），含 `translation` / `rotation`（四元数）/ `scale`。
- `animations`：关节动画；每个 channel 指定 `joint` 与 `interpolation`（`"linear"` 或 `"step"`），并分别给出 translation / rotation / scale 轨道。

向量约定：2D 点与偏移为 `[x, y]`，warp 边界为 `[x, y, width, height]`；3D 为右手系、Y 轴向上，旋转写作四元数 `[x, y, z, w]`；颜色为直通 RGBA，取值 `[0, 1]`。名称最长 128 个字符。

### 预算（原型档位）

预算在创建任何 GPU 资源之前检查，且可调整，属于原型档位而非永久格式限制。默认值：载荷 8 MiB；顶点 50 000、索引 150 000；关节 128；层 64、参数 64；变形器 128；单个遮罩 64 点；网格单轴最长 64；单个 channel 4 096 key、单个动画 1 024 channel、模型 32 个动画、动画 key 总计 262 144。

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

`cargo test` 覆盖模型管线、模拟和坐标映射；`tests/gpu_render.rs` 通过真实 GPU 离屏渲染检查透明、遮罩、绘制顺序和蒙皮。

Windows 合成与输入验证（会启动自己的纯色背景和应用窗口，仅截图应用区域）：

```powershell
& "$HOME/.cargo/bin/cargo.exe" build
pwsh -NoProfile -File tests/windows_smoke.ps1
```

验证日志和应用区域截图保留在每次新建的 `.artifacts/` 子目录；测试背景与应用在结束时关闭。

## 当前状态与范围外内容

- 本地验收已通过：单元与集成测试、真实 GPU 离屏渲染测试（`tests/gpu_render.rs`）、以及 `tests/windows_smoke.ps1`（真实透明合成、点击穿透双向切换、重叠窗口恢复、冒烟交互序列）。本 README 不包含帧率或性能结论。
- 无 GLB 或其他外部格式导入。
- 无联网、商店、账号、支付、SQLite；联网需求属于后续工作。
- 无可视化编辑器。
- 无 IK、物理、morph，也不是最终 PBR 管线。
- 遮罩是静态本地多边形，当前不随变形器变形。
- 非 Windows 平台不在第一阶段承诺内（Windows 首发）。
- 示例模型是自制验证资产，不代表最终美术质量。
