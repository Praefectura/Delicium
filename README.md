# Delicium

[Latine](#latine) · [English](#english) · [中文](#中文) · [Documentation](#documentation)

*Delicium* — Latin: "darling, pet, beloved" (Lewis & Short; the singular of *deliciae*).
One `delicium`, many `deliciae`: the word already inflects for a single pet and for a
whole desktop of them.

A Windows desktop-character runtime. A transparent overlay sits on the desktop and runs
characters in Delicium's own 2D parametric and 3D skeletal formats, without a
general-purpose game engine — wgpu, winit, egui and bevy_ecs underneath. First platform
is Windows; performance and visual quality come before compatibility with existing model
formats.

---

## Latine

*Delicium* nomen Latinum est, quod "delicias" vel "rem caram" significat: singulariter
*delicium*, pluraliter *deliciae*. Itaque vocabulum ipsum et unum animalculum et totam
tabulam operis designat.

Delicium est programma Windows quod personas animatas in tabula operis ostendit. Fenestra
perlucida in tabula operis manet, et personae formis propriis Delicii utuntur: formae duae
dimensionis parametris et formae trium dimensionis ossium. Machina ludi universalis non
adhibetur; wgpu, winit, egui, bevy_ecs infra sunt. Primum solum Windows; celeritas et
species picturae ante omnia, non convenientia cum formis alienis.

**Quid facere potest:**

- Fenestra perlucida: sola persona murem accipit, campus vacuus murem transit. Fenestra
  autem imperii semper murem accipit.
- Formae 2D parametris (min / max / default), rotatio et distortio, pictura, masca
  polygonorum.
- Formae 3D cum ossibus, animatio, cutis per GPU, materia asperitatis et metalli, lux
  directa, profunditas.
- Simulatio fixa 60 Hz, gradus quattuor per imaginem, interpolatio in reddendo.
- Usque ad 64 personas; vicinae intra spatium 2.5 unitatum inter se respondent.
- Personae sponte ambulant in campo visibili, nisi `--no-wander`.
- Ictus muris: preme et solve in eodem loco (minus quam 4 px) — persona se vertit et
  saltat per circa unum secundum; motus est in pictura tantum, locus non mutatur.
- Icona in area nuntiorum: sinistra preme ut fenestram imperii ostendas aut celes,
  dextra preme ut menu aperias.
- Configuratio in `%APPDATA%\Delicium\config.json` servatur et in initio legitur: personae,
  loci, mores (ictus regionum, vagatio), lingua, persona prima, et finis colloquii.
- **Bibliotheca formarum**: machina nulli formae certae alligata est. Quinque formae
  inclusae per id vocantur (`bangboo-eous`, `puppet2d`, `robot3d`, et varietates pictae);
  quodlibet `*.dmodel.json` in `%APPDATA%\Delicium\models\` positum fit pars bibliothecae.
- **Lingua duplex**: Sinica simplificata et Anglica, in pagina praeferentiarum electa aut ex
  lingua Windows sumpta.
- **Fenestra colloquii**: unaquaeque persona memoriam colloquii in fenestra imperii habet.
  Sine fine constituto lineis localibus respondet; cum fine OpenAI-compatibili nuntium,
  contextum et responsum mittit et ostendit. Colloquia inter personas eodem in indice
  scribuntur.
- Unum animalculum Bangboo (Eous) in `--bangboo` modo (idem ac `--pet bangboo-eous`);
  alterum in fenestra imperii e bibliotheca additur; formae propriae exempli etiam adsunt.

**Aedificare et agere** (Rust MSVC in Windows):

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
& "$HOME/.cargo/bin/cargo.exe" run --release -- --bangboo
```

Programma stabile `delicium-launcher.exe` primum se ipsum et machinam in
`%LOCALAPPDATA%\Delicium\` ponit, deinde novam versionem ex GitHub quaerit, et animalculum
aperit. In initio nulla fenestra apparet.

Plena praecepta: [docs/USAGE.md](docs/USAGE.md) (Sinice). Formae descriptio:
[docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md). Examinationes:
[docs/VERIFICATION.md](docs/VERIFICATION.md).

---

## English

*Delicium* is Latin for "darling, pet, beloved" — the singular of *deliciae*. The word
already carries both a single pet and a whole desktop of them, which is the whole idea.

Delicium is a Windows desktop-character runtime. A transparent, always-on-top overlay
runs animated characters in Delicium's own formats — 2D parametric models and 3D skeletal
models — without a general-purpose game engine. Rendering is wgpu, the window layer is
winit, the control panel is egui, and the simulation runs on bevy_ecs.

Windows first. Performance and visual quality come before compatibility with existing
model formats: Delicium does not import GLB or other external formats.

### What it does today

- **Transparent overlay.** Only the character's outline accepts the mouse; empty scene
  areas pass clicks through to whatever is underneath. The control window always stays
  interactive.
- **Two model families.** 2D sprite models with parametric deformation (rotation and
  warp), textures and polygon masks. 3D skeletal models with animation, GPU skinning,
  base PBR material (roughness/metallic), directional light and depth.
- **Fixed-step simulation.** 60 Hz with up to four catch-up substeps per frame, and
  interpolation on the render side.
- **Up to 64 characters**, with nearest-neighbour social responses inside a 2.5-unit
  radius, including 2D/3D cross-type pairs.
- **Autonomous wandering.** Characters walk horizontally inside the visible area of the
  scene, on an idle/walk state machine; `--no-wander` turns it off.
- **Click reaction.** Press and release in place (under 4 px) makes the character turn
  toward the click and do a short startled hop. The hop is a presentation offset: the
  simulated position does not move.
- **Tray icon.** Left click shows or hides the control window; right click opens a menu.
  Closing the control window hides it to the tray.
- **Persistent configuration** in `%APPDATA%\Delicium\config.json`: instances, positions,
  the hit-region and wandering preferences, the interface language, the default character and
  the chat endpoint are restored on the next launch.
- **Model library.** No character is wired into the engine. Five bundled models are addressed
  by id (`bangboo-eous`, `puppet2d`, `robot3d`, and textured variants), and any
  `*.dmodel.json` dropped into `%APPDATA%\Delicium\models\` becomes a library entry. `--pet
  ID|PATH` starts one of them; the controls list the library and add instances from it.
- **Two interface languages.** Simplified Chinese and English, chosen in the settings page or
  following the Windows UI language.
- **Conversation panel.** Each character has an in-memory conversation in the control window.
  Without a configured endpoint the pet answers with local lines; with an OpenAI-compatible
  endpoint configured it sends the persona, the recent turns and the new message, and shows the
  reply. Character-to-character social lines land in the same log.
- **A bundled Bangboo pet** (Eous) with `--bangboo` (an alias for `--pet bangboo-eous`);
  a second one is added from the model library in the control window. Self-made example
  assets are included.

### Install

There is no published release yet, so there is nothing to download. Once one exists, the
intended flow is: download the launcher from the Releases page, put it anywhere, and run
it. On first start it copies itself into `%LOCALAPPDATA%\Delicium\`, so the download
location does not matter and the original file can be deleted afterwards.

### Build and run

Requires a Rust MSVC toolchain on Windows and a Visual Studio build environment.

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
& "$HOME/.cargo/bin/cargo.exe" run --release -- --bangboo
```

### The launcher

`delicium-launcher.exe` is the stable entry point. On first start it copies itself into
`%LOCALAPPDATA%\Delicium\`, then checks GitHub Releases for a newer build, installs it if
there is one, and starts the pet. A double click goes straight to the pet — no window, no
interruption. Settings open on demand from the tray menu (`--settings` opens them directly).

The settings page carries the hit-region and wandering toggles, the interface language, the
default character (a library id) and the conversation endpoint (base URL, model, API key).
It writes only the fields it owns, through a temporary file and an atomic rename, so unknown
fields and the engine's saved scene survive; the running pet polls the file and applies
preference changes live.

The layout is `%LOCALAPPDATA%\Delicium\` with `delicium-launcher.exe`, `state.json`,
`launcher.json`, `logs\`, and one `app\<version>\delicium.exe` per installed version.
Each version gets its own directory, so an update never overwrites the build that is
currently running. The launcher replaces its own image with `ReplaceFileW`, which Windows
allows while the file is executing.

Only one engine version is kept: a new version is recorded as pending, and once it has run
long enough to count as healthy the version it replaced is deleted along with its directory.
If the new version crashes early three times it is discarded and the version that was
already running stays. A directory without a valid `.install.json` marker is never deleted,
and neither are your own models in `%APPDATA%\Delicium\models\`.

Updates are read from a fixed address
(`.../releases/latest/download/latest.json`) and verified with SHA-256 over TLS. The
manifest never supplies a URL: asset addresses are derived from the repository and the
release tag, so a compromised manifest cannot redirect a download. A failed or interrupted
update installs nothing and the running version keeps starting.

### Command line

The engine (`delicium.exe`) takes `--model`, `--pet`, `--bangboo`, `--overlay-size`,
`--exit-after`, `--no-wander`, `--backend`, the `--bench` family, and `--smoke-test`.
The launcher takes `--settings`, `--no-update`, `--engine PATH`, and forwards everything
after `--` to the engine. Full tables: [docs/USAGE.md](docs/USAGE.md).

### Testing

```powershell
& "$HOME/.cargo/bin/cargo.exe" test --release --lib
& "$HOME/.cargo/bin/cargo.exe" test --test gpu_render
& "$HOME/.cargo/bin/cargo.exe" clippy --all-targets -- -D warnings
& "$HOME/.cargo/bin/cargo.exe" fmt --check
```

Two `GetGuiResources` leak tests read process-wide GDI counters, so they are `#[ignore]`d
and must run in a dedicated process invocation without other tests alongside:

```powershell
& "$HOME/.cargo/bin/cargo.exe" test --release --lib -- --ignored --test-threads=1 repeated_builds_do_not_grow_the_process_gdi_object_count repeated_chunked_builds_do_not_grow_the_process_gdi_object_count
```

Windows GUI acceptance scripts live in `tests/` and each starts its own scene, drives
real input, and writes logs and screenshots to a fresh `.artifacts/` directory.

### Model formats

`delicium.model` JSON, extension `.dmodel.json`, strictly self-contained — no file paths,
URLs, scripts or external resources. Version `0.1` is the original colour path; `0.2`
adds embedded PNGs, UVs, base materials and optional action bindings. Unknown versions
and unknown fields are rejected. The full specification is in
[docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md) (Chinese); the English README gives only the
summary above.

### Status

Pre-release, single platform, one tagged release (`v0.1.0`). What has actually been run:

- **Offline gate.** `fmt --check`, `clippy --all-targets -D warnings`, 274 library tests
  (19 ignored by design), seven non-GPU integration targets, eight hardware-GPU tests, and a
  release build: 390 passed, 0 failed. Both `GetGuiResources` leak tests pass in a dedicated
  process. Eight Windows GUI acceptance scripts pass. The launcher/settings handshake
  was proved with a cross-process probe, and version retention was exercised end to end on a
  synthetic installation root (16 assertions, including a second start).
- **Chat transport.** The conversation panel and its OpenAI-compatible client were exercised
  against a loopback HTTP fixture owned by the test, including refusal of redirects and
  error statuses. **No real third-party LLM service has been contacted**, and the optional
  API key is stored in plain text in `%APPDATA%\Delicium\config.json`.
- **Release pipeline.** Tag `v0.1.0` triggered `.github/workflows/release.yml` on
  `windows-latest`; the run finished green and published three assets:
  `delicium-launcher.exe`, `delicium.exe` and `latest.json`. An anonymous request to
  `releases/latest/download/latest.json` returns the manifest, which redirects to
  `objects.githubusercontent.com` and is followed by the client's WinHTTP redirect policy.
- **Install from the published Release.** The launcher downloaded from the Release, pointed
  at an empty installation root, fetched the manifest and the engine over the network,
  verified SHA-256, installed `app/0.1.0/delicium.exe`, and ran it to a clean exit. The
  on-disk engine hash matches the manifest. A second launch reported `Up to date (0.1.0)`
  and did not reinstall.
- **Still outstanding.** That end-to-end run happened on the development machine with an
  isolated `DELICIUM_INSTALL_ROOT`, not on a pristine machine. Self-replacement of the
  launcher image has not been exercised by a release that raises `min_launcher_version`,
  because no such release exists yet. Dual-monitor acceptance is untested.

Treat the updater as implemented and exercised end to end once, not as battle-tested.

### What is not here yet

No GLB or other external format import; no visual editor; no IK, physics or morph targets;
wandering is horizontal only, with no vertical movement or obstacle avoidance; the only
network feature is the optional, user-configured chat endpoint — there is no store, no
account system, no payment and no SQLite in the runtime. Materials are basic GGX/Schlick
without IBL, shadows, normal maps or mipmaps. Non-Windows platforms are out of scope for the
first stage. See [docs/ROADMAP.md](docs/ROADMAP.md) for the current status list.

### Licence

MIT, © 2026 Praefectura — see [LICENSE](LICENSE). The bundled `bangboo-eous.dmodel.json`
is adapted from OSad's *Bangboo 01 - Eous*, whose original page declares CC BY 4.0;
attribution and the list of modifications are in
[assets/examples/bangboo-eous.md](assets/examples/bangboo-eous.md). Character and
trademark rights do not transfer with that model licence. This project is not affiliated
with the rights holders.

---

## 中文

**Delicium** 是一个 Windows 桌面角色运行时。透明 Overlay 常驻桌面，运行 Delicium 原生格式的角色：2D 参数化模型与 3D 骨骼模型。不依赖现成游戏引擎——渲染是 wgpu，窗口是 winit，控制窗是 egui，模拟运行在 bevy_ecs 上。

第一阶段只面向 Windows，优先做好性能与表现质量，不承担既有外部模型格式的兼容（不导入 GLB 等格式）。

### 现在能做什么

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

### 安装

目前还没有可下载的 Release。发布之后的预期流程是：从 Releases 页面下载启动器，放在任意位置运行。首次启动会把它自己复制进 `%LOCALAPPDATA%\Delicium\`，因此下载位置无所谓，原文件之后可以删除。

### 构建与运行

需要 Windows 上的 Rust MSVC 工具链与 Visual Studio 构建环境。

```powershell
& "$HOME/.cargo/bin/cargo.exe" run --release
& "$HOME/.cargo/bin/cargo.exe" run --release -- --bangboo
```

### 启动器与自动更新

`delicium-launcher.exe` 是稳定入口。首次启动时把自己复制进 `%LOCALAPPDATA%\Delicium\`，随后向 GitHub Releases 查询新版本，有则安装，然后拉起桌宠。双击直接进桌宠，不弹窗、不打断；设置页从托盘菜单按需打开（`--settings` 直接打开）。

设置页包含命中区域与漫游开关、界面语言、默认角色（库 id）和对话端点（Base URL、模型名、API Key）。它只写自己拥有的字段，经临时文件原子替换落盘，因此未知字段与引擎保存的场景都会保留；运行中的桌宠会轮询该文件并实时应用偏好改动。

目录布局为 `%LOCALAPPDATA%\Delicium\`，其中有 `delicium-launcher.exe`、`state.json`、`launcher.json`、`logs\`，以及每个已安装版本各自的 `app\<version>\delicium.exe`。每个版本独占一个目录，更新不会覆盖正在运行的构建。启动器用 `ReplaceFileW` 替换自身镜像——Windows 允许替换正在执行的 exe。

只保留一个引擎版本：新版本先记为待确认，运行足够久确认为健康后，被替换的版本连同其目录一起删除；新版本若早期连续崩溃三次则被丢弃，原来能用的版本继续运行。没有有效 `.install.json` 标记的目录永不删除，`%APPDATA%\Delicium\models\` 里的自有模型也不参与清理。

更新清单从固定地址读取（`.../releases/latest/download/latest.json`），经 TLS 传输并用 SHA-256 校验。清单**不提供任何 URL**：资产地址由仓库与 release tag 推导，因此被篡改的清单无法把下载重定向到别处。更新失败或中断时不安装任何东西，正在运行的版本继续启动。

### 命令行

引擎（`delicium.exe`）接受 `--model`、`--pet`、`--bangboo`、`--overlay-size`、`--exit-after`、`--no-wander`、`--backend`、`--bench` 系列与 `--smoke-test`；启动器接受 `--settings`、`--no-update`、`--engine PATH`，`--` 之后的参数原样转交引擎。完整表格见 [docs/USAGE.md](docs/USAGE.md)。

### 测试

```powershell
& "$HOME/.cargo/bin/cargo.exe" test --release --lib
& "$HOME/.cargo/bin/cargo.exe" test --test gpu_render
& "$HOME/.cargo/bin/cargo.exe" clippy --all-targets -- -D warnings
& "$HOME/.cargo/bin/cargo.exe" fmt --check
```

两个 `GetGuiResources` 泄漏测试读取**进程级** GDI 计数，必须各自在独立进程中运行（命令见英文节）。Windows GUI 验收脚本位于 `tests/`，每次启动自己的场景、注入真实输入，并把日志与截图写到新建的 `.artifacts/` 目录。

### 模型格式

`delicium.model` JSON，扩展名 `.dmodel.json`，完全自包含——不含文件路径、URL、脚本或外部资源。`0.1` 是原颜色路径；`0.2` 增加内嵌 PNG、UV、基础材质与可选动作绑定。未知版本和未知字段一律拒绝。完整规范在 [docs/MODEL-FORMAT.md](docs/MODEL-FORMAT.md)，英文 README 只给摘要。

### 验证状态

预发布阶段，仅 Windows，已有一个带 tag 的发布（`v0.1.0`）。以下都是实际跑过的：

- **离线门**：`fmt --check`、`clippy --all-targets -D warnings`、274 项 lib 测试（19 项按设计忽略）、七个非 GPU 集成目标、八项硬件 GPU 测试与 release 构建，共 390 通过、0 失败；两个 `GetGuiResources` 泄漏测试在独立进程中通过；八套 Windows GUI 验收脚本通过；启动器与设置页的握手用跨进程探针实测通过；版本保留在合成安装根上完成 16 项端到端断言（含二次启动幂等）。
- **对话传输**：会话面板与 OpenAI 兼容客户端只在本机回环夹具上验证过，含拒绝重定向与错误状态；**未联系任何真实第三方 LLM 服务**。可选的 API key 以明文存放在 `%APPDATA%\Delicium\config.json`。
- **发布流水线**：推 tag `v0.1.0` 触发 `.github/workflows/release.yml` 在 `windows-latest` 上运行，结果全绿，发布三个资产：`delicium-launcher.exe`、`delicium.exe`、`latest.json`。未认证请求 `releases/latest/download/latest.json` 能拿到清单；该请求重定向到 `objects.githubusercontent.com`，由客户端的 WinHTTP 重定向策略跟随。
- **从已发布 Release 安装**：把 Release 里的启动器下载到本机，指向一个空安装根，经网络取清单与引擎、校验 SHA-256、装入 `app/0.1.0/delicium.exe` 并运行至干净退出；落盘引擎哈希与清单一致。第二次启动报告 `Up to date (0.1.0)`，未重复安装。
- **仍未完成**：上述端到端是在开发机上用隔离的 `DELICIUM_INSTALL_ROOT` 跑的，不是在全新机器上。启动器镜像自替换尚未被"提高 `min_launcher_version` 的发布"实际触发过，因为还不存在这样的发布。双屏实机验收未做。

请把更新器理解为"已实现并端到端跑通过一次"，而不是"久经考验"。

### 尚未包含

无 GLB 或其他外部格式导入；无可视化编辑器；无 IK、物理与 morph；漫游只做水平方向，无竖直移动与避障；运行时唯一的联网能力是用户自行配置的对话端点，无商店、账号、支付或 SQLite。材质是基础 GGX/Schlick，尚无 IBL、阴影、法线贴图与 mipmap。第一阶段不承诺非 Windows 平台。当前状态与范围外清单见 [docs/ROADMAP.md](docs/ROADMAP.md)。

### 许可

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
