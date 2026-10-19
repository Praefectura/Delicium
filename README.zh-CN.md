# Delicium

[English](README.md) · [简体中文](README.zh-CN.md) · [Latine](README.la.md)

**Delicium** 是一个 Windows 桌面角色运行时。透明 Overlay 常驻桌面，运行 Delicium 原生格式的角色：2D 参数化模型与 3D 骨骼模型。不依赖现成游戏引擎——渲染是 wgpu，窗口是 winit，控制窗是 egui，模拟运行在 bevy_ecs 上。

第一阶段只面向 Windows，优先做好性能与表现质量，不承担既有外部模型格式的兼容（不导入 GLB 等格式）。

## 现在能做什么

- **透明 Overlay**：只有角色轮廓接收鼠标，空白区域穿透到下面的窗口；控制窗始终保持可交互。
- **两类模型**：2D 精灵支持参数变形（rotation / warp）、贴图与 polygon 遮罩；3D 骨骼支持动画、GPU 蒙皮、粗糙度/金属度基础材质、方向光与深度。
- **固定步长模拟**：60 Hz，每帧最多 4 个子步，渲染侧插值。
- **最多 64 个角色**，半径 2.5 内最近邻触发社交响应，支持 2D/3D 跨类型。
- **自主漫游**：角色在场景可见范围内按待机/行走状态机水平走动，`--no-wander` 关闭。
- **点击反应**：原地按下并释放（位移 < 4px）→ 角色转向点击点并做一次约 1 秒的惊吓小跳。小跳是呈现偏移，模拟位置不变。
- **系统托盘**：左键切换控制窗显隐，右键菜单；关闭控制窗 = 隐藏到托盘。
- **配置持久化**：`%APPDATA%\Delicium\config.json` 保存实例、位置、命中区域与漫游偏好、界面语言、默认角色与对话端点，下次启动恢复。
- **模型库**：引擎不再绑定任何具体角色。五个内建模型按 id 寻址（`bangboo-eous`、`puppet2d`、`robot3d` 及贴图版），把 `*.dmodel.json` 放进 `%APPDATA%\Delicium\models\` 就成为库条目；`--pet ID|PATH` 启动其中一个，控制窗列出模型库并可从库中新增实例。
- **两种界面语言**：简体中文与 English，在设置页选择，或跟随 Windows 界面语言。
- **会话框**：每个角色在控制窗里有自己的内存会话。未配置端点时用内置本地台词回答；配置了 OpenAI 兼容端点后，请求携带角色提示词、最近若干轮与本次消息，并回显回复。角色之间的社交台词也进入同一份记录。
- **一只邦布桌宠**（伊埃斯/Eous，`--bangboo`，等价 `--pet bangboo-eous`），第二只从控制窗的模型库 `Add` 加入，另有自制示例资产。

## 安装

从 [最新 Release](https://github.com/Praefectura/Delicium/releases/latest) 下载 `delicium-launcher.exe`，放在任意位置运行。首次启动会把它自己复制进 `%LOCALAPPDATA%\Delicium\`，因此下载位置无所谓，原文件之后可以删除。随后启动器自行取回引擎、校验 SHA-256 并拉起桌宠，没有其它安装步骤。

## 构建与运行

需要 Windows 上的 Rust MSVC 工具链与 Visual Studio 构建环境。

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
& "$HOME/.cargo/bin/cargo.exe" run --release -- --bangboo
```

## 启动器与自动更新

`delicium-launcher.exe` 是稳定入口。首次启动时把自己复制进 `%LOCALAPPDATA%\Delicium\`，随后向 GitHub Releases 查询新版本，有则安装，然后拉起桌宠。双击直接进桌宠，不弹窗、不打断；设置页从托盘菜单按需打开（`--settings` 直接打开）。

设置页包含命中区域与漫游开关、界面语言、默认角色（库 id）和对话端点（Base URL、模型名、API Key）。它只写自己拥有的字段，经临时文件原子替换落盘，因此未知字段与引擎保存的场景都会保留；运行中的桌宠会轮询该文件并实时应用偏好改动。

目录布局为 `%LOCALAPPDATA%\Delicium\`，其中有 `delicium-launcher.exe`、`state.json`、`launcher.json`、`logs\`，以及每个已安装版本各自的 `app\<version>\delicium.exe`。每个版本独占一个目录，更新不会覆盖正在运行的构建。启动器用 `ReplaceFileW` 替换自身镜像——Windows 允许替换正在执行的 exe。

只保留一个引擎版本：新版本先记为待确认，运行足够久确认为健康后，被替换的版本连同其目录一起删除；新版本若早期连续崩溃三次则被丢弃，原来能用的版本继续运行。没有有效 `.install.json` 标记的目录永不删除，`%APPDATA%\Delicium\models\` 里的自有模型也不参与清理。

更新清单从固定地址读取（`.../releases/latest/download/latest.json`），经 TLS 传输并用 SHA-256 校验。清单**不提供任何 URL**：资产地址由仓库与 release tag 推导，因此被篡改的清单无法把下载重定向到别处。更新失败或中断时不安装任何东西，正在运行的版本继续启动。

## 命令行

引擎（`delicium.exe`）接受 `--model`、`--pet`、`--bangboo`、`--overlay-size`、`--exit-after`、`--no-wander`、`--backend`、`--bench` 系列与 `--smoke-test`；启动器接受 `--settings`、`--no-update`、`--engine PATH`，`--` 之后的参数原样转交引擎。完整表格见 [docs/USAGE.md](docs/USAGE.md)。

## 测试

```powershell
& "$HOME/.cargo/bin/cargo.exe" test --release --lib
& "$HOME/.cargo/bin/cargo.exe" test --test gpu_render
& "$HOME/.cargo/bin/cargo.exe" clippy --all-targets -- -D warnings
& "$HOME/.cargo/bin/cargo.exe" fmt --check
```

两个 `GetGuiResources` 泄漏测试读取**进程级** GDI 计数，必须各自在独立进程中运行（命令见英文版 README）。Windows GUI 验收脚本位于 `tests/`，每次启动自己的场景、注入真实输入，并把日志与截图写到新建的 `.artifacts/` 目录。

## 模型格式

`delicium.model` JSON，扩展名 `.dmodel.json`，完全自包含——不含文件路径、URL、脚本或外部资源。`0.1` 是原颜色路径；`0.2` 增加内嵌 PNG、UV、基础材质与可选动作绑定。未知版本和未知字段一律拒绝。完整规范在 [docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md)，英文 README 只给摘要。

## 验证状态

预发布阶段，仅 Windows，已有三个带 tag 的发布（`v0.1.0`、`v0.1.1`、`v0.1.2`）。以下都是实际跑过的：

- **离线门**：`fmt --check`、`clippy --all-targets -D warnings`、274 项 lib 测试（19 项按设计忽略）、七个非 GPU 集成目标、八项硬件 GPU 测试与 release 构建，共 390 通过、0 失败；两个 `GetGuiResources` 泄漏测试在独立进程中通过；八套 Windows GUI 验收脚本通过；启动器与设置页的握手用跨进程探针实测通过；版本保留在合成安装根上完成 16 项端到端断言（含二次启动幂等）。
- **对话传输**：会话面板与 OpenAI 兼容客户端只在本机回环夹具上验证过，含拒绝重定向与错误状态；**未联系任何真实第三方 LLM 服务**。可选的 API key 以明文存放在 `%APPDATA%\Delicium\config.json`。
- **发布流水线**：每个 tag 触发 `.github/workflows/release.yml` 在 `windows-latest` 上运行，均全绿并发布三个资产：`delicium-launcher.exe`、`delicium.exe`、`latest.json`。`v0.1.2` 的 tag run（`39221001049`）与 main push run（`39220976113`）都成功；后者上传 11.2 MB 工作流产物并跳过发布步骤，因此合并到 `main` 不会创建 Release。三个 `v0.1.2` 资产均为 `state=uploaded`，其 digest 与匿名清单逐条一致（清单里 `min_launcher_version "0.1.2"`）；该请求重定向到 `objects.githubusercontent.com`，由客户端的 WinHTTP 重定向策略跟随。
- **从已发布 Release 安装**：把 `v0.1.0` 的启动器下载到本机，指向一个空安装根，经网络取清单与引擎、校验 SHA-256、装入 `app/0.1.0/delicium.exe` 并运行至干净退出；落盘引擎哈希与清单一致。第二次启动报告 `Up to date (0.1.0)`，未重复安装。同样的两阶段在 `v0.1.1` 上用全新空根跑通：清单、24.7 MB 下载、SHA-256 与 size 匹配、`app/0.1.1`、引擎启动确认、干净退出，再到 `Up to date (0.1.1)` 且只保留一个版本目录，24 项断言全过，耗时 36 秒。
- **仍未完成**：上述端到端都是在开发机上用隔离的 `DELICIUM_INSTALL_ROOT` 跑的，不是在全新机器上。`v0.1.2` 把 `min_launcher_version` 提到 `0.1.2`，已安装的 `0.1.1` 启动器会先替换自身镜像再装新引擎；这条分支已随版本发布，但还没有在预置 0.1.1 的安装根上实际执行过。双屏实机验收未做。

请把更新器理解为"已实现并端到端跑通过一次"，而不是"久经考验"。

## 尚未包含

无 GLB 或其他外部格式导入；无可视化编辑器；无 IK、物理与 morph；漫游只做水平方向，无竖直移动与避障；运行时唯一的联网能力是用户自行配置的对话端点，无商店、账号、支付或 SQLite。材质是基础 GGX/Schlick，尚无 IBL、阴影、法线贴图与 mipmap。第一阶段不承诺非 Windows 平台。当前状态与范围外清单见 [docs/ROADMAP.md](docs/ROADMAP.md)。

## 许可

MIT，© 2026 Praefectura，见 [LICENSE](LICENSE)。`bangboo-eous.dmodel.json` 改编自 OSad 的 *Bangboo 01 - Eous*（原作页面声明 CC BY 4.0），署名与改动清单见 [assets/examples/bangboo-eous.md](assets/examples/bangboo-eous.md)。角色与商标权利不随模型许可转移，本项目与权利方无关联。

---

## Documentation

| File | Contents | Language |
|---|---|---|
| [docs/USAGE.md](docs/USAGE.md) | Build, run, every command-line flag, control window, overlay and multi-monitor behaviour | 中文 |
| [docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md) | Complete `delicium.model` specification, budgets, texture rules | 中文 |
| [docs/VERIFICATION.md](docs/VERIFICATION.md) | Test procedures and every dated acceptance record, including failed gates | 中文 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Current status, what is out of scope, licence table | 中文 |
| [assets/examples/bangboo-eous.md](assets/examples/bangboo-eous.md) | Eous asset provenance, CC BY 4.0 attribution, modifications | 中文 |

The documentation files are Chinese because they were written during development and are
moved verbatim so that no measured number is retyped. Contributions that add English or
Latin versions of them are welcome.

Delicium's measured numbers in `docs/VERIFICATION.md` are per-run results from the day
they were recorded. A later improvement does not turn a failed gate into a pass, and the
file keeps both.
