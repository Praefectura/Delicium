# Delicium 使用说明

> 本文件保存构建、运行、命令行、控制窗与授权说明，逐字移自 README（2026-10-18）。
> 英文 README 只给摘要，此处为权威文本。

## 构建与运行

前置条件：Windows 上的 Rust MSVC 工具链，构建依赖机器上已有的 Visual Studio 构建环境。

PowerShell（不要求修改全局 PATH，按绝对路径调用 cargo；若 cargo 已在 PATH 中可直接写 `cargo`）：

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
```

### 邦布桌宠

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release --locked --offline -- --bangboo
# 已构建时：
.\target\release\delicium.exe --bangboo
```

此模式加载两只伊埃斯（Eous）3D桌宠：白色头罩、黑色面板、黄绿环眼及橙色“01”围巾，可点击反应、重复点击重启、拖拽移动。靠近时双方挥手问候，再一起弹跳玩耍；点击或拖走一只会结束双方互动。控制窗的添加3D角色在此模式下添加伊埃斯。此入口不读写已有用户配置，退出后不会覆盖原场景。

模型改编自 [OSad 的 Bangboo 01 - Eous](https://sketchfab.com/3d-models/bangboo-01-eous-a3dbb1c10b4c4f1b97a47fa798239af9)（原作页面声明 CC BY 4.0），保留22骨、内嵌2048像素角色贴图，运行时不加载外部图片或游戏客户端文件。成品为 `assets/examples/bangboo-eous.dmodel.json`，离线适配工具为 `tools/convert_eous.py`；来源、许可与改动见 [资产说明](../assets/examples/bangboo-eous.md)。旧自制九骨资产及 `tools/generate_bangboo.py` 保留。

## 命令行

与 `src/app/cli.rs` 一致：

| 参数 | 说明 |
| --- | --- |
| `--model PATH` | 加载自包含的 `.dmodel.json` 模型，可重复，最多 64 个；不提供时加载两个内置示例 |
| `--bangboo` | 加载两只可互动邦布，不读写用户配置；与 `--model`、`--smoke-test`、`--bench` 互斥 |
| `--backend auto\|dx12\|vulkan` | 选择 GPU 后端，默认 auto |
| `--overlay-size WIDTHxHEIGHT` | 显式有界场景尺寸（逻辑像素），允许 320x240 到 3840x2160；不提供时场景覆盖所有显示器的物理包围盒（`--smoke-test` 与 `--bench` 归一化为有界 960x640） |
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

配置持久化：正常退出把实例来源与位置、命中区域偏好写入 `%APPDATA%\Delicium\config.json`，下次启动恢复；显式 `--model` 完全压过配置中的实例，`--smoke-test`/`--bench`/`--bangboo` 不读不写。`DELICIUM_NO_CONFIG=1` 环境变量在任意启动下禁用配置读写（GUI 回归脚本用它隔离用户配置）。

普通启动时角色在场景可见水平范围内自主游走，`--no-wander` 关闭；`--smoke-test` 与 `--bench` 默认归一化为静止，使其固定负载保持确定；`--bench-wander` 显式让基准在漫游开启下运行。

普通启动（未给 `--overlay-size`）时，单个场景客户区覆盖启动时所有显示器物理矩形的包围盒（含负坐标与错位布局）；首次显示前设置 Windows 文档化的 `NonRudeHWND` 属性，避免覆盖整屏被 shell 误判为全屏并抑制自动隐藏任务栏，不再留 2px 缺口。属性设置失败会报告错误而不显示场景，不去修改系统任务栏层级。

场景固定使用主显示器缩放：每世界单位 `q = 640 × primary_scale / 4.5` 物理像素，竖直跨度为包围盒物理高除以 q；水平中心和底边按主屏偏移修正，使已有世界坐标在主屏上的位置不随扩窗整体漂移。扩大的是活动范围而非角色尺寸；两屏物理像素密度不同不保证角色厘米大小相同。控制窗仍锚定主屏右上（右边距和顶边距均 50 逻辑像素）。显式 `--overlay-size` 保持原有界窗口、视图与偏移布局。

角色可在相邻屏接缝两侧连续渲染、拾取与拖拽。自主漫游只沿角色当前高度的实际显示区域、在包含角色当前位置的连通横段内走动；不穿越显示器间的空隙，无可用横段时暂停而不瞬移。用户仍可主动拖动到其他高度。拖拽使用 winit 原生鼠标捕获，离开场景客户区会取消；拖过控制窗的行为不作承诺。显示布局是启动快照，热插拔或布局重排后需重启。

## 控制窗

- `Add 2D` / `Add 3D`：新增一个示例角色。
- 角色列表：显示 `id: 名称 (类型)`，选中后显示位置并可用 x/y 拖动框调整。
- 邻接状态：显示是否正在响应附近角色。
- `Reload selected` / `Remove selected`：重载或移除选中角色。
- 本地 `.dmodel.json` 路径输入框 + `Load as new` / `Replace selected`：以新实例加载，或替换当前选中实例。
- 把一个模型文件拖放到任意一个窗口，都会新增一个角色。
- 角色命中区域复选框：勾选时，仅角色网格投影轮廓及边缘余量接收点击；取消勾选时，整个场景窗口接收点击。拖拽期间临时使用整窗区域，释放后恢复轮廓区域。
- 拖动会暂停被拖角色的游走，释放后恢复；释放时重新抽取一段静止时长，角色不会立刻走开。
- 在角色上按下并原地释放（释放点与按下点距离 < 4px）= 点击：角色面向点击点并做一次约 1 秒的惊吓小跳（2D 通过翻转朝向、3D 通过头部朝向表达，跳跃是渲染侧偏移、不改变模拟位置），期间游走暂停、结束沿原目标继续；点击不移动角色。
- 关闭控制窗 = 隐藏到系统托盘（左键托盘图标或菜单 Show/Hide controls 恢复，恢复时置顶）；托盘不可用时关闭回退为退出。场景窗关闭始终退出程序。
- 命中区域使用 2D 变形网格与 3D 蒙皮网格的投影三角形并集，保留耳朵、四肢间隙；带 8px 轴向边缘余量，不是严格按 alpha、遮罩或抗锯齿逐像素命中。始终相对上次成功安装的几何比较：位移不超过 4px 时复用，超过 4px 但小于 7px 时才允许 K 帧节流；达到 7px，或实例身份、拓扑、窗口尺寸、视图变化时当帧更新，避免旧窗口区域裁掉当前动画。8px 余量不增大。
- 区域构建失败时报告错误并退回整窗穿透，不逐帧重试；切换复选框可重新尝试。
- 状态行（Frame ms | tick | interactions | pending）、GPU 适配器信息。
- 加载错误显示在控制窗内，可用 `Dismiss error` 关闭；`Quit` 退出程序。

