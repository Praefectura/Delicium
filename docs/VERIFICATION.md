# Delicium 验证与验收记录

> 本文件保存全部测试说明与按日期的验收记录，逐字移自 README（2026-10-18）。
> 其中的数字是当轮实测结果，不随之后版本更新；未通过的项按原样保留，
> 不因后续改进而追认为通过。

## 测试与检查

```powershell
& "$HOME/.cargo/bin/cargo.exe" test
& "$HOME/.cargo/bin/cargo.exe" test --test gpu_render
& "$HOME/.cargo/bin/cargo.exe" test --release --lib -- --ignored --test-threads=1 repeated_builds_do_not_grow_the_process_gdi_object_count repeated_chunked_builds_do_not_grow_the_process_gdi_object_count
& "$HOME/.cargo/bin/cargo.exe" clippy --all-targets -- -D warnings
& "$HOME/.cargo/bin/cargo.exe" fmt --check
& "$HOME/.cargo/bin/cargo.exe" run --release -- --smoke-test
```

第三个命令不是可选的：两个 `GetGuiResources` 泄漏测试读的是**进程级** GDI 句柄计数，会与其它测试互相干扰，所以标记为 `#[ignore]` 并在独立进程中运行。只跑常规 `cargo test` 时它们不会执行，删除 `Drop` 里的 `DeleteObject` 而保留自维护计数器仍会让全套测试为绿，因此需要显式运行这一条。

`cargo test` 覆盖模型管线、模拟和坐标映射；`tests/gpu_render.rs` 通过真实 GPU 离屏渲染检查透明、遮罩、绘制顺序和蒙皮。Windows 下的 lib 测试还将同一动画帧中所有 alpha 非零的 GPU 像素与真实 GDI 区域逐点比对，覆盖区域重建、缓存复用和未超过 4px 的累计漂移。

原生区域采用流式分块批量安装：每 41 个多边形一批，单次 `CreatePolyPolygonRgn(points, counts, …, WINDING)` 建批（批次内绕向先归一化，批次 = 精确并集），批结果立即折入位宽归并槽（与原平衡 `RGN_OR` 同骨架，32 位宽的层级匹配原生坐标范围），只改变合并次序与批内建区方式，不改变几何、填充规则或缓存阈值；不会一次创建全部多边形句柄（单次批次 y 跨度 ≥ 约 2^24 会被 GDI 拒收，生产坐标距边界 3–4 个数量级，超界 fail-closed 返回错误）。Windows 测试以原线性算法为参考，检查新旧区域的 `RGN_XOR` 是否为空，并包含能识别区域差异的反例、10,512 多边形句柄峰值与错误释放检查；冻结 GPU 几何也执行精确区域等价断言。2026-10-15 曾评估以单次 `CreatePolyPolygonRgn` 整帧替换（Phase 0 决策门否决，整帧单调用慢 11.5x 且超线性）；2026-10-16 分块形态经重新预注册决策门采纳并实施（配对中位数 0.332），见下文两节。2026-10-18 起高密度且适用的输入改由整数扫描线快路径一次 `ExtCreateRegion` 建区，其余输入仍走上述 chunk-41，两条路径严格等价；该轮端到端性能门未通过但经确认保留接线，见下文"伊埃斯精确原生建区"。

Windows 合成与输入验证（需要窗口的脚本会启动自己的纯色背景和应用窗口；带截图的脚本仅截图应用区域，`windows_wander.ps1` 设计上不截图）：

```powershell
& "$HOME/.cargo/bin/cargo.exe" build
pwsh -NoProfile -File tests/windows_desktop.ps1
pwsh -NoProfile -File tests/windows_smoke.ps1
pwsh -NoProfile -File tests/windows_pixel_hit.ps1
pwsh -NoProfile -File tests/windows_wander.ps1
pwsh -NoProfile -File tests/windows_actions.ps1
pwsh -NoProfile -File tests/windows_bangboo.ps1
pwsh -NoProfile -File tests/windows_settings.ps1
pwsh -NoProfile -File tests/windows_chat.ps1
```

`windows_desktop.ps1` 验证普通启动的桌面范围：场景客户区等于所有显示器物理矩形的包围盒、`NonRudeHWND` 属性已设置、控制窗在屏内并按右上锚点定位、桌面模式拖拽越过旧有界窗口右边缘（12 步物理拖拽，世界位移与像素比一致）、释放后区域与空白穿透恢复、无 fallback；该脚本设计上不截图。`windows_smoke.ps1` 覆盖控制窗操作与完整 GUI 生命周期（显式 `--overlay-size 960x640` 保持有界布局），并覆盖一次原地点击：断言 `poke instance=` 日志 +1、press/release 各 +1、点击前后角色位置不变、点击与拖拽命中同一实例；该段拖拽向左（避免落入右侧同纹理傀儡的拾取重叠区），poke 锚点由拖拽锚点减去光标行程推导。`windows_pixel_hit.ps1` 专门检查默认轮廓模式下的空白/耳间/腿间穿透、角色拖拽、释放恢复、窗口层级与模式切换（同样显式有界）。这三个既有回归脚本显式传入 `--no-wander` 保持固定锚点；`windows_wander.ps1` 单独覆盖普通启动的自主水平游走、拖动期间暂停、释放后恢复，以及游走下的命中区域与穿透行为。四套脚本均设置 `DELICIUM_NO_CONFIG=1`，用户保存的配置不会泄漏进测试场景。两套截图脚本在客户区覆盖主显示器（含跨屏包围盒，每轴保留 8 物理像素守卫容差）时拒绝截图，防止把桌面范围误当成有界场景捕获；`windows_wander.ps1` 不截图。几何单测负责冻结帧的区域覆盖，动画截图仅验证角色存在和透明合成，不能替代同姿态无裁剪证明。

`windows_actions.ps1` 是单屏动作专项：在启用漫游的有界普通窗口中，用真实输入覆盖两类角色的待机/行走/返回待机、点击优先与重复点击重启、拖拽挂起/释放恢复，并保存不同动作及步态相位的应用区域截图供目检。动作日志只是状态证据，不能代替画面检查；lib中的动作冻结帧测试还把实际GPU可见像素与同姿态GDI命中区域逐点对照。

验证日志和应用区域截图保留在每次新建的 `.artifacts/` 子目录；测试背景与应用在结束时关闭，鼠标释放并恢复光标。上述八套脚本均支持 `-ValidateOnly`，只编译测试框架而不启动窗口。`windows_bangboo.ps1` 在有界场景固定世界位置，覆盖双方问候/玩耍的真实姿态、3D点击与重复点击、拖拽成对取消和冷却后重新配对，并保存各阶段截图；固定位置是专项测试参数，普通 `--bangboo` 启动仍默认漫游。

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

### 点击反应、托盘与配置持久化验收（2026-10-16）

- 最终离线全门通过（`EXIT_CODE=0`）：fmt、locked/offline clippy（全 target，warnings 为错误）、139 项非 GPU 测试（lib 81 通过 + 2 个 `#[ignore]` 探针 + model_pipeline 15 + simulation 24 + render_coordinates 6 + textured_model 13，`--test-threads=1`）、8 项 GPU 测试（NVIDIA RTX 5070 Laptop / Vulkan）、debug 与 release 构建。
- 同构建 GUI 脚本：`windows_smoke.ps1` 60 项（新增 8 条点击断言：`poke instance=` +1、press/release 各 +1、点击不移动角色、点击与拖拽同一实例）、`windows_pixel_hit.ps1` 44 项、`windows_wander.ps1` 62 项、`windows_desktop.ps1` 51 项、`windows_bench.ps1 -ValidateOnly` 59 项检查，全部通过、无命中区域 fallback。
- 托盘与配置端到端验收 67 项（隔离 APPDATA，不触碰用户真实配置）：托盘安装、隐藏窗口从不显示、TaskbarCreated 广播触发图标重装（先删除注册模拟 shell 重启）、左键消息切换控制窗显隐、关闭控制窗隐藏到托盘且不退出、场景窗关闭退出且正常写配置、右键菜单真实点击 Quit 退出、配置加载（含版本校验与坐标）、退出保存坐标与 pixel_hittest、重启恢复、`DELICIUM_NO_CONFIG=1` 读写双禁、`--smoke-test` 不写文件。
- 测试环境两处发现（仅影响测试脚本）：向左拖拽是点击断言的刻意设计（向右会把角色拖进同纹理傀儡的拾取重叠区，同 id 降序解析到另一实例）；`exit frames=` 日志只由 `--exit-after` 超时路径打印，场景窗关闭走另一条退出路径。

验证工件：`.artifacts/round-click-tray-config-20261016-135433/`（`03-full-gate.log`、`06-tray-config-verify.ps1` 及运行日志 `stderr-a/b/c/d/e.log`、隔离配置 `verify-appdata/`）；GUI 运行目录（`.artifacts/windows-20261016-135235-2202/` 等 `windows-*` 与 `windows-pixel-*`、`windows-wander-*`、`windows-desktop-*` 各一）。

### 任务栏唤出缺口验收（2026-10-16）

现场发现修复：场景窗口与主显示器矩形精确相等时被 shell 判为全屏窗口、抑制自动隐藏任务栏的唤出（对照实验：收缩 1 物理像素即恢复）。修复按 `SHAppBarMessage(ABM_GETTASKBARPOS)` 的任务栏停靠边留 2 物理像素缺口（新平台模块 `src/platform/desktop.rs`，含 x64 `APPBARDATA` 布局与边映射单测；查询失败回退底边）。

- 离线全门通过（`EXIT_CODE=0`）：lib 85 通过（含 4 项缺口几何/布局单测）+ 2 个 `#[ignore]` 探针、model_pipeline 15、simulation 24、render_coordinates 6、textured_model 13、GPU 8、fmt/clippy/debug/release。
- 同构建 GUI：`windows_desktop.ps1` 53 项（脚本自行查询任务栏边推导期望矩形，新增"客户区绝不精确覆盖显示器"断言）、`windows_smoke.ps1` 60 项、`windows_pixel_hit.ps1` 44 项、`windows_wander.ps1` 62 项、bench `-ValidateOnly` 59 项，全部通过。两套截图脚本的"桌面范围"守卫同步放宽为近似覆盖（每轴 8 物理像素容差），保持 fail-closed。
- 实机驻留复验：修复构建运行中，光标驻留屏幕底边 5 秒任务栏正常滑出（`(0,1528)-(2560,1600)`，与无应用基线一致），移开后缩回；场景客户区实测 `(0,0)-(2560,1598)`，缺口精确 2 像素。截图 `07-revealed-band.png` 可见任务栏完整滑出。

验证工件：`.artifacts/round-taskbar-gap-20261016-151634/`（`03-full-gate.log`、`gui-*.log`、`07-taskbar-dwell.ps1` 及日志、滑出截图）；根因实验归档 `.artifacts/run-logs/`。

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

流式平衡合并已完成，同一配置的单次完整 10 组对照通过。轮廓集合、8px 余量、4px 成功基线缓存和模拟追帧上限保持不变；没有通过降低更新频率或粗化几何换取收益（本句为当时口径；2026-10-15 的区域安装节流是其后首个受控安装节奏变更，几何与阈值口径仍不变，见下文；2026-10-15/16 评估的安装路径批量化未采纳，合并算法与几何口径至今未再变更）。

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

### 2026-10-15 区域安装节流（K 帧节奏）

漫游对照定位的瓶颈（64 实例逐帧动画变形击穿 4px 基线缓存、每帧全量重建原生区域约 50-60 ms、超出 66.67 ms 追帧上限导致丢步）实施受控节流：每帧提取与对比语义不变，仅把「已变几何 → `SetWindowRgn` 交付」按 K 帧节奏摊销。K 按新鲜多边形数分档（`region_cadence`）：< 2048 → 1，< 8192 → 2，否则 4；实测 4/16/32/64 实例 polygon_max 为 648/2616/5232/10512，即 K=1/2/2/4。K=1 保持历史行为（普通桌面 4 实例与全部既有 GUI 脚本不受影响）。未到节奏的已变帧记为 `deferred`（不安装、不动基线、不报错）：几何集合、4px/8px 口径、`FIXED_DT`/`MAX_SUBSTEPS` 与平台层合并算法均未改（合并算法的批量化替代其后经决策门评估并否决，见下文，生产未变）；滞后上界 K−1 个脏帧（c64 约 11.7px）与既有 4px+8px=12px 容差同量级。拖拽恢复强制下帧立即安装（`force_install`），模式切换/进入 fallback/像素测试命令均重置计数。

同一机器会话、同一新 release 二进制，先跑关闭态完整 10 组再跑开启态（`-Wander`）完整 10 组，两轮均 `pass=true cases=10`、script_version=4；与上一轮 v3 归档（旧二进制）并列对照（对照脚本 `MAIN_CADENCE_PROTOCOL_PHASE_CONFIG_RESOURCE_AND_COMPARISON_CHECKS=PASS`，含全套协议/阶段算术/资源 CSV 交叉/划分等式断言）：

| 用例 | pixel 提交/s 旧开→新开 | native 建区 p50 ms 旧开关→新开关 | native p95 ms 旧开→新开 | 场景回调 p50 ms 旧开→新开 | 新开 重建/推迟/复用 | 丢弃模拟步 旧开→新开 |
| --- | --- | --- | --- | --- | --- | --- |
| 4 | 54.16 → 53.13 | 3.737/3.866 → 4.147/4.224 | 4.327 → 5.686 | 5.07 → 5.50 | 1416/0/178 | 0 → 0 |
| 16 | 50.21 → 51.85 | 13.420/14.788 → 14.426/14.942 | 18.564 → 18.791 | 18.46 → 5.64 | 778/778/0 | 0 → 0 |
| 32 | 25.86 → 35.80 | 25.724/29.583 → 27.628/28.800 | 35.273 → 34.951 | 36.75 → 11.83 | 537/537/0 | 0 → 0 |
| 64 | 13.13 → 31.86 | 51.262/58.559 → 54.702/55.619 | 72.324 → 61.640 | 73.25 → 11.38 | 239/717/0 | 225 → 42 |
| 64 持续 300s | 12.61 → 31.89 | 51.739/59.858 → 54.950/55.151 | 78.414 → 61.157 | 74.83 → 11.56 | 2392/7175/0 | 2868 → 370 |
| 4 替换 100 次 | 53.52 → 53.64 | 3.580/4.516 → 3.819/3.764 | 6.899 → 4.997 | 5.86 → 4.98 | 1966/0/175 | 0 → 0 |

要点：

- 64 实例开启态提交率 13.13→31.86/s（+143%），持续 300s 12.61→31.89/s（+153%）；丢弃模拟步 225→42（占应执行步 12.49%→2.33%）与 2868→370（15.93%→2.06%）。重建份额恰为 1/K（c64 239/956≈0.25）：节流按设计把安装摊到 4 帧中的 1 帧，`scene_callback` p50 由 73.3→11.4 ms；残余丢步来自安装帧：安装 p95/p99 为 61.6/64.5 ms（p50 55.6 ms）再加其余帧内工作使帧间隔 p95/p99 达 72.6/76.0 ms，越过 66.67 ms 追帧上限的帧仍偶有出现。
- 关闭态同样受益（无游走逐帧位移也随动画变形逐帧变化）：c64 提交率 15.54→31.96/s、c32 30.58→36.76/s；K=2/4 档的滞后对静止基准渲染无可见影响，K=1 档（4 实例/替换场景）行为与数字与历史同量级（阶段差异在环境漂移范围内，含与节流无关的 present/encode 同向差异）。
- 整窗（interactive）四计数与 polygon_max 全 0，提交率不变；`deferred` 仅出现在 pixel 模式，且在 K=1 用例恒为 0（对照脚本按此断言）。
- 资源侧无异常：两轮 680+680 条样本、678+678 份诊断 snapshot 无查询错误/区域失败/采样溢出；持续场景 measure 首尾 10 秒窗口 PrivateBytes 387.40→388.96 MiB（新开），CPU 约 0.87 核；4 实例替换 100/100/100/100 均完成。GPU 执行时间与显存仍未测量。

原始结果：关闭态 `.artifacts/windows-bench-20261015-213928-7423-a557cf62/`、开启态 `.artifacts/windows-bench-wander-20261015-215058-4990-113fe74a/`（此前一轮 `.artifacts/windows-bench-20261015-212412-8750-1e014734` 因外部鼠标点击命中基准窗口与全阶段环境膨胀作废保留、不改写）；协调目录 `.artifacts/perf-cadence-20261015-212028/`（`01-bench-off.log`、`01b-bench-off-rerun.log`、`02-bench-on.log`、`03-full-gate.log`、`main-cadence-comparison.json`、`verify-cadence-comparison.py`）。harness `ScriptVersion` 3→4（`regions.deferred` 成为必需字段，`-ValidateOnly` 57→59 项）；benchmark 侧新增 `RegionDecision::Deferred` 与 `regions.deferred` 计数，`valid` 不受影响。

最终离线全门通过（同目录 `03-full-gate.log`，`EXIT_CODE=0`）：fmt、locked/offline clippy（全 target，warnings 为错误）、114 项非 GPU 测试（lib 63 + model_pipeline 15 + simulation 17 + render_coordinates 6 + textured_model 13，`--test-threads=1`）、8 项 GPU 测试（NVIDIA RTX 5070 Laptop / Vulkan，无忽略）、debug 与 release 构建。同构建 GUI 复跑：`windows_pixel_hit.ps1` 44、`windows_smoke.ps1` 52、`windows_wander.ps1` 62、`windows_desktop.ps1` 51 项全过、无 fallback。

### 2026-10-15 安装路径批量化决策（单次 CreatePolyPolygonRgn，未实施）

针对上一节定位的残余瓶颈（64 实例安装帧 `native_region_build_install` p50 约 55 ms、p95/p99 越过 66.67 ms 追帧上限），以**先证据后实施**的方式评估了「用单次 `CreatePolyPolygonRgn(points, counts, nPolys, WINDING)` 替换逐多边形 `CreatePolygonRgn` + 2N−1 次内核调用」的批量化方案。Phase 0 离线微基准决策门（`src/platform/mod.rs` 的 `#[ignore]` 探针 `poly_poly_batch_probe`，release、三档 2616/5232/10512 多边形、每档 ≥20 轮 A/B 交错）**否决了整帧单调用**，并留下两条 GDI 边界事实与一个探索性候选：

- **整帧单调用（B）对现有流式平衡合并（A）：p50 比值 2.7 / 5.4 / 11.5**（n=2616/5232/10512；三次独立运行比值一致）。B 每翻倍多边形数涨约 4x（≈二次方），n=10512 时 1328 ms 对 A 116 ms。机制标定：批调用成本由批内多边形数主导（单簇扫描 164→0.68 ms、328→1.83 ms、656→6.10 ms、1312→25.8 ms），与空间跨度无关；验证+flatten 仅 0.42 ms 非瓶颈。等价性硬断言（9 组对抗用例 × 原序/旋转/全反转 + 10512 全量，`RGN_XOR` 差分、含混合绕向归一化）全部通过，但性能门限 ≥0.8 判定停止，**Phase 1 未实施，生产路径未改动**。
- **GDI 边界事实（均 fail-closed，返回错误而非错误区域）**：多多边形批次在 y 跨度 ≥ 约 2^24 时被拒（x 跨度与单多边形无此限制；生产坐标距边界 3–4 个数量级）；一次持有约 10512 个活区域超进程 GDI 句柄配额（~10k），这反证了流式合并「有界存活」设计的必要性。
- **探索性候选（未预注册、单设置）**：按实例分批（每批约 164 个多边形 ≈ 生产形态）+ 位宽归并测得 **53.2/52.3/42.4 ms 对生产序基线 89.1/86.3 ms（约 0.60x）**；对照显示分组排序本身贡献约 23%（115.7→89.1 ms），批调用在分组之上另贡献约 40%。若作为下轮方向需重新预注册决策门并跑 bench 验证。（2026-10-16 更新：该候选已重新预注册——改用真实几何后按 chunk=41 纯分块采纳并实施，见下节。）

证据：`.artifacts/perf-batch-20261015-230238/`（`01-probe.log` 探针输出、`02-gate-decision.md` 判读记录、`03-full-gate.log` 全门 `EXIT_CODE=0`）。全门口径：63 项 lib 通过 + 1 个 `#[ignore]` 探针 + 15+17+6+13 项集成 + 8 项 GPU，fmt/clippy/debug/release 构建。探针保留为复现装置，不随常规测试运行。该节留下的两条 GDI 边界事实继续生效；探索性候选已由下一节重新预注册并实施。

### 2026-10-16 分块批量安装（chunk=41，已实施）

上一节的探索性候选（按批分组 + 单次 `CreatePolyPolygonRgn`）经**重新预注册决策门**评估后实施。与探索轮的关键差异：探针夹具改用**生产形态真实几何**（`Simulation` 直接构建 64 实例 8×8 棋盘、采样 13 帧、决策帧 10512 多边形/59267 点，与归档 c64-pixel 口径一致），因为合成夹具基线（86–89 ms）比真实基线（50.6 ms）高约 1.7x，0.60x 不可外推。门（`src/app/hit_region_tests.rs` 的 `#[ignore]` 探针 `region_batch_gate_probe`，release、6 臂 21 轮配对交错、三次独立运行）：

- 主指标配对中位数：**chunk-41 = 0.3320/0.3313/0.3321**（≤0.70 采纳）；chunk-82 0.370、chunk-164 0.473、chunk-328 0.694、per-instance 0.599（形态决胜: 0.599 ≥ 0.95×0.332 → **纯分块**，应用层零接口变化）；chunk 选择集合 {41} → **`REGION_CHUNK_POLYGONS = 41`**。
- 机制：批成本随批内多边形数超线性（41→164：4x 多边形 ≈ 7x 成本），批数与归并数只随批减小线性增长；build-only 诊断证实归并廉价（~14 µs/次）。
- 等价性硬断言全过：决策帧真几何 × 6 臂 vs sequential oracle、对抗用例 × 3 排列 × chunk{1,2,3,164} × per-instance、空输入/边界错/无效多边形 fail-closed、全路径 live==0（无原生区域泄漏）。两条 GDI 边界事实保持 fail-closed，生产坐标距边界 3–4 个数量级。

Phase 1 生产切换：`set_hit_region` → `chunked_region(polygons, 41)`（`boundaries_from_chunk` + `streamed_batched_region`；每批单次 `CreatePolyPolygonRgn` 立即折入 carry-slot 位宽归并槽，与旧平衡合并同骨架）；`unified_region`/`polygon_region` 降为测试参考，`CreatePolygonRgn` 转 `cfg(test)`，绕向归一化（i128 shoelace）保留在 `batch_points`。应用层无改动（分支 1）。

同机器会话 v4 harness 关闭态/开启态各 10 组（`pass=true cases=10`）+ 上一轮归档对照（对照脚本 `MAIN_PERBATCH_PROTOCOL_PHASE_CONFIG_RESOURCE_AND_COMPARISON_CHECKS=PASS`，680+680 资源样本零异常）：

| 用例 | native p50 ms off 旧→新 | native p50 ms on 旧→新 | 提交/s off 旧→新 | 提交/s on 旧→新 | 丢步 off / on |
| --- | --- | --- | --- | --- | --- |
| 4 | 4.15→1.77 (-57%) | 4.22→1.94 (-54%) | 52.7→53.6 | 53.1→53.4 | 0→0 / 0→0 |
| 16 | 14.43→5.08 (-65%) | 14.94→5.89 (-61%) | 51.8→53.9 | 51.9→53.7 | 0→0 / 0→0 |
| 32 | 27.63→9.22 (-67%) | 28.80→10.93 (-62%) | 36.8→49.5 | 35.8→53.2 | 0→0 / 0→0 |
| 64 | 54.70→17.71 (-68%) | 55.62→21.33 (-62%) | 32.0→47.5 | 31.9→44.9 | 38→0 / 42→0 |
| 64 持续 300s | 54.95→17.68 (-68%) | 55.15→21.22 (-62%) | 31.9→47.7 | 31.9→45.3 | 384→0 / 370→0 |
| 4 替换 100 次 | 3.82→1.67 (-56%) | 3.76→1.89 (-50%) | 52.8→53.5 | 53.6→53.2 | 0→0 / 0→0 |

要点：安装 p95 由 c64 的 62.5→18.6 ms（off），`frame_interval` p95 71.6→29.5 ms 全部用例回到 66.67 ms 追帧上限内；本轮 10+10 组丢步全部清零（上轮节流后残余 38/384 与 42/370 均来自安装帧，至此消除）；降幅随规模单调且小档同样受益（K=1 的 4 实例 -57%），interactive 四档零安装、提交率漂移 ≤1.8%（预注册带 ±15%）；`deferred` 节奏与 K 分档不变（重建份额 {16,32}=1/2、64=1/4）。产物：`.artifacts/windows-bench-20261016-032823-0641-f60e588e/`（off）、`.artifacts/windows-bench-wander-20261016-034302-6570-1d3e38f1/`（on）、协调目录 `.artifacts/perf-perbatch-20261016-015856/`（`01-probe.log`、`02-gate-decision.md`、`03-full-gate.log`、`04-bench-off.log`、`05-bench-wander.log`、`04-bench-comparison.md`、`main-perbatch-comparison.json`、`verify-perbatch-comparison.py`）。

最终离线全门通过（同目录 `03-full-gate.log` `EXIT_CODE=0`）：fmt、locked/offline clippy（全 target，warnings 为错误）、118 项非 GPU 测试（lib 67 通过 + 2 个 `#[ignore]` 探针 + model_pipeline 15 + simulation 17 + render_coordinates 6 + textured_model 13，`--test-threads=1`）、8 项 GPU 测试、debug 与 release 构建。同构建 GUI 复跑：`windows_pixel_hit.ps1` 44、`windows_smoke.ps1` 52、`windows_wander.ps1` 62、`windows_desktop.ps1` 51 项全过、无 fallback。

### 跨显示器场景与任务栏豁免（2026-10-17）

普通启动改为单 overlay 覆盖启动时所有显示器物理包围盒；首次显示前设置 `NonRudeHWND`，替代旧 2px 缺口。视图增加 `center_x`，以主屏尺度与双轴锚点保留既有实例的主屏位置；漫游按实例当前高度的真实可见横段限制，不走包围盒内的无显示区域。DPI/位置事件校正物理客户区，视图与尺寸改变即时更新原生命中；surface 超出实际设备维度限制时在 configure 前明确拒绝。

双屏目标布局（此前真实窗口探针机位，坐标已由单测核验）为主屏 `(0,0)-(2560,1600)`（150%），副屏 `(2560,-511)-(4160,2049)`（175%）；包围盒应为 4160×2560，`span=12`、`center_x=3.75`、`bottom=-3.0046875`。主屏默认实例屏幕 x 应保持 768/1109.33/1450.67/1792。本轮 GUI 运行时仅连接主屏，实际视图为 `span=7.5`、`center_x=0`、`bottom=-0.9`；不将单屏结果当作双屏实机验收。

离线全门通过：`.artifacts/round-cross-monitor-span-20261016-163059/05-full-gate.log`，`EXIT_CODE=0`。fmt、all-targets locked/offline clippy（warnings 为错误）、156 项非 GPU 测试（lib 91 + model_pipeline 15 + render_coordinates 10 + simulation 27 + textured_model 13；另 2 ignored）、8 项 RTX 5070 Vulkan GPU 测试、debug/release 构建均通过。定点审查未发现可确认的坐标/DPI/命中/可见域运行时缺陷。同一构建的单屏 GUI 回归通过：smoke 60、pixel 44、wander 63、desktop 57 项 `[OK]`，均 exit=0；bench `-ValidateOnly` 通过 59 检查。desktop/wander 各有 1 条跨屏条件跳过。`09-gui-*.log` 保留证据；`09-supplemental.log` exit=1，因双屏布局前置条件不满足，在启动应用、输入及截图前退出，未生成接缝截图。用户当前仅单屏，已决定推迟跨屏拖拽/漫游、接缝画面与本轮任务栏 dwell 的双屏实机验收，先开发其他功能。

### 旧简化版动作切换与邦布桌宠（2026-10-17）

以下为换用伊埃斯资产前的旧九骨自制模型验收，不代替新模型的输入或美术验收。旧 `--bangboo` 的两只自制邦布已完成本地功能与画面验收：点击及重复点击有独立反应，拖拽中断双方社交，释放并经过冷却后可重新配对；双方播放独立的挥手问候和弹跳玩耍动画。普通启动此模式默认开启漫游，控制窗 Add 3D 继续添加邦布，Add 2D 保留原纹理2D资产；不读写用户原场景配置。

- 完整离线门：fmt、all-targets locked/offline clippy（warnings 为错误）、debug/release 构建通过；198 项核心/集成测试通过（lib 121、model_pipeline 25、render_coordinates 10、simulation 29、textured_model 13；另 2 个 ignored 探针）。其中 lib 含真实 GPU/GDI 命中覆盖，不将此合计称为纯非 GPU 测试。
- 独立 `gpu_render` 的 8 项测试在 NVIDIA RTX 5070 Laptop / Vulkan 通过。邦布两实例、五动作的 22 个同帧快照覆盖，以及旧动作与区域覆盖均通过。
- 最新真实 Windows GUI：`windows_bangboo.ps1` 33 项、默认 `windows_smoke.ps1` 60 项通过，无区域 fallback；bench `-ValidateOnly` 59 项通过。主代理已查看本轮全部 7 张邦布截图，确认双方挥手、玩耍相位变化、点击反应及拖拽移位，不以日志代替画面。
- 资产落盘自检 1084 项零失败；生成器的结构失败保留、检查 CLI 非零退出、失败不写资产三个负例通过。

验证日志：`.artifacts/round-bangboo-20261017/` 的 `05-full-gate.log`、`17-windows-bangboo.log`、`18-windows-smoke.log`、`19-bench-validate.log` 均 exit=0；截图：`.artifacts/windows-bangboo-20261017-032326-4390-f25398dc50e44e3db30138a4c9e5af54/`。双屏实机验收仍按此前决定后延。

### 伊埃斯现成模型适配（2026-10-17）

`--bangboo` 已改用 OSad《Bangboo 01 - Eous》的本地转换资产，原自制九骨版保留。角色为 11331 顶点、18224 三角、22 骨、2048×2048 内嵌贴图，当前 v20 文件 6893849 字节，未放宽原 8 MiB 限制。展示凳子不进入桌宠；复用原待机骨架并修正待机循环的静止帧跳变与脚底，另适配行走、点击反应、问候和玩耍。来源、署名、修改及复现命令见 `assets/examples/bangboo-eous.md`。

- 用户指出转头时头罩斜压、环眼变水滴后，实际窗口已复现。根因是源眼睛平均仅约11%跟随 head，与运行时单 head 驱动不匹配。局部统一眼环、面板和头罩主体绑定，按焊接拓扑在头罩下方做两环窄颈圈；保留耳链、围巾、五动作及全局60度上限，不靠关闭社交掩盖问题。
- 转换器 10 项 Python 测试、81 项定向 Rust 回归通过；新增头面共同变换检查在旧 v7 上先红、v9 上转绿，覆盖五动作、正负30/60度及无驱动。严格 all-targets locked/offline clippy、fmt 与 release 构建通过。
- 使用 v9 的两实例、五状态 22 帧 GPU/native 同帧轮廓覆盖通过，主代理查看了 Idle/Greet/Play/Reaction/Walk 图。适配后288个五权重顶点压四，在2406个采样姿态中最大误差0.007322、全顶点均值0.00000683，低于原0.0075/0.0005预算。原绑定到有意头面适配的采样最大位置改变约0.435232，另计；不能将四槽误差冒称对原作无损。
- 同姿态正面/三分之四的24张修复前后对照已逐图查看；v8颈部折痕候选未采用，v9将过渡移至围巾遮挡内。实际 release 有界窗口再次预览通过：40秒自然退出，`frames=845`，双方 Greet→Play→End 循环，无 fallback/加载失败；主代理查看四张问候/玩耍客户区截图，未再见原头罩斜压与眼环非刚性拉扯。未注入鼠标输入，新资产物理点击/拖拽 GUI 尚未复验；不沿用上节旧模型33项结果，也不代表用户已认可最终外观。

本轮工件根：`.artifacts/round-eous-20261017-124825-cbe67405/`。v9转换报告 `eous-head-v9-report.json`；旧缺陷回归与新验证见 `17` 至 `28` 前缀日志；同姿态对照 `head-comparison-v9/contact.png`；GPU图 `.artifacts/eous-gpu-3912-1792217292604308800/`；真实窗口图及日志 `app-preview-20261017-141535-1e17e7290790474c881a00994bc90974/`。GPU覆盖的截图目录创建已改为 `create_dir_all`，避免缺少 `.artifacts/` 父目录时失败。没有改默认四示例、冻结 bench、Cargo.lock 或运行时导入架构；未自动 Git 或公开上传模型。

### 伊埃斯前后小碎步（2026-10-17）

v18将旧的Z轴左右张合改为X轴前后迈步：髋基线-22度、左右正负13度，摆动膝从44度平滑屈至62度，左右错开半周期，0.8秒一循环。入口相位前移四分之一周期以平衡待机与点击反应后的起步；根骨保持25度Y朝向，整实例镜像覆盖左右方向，正交相机不变。源脚踝骨没有蒙皮权重，可见抬脚来自膝部；公共根骨高度以实际脚底校准，不用整身跳动代替迈步。

- 实际脚底前后行程0.07404/0.07408世界单位、侧摆小于0.00000006；摆动脚相对支撑脚抬高约0.00969，支撑最低点最大0.002744。真实Simulation覆盖起步、停步、行走中点击打断及反应结束后立即再起步，完整链最低点为正0.000002513；原地板下限-0.0005、支撑<0.003及拟合预算不变。Simulation固定步为60Hz，另有120Hz单clip插值检查。
- 几何、UV、贴图、骨架与v9相同，其余四段动作通道逐值相同；重新拟合使57个导出顶点的四槽权重变化。2406姿态的最大拟合误差0.00731596、全顶点均值0.00000709，保留既有头面与耳链检查。
- Python12项、70项定向Rust、严格all-targets locked/offline clippy、fmt及release构建通过。两实例五状态的22帧GPU/native覆盖和新增8相位双方向覆盖均通过，共30帧。主代理查看了双方向32张A/B接触表、五状态GPU代表图与全部8相位图。
- 开启漫游的真实release预览通过：60秒自然exit0、`frames=964`，12张截图均以fresh Walk日志及前后相位核验；主代理已查看全部行走截图。未注入物理输入或留下常驻实例，不把此结果当作新模型点击/拖拽的物理GUI复验或用户最终美术认可。

同一工件根中：`eous-walk-v18-report.json`、`walk-comparison-v18/walk-before-after.gif`、`walk-v18-gpu-contact.png`、`walk-v18-app-contact.png`；日志 `logs/walk-v18-*.log`。GPU原图 `.artifacts/eous-gpu-39848-1792228218168896600/`；真实窗口原图 `walk-app-preview-20261017-171137-4d0d0995358a4e7b98ca50d3c6065c1d/`。本次仅修改离线步态、资产和定向验证，不改运行时相机、动作状态机或其他功能。

### 伊埃斯耳部截断与性能诊断（2026-10-17）

已复现旧 K4 节流使当前动画越过上次安装区域：合成 10px 移动及真实伊埃斯 desktop 动作均漏掉 GPU 可见像素。`SetWindowRgn` 同时裁剪画面，因此不能只把它当作输入区域更新问题。

当前候选保留 8px 余量，以 4px 死区、7px 强制更新边界约束复用；身份、拓扑、尺寸或视图变化立即安装。另将重复角点蒙皮改为每实例每帧按被引用顶点缓存，并把 raw 采集与整数区域物化分开。19 项轮廓测试与 16 项应用根单测通过，包含两项先红转绿的时序覆盖；K4 的安全 Deferred 帧和累计越界也检查当前 GPU 像素覆盖，未改变模型或画质。fmt、严格 clippy 与 release 构建通过。35 秒有界无输入实窗完成 24 张截图和 24 次 `GetWindowRgn` 回读，空白角始终在区域外；已亲看问候、玩耍及行走代表画面，未见原先耳尖截断。该预览不代替同帧全像素覆盖或物理点击/拖拽复验。

**该候选未通过性能验收，不是已完成的优化版本。** 同 AMD/Dx12、2560×1600 desktop 配置下，修前/候选各三轮 35 秒交替对照的中位数为：

| 指标 | 修前 | 安全候选 |
| --- | ---: | ---: |
| 实际提交率 | 21.58/s | 10.95/s |
| 每帧几何总耗时均值 | 29.42ms | 30.97ms |
| 每帧原生建区摊销均值 | 13.62ms | 54.81ms |
| 单次原生建区 p50 | 54.42ms | 54.72ms |
| 帧间隔 p95 | 89.39ms | 93.85ms |

真实动作的裁剪/退化剔除后拓扑频繁变化，安全分类使安装份额从约 25% 升至 100%；不是 lazy 物化本身禁用节流，也不是单次 GDI 安装变慢。分项探针测得 raw p50 6.58ms、物化 p50 23.41ms。后续须优化物化或原生建区本身，不能恢复会漏区的旧节流来换取提交率。提交率不是 GPU 执行 FPS。

证据根 `.artifacts/eous-region-20261017-180543/`：`paired-comparison-v2.json`、六轮 `logs/{baseline,optimized}-{1,2,3}.err.log`、`logs/cpu-probe-final.log`。原对照二进制与 v18 资产保留，临时运行时计时接线已移除；独立计时桶缺失以 `null` 表示，不当作零成本。

### 伊埃斯精确原生建区（2026-10-18）

在物化后不变的多边形上增加一条整数扫描线快路径：逐行求精确区间、按位集做真正并集、纵向合并规范矩形带，一次 `ExtCreateRegion` 建区。适用条件为至少 16,384 个多边形、每个不超过 32 点的严格凸简单多边形，包围盒各轴 ≤16,384、位集 ≤64,000,000 像素、总点数 ≤2^22、扫描工作量 ≤64,000,000、输出 ≤262,144 个 RECT；不符合条件的输入（低密度、非凸、自交、超预算）原样走 chunk-41，非法输入、算术/分配错误与 `ExtCreateRegion` 失败仍是错误而非降级。填充规则不靠猜：每次测试运行都用 `calibrate_fill_rule` 对 GDI 现场标定，16 个候选规则中恰有一个能复现 GDI。

**性能预注册门未通过，本轮不作为优化交付宣称。** 同 AMD/Dx12、2560×1600 desktop、scale 1.5、两只 v18、无输入，旧 chunk-41 / 安全候选 / 新快路径三方各三轮 35 秒交错：

| 门项 | 要求 | 实测 | 判定 |
| --- | --- | --- | --- |
| 三组配对提交率比 | 全部 > 1 | 1.0517 / 1.0529 / 1.0769 | 通过 |
| 配对中位 | ≥ 1.10 | 1.0529 | **未通过** |
| 新提交中位 | ≥ 23.74/s | 22.067/s | **未通过** |
| 新中位 / 安全候选中位 | ≥ 1.5x | 22.067 vs 10.735 = 2.06x | 通过 |
| 帧间隔 p95 不劣于旧版 | 每轮成立 | 约 49 对约 90 ms | 通过 |
| failed | 0 | 0 | 通过 |

快路径把单次原生建区 p50 从约 54.5 ms 降到约 8.6 ms（削掉约 46 ms）且可复现。更正耗时解释：`geometry_ms` 约 30.5–31.2 ms 是收集与物化的合计，已包含其中约 24.2 ms 的物化，不能再相加成约 55 ms；独立 CPU 分段探针在 1440px 高视图测得收集 p50 6.5842 ms、物化 p50 23.4102 ms，不冒充 1600px 实窗的直接分段结果。安全分类下实际每帧安装，而旧路径多数帧按 K4 推迟，因此单次安装节省不能直接等同端到端收益。端到端提交率实测只从 20.86/s 升到 22.07/s（+5.3%），低于 +10% 的门。第 3 轮 baseline 被本会话并发 release 构建污染（14.654/s、回调 p95 173 ms，平时约 20.8/s），故另跑第 4 轮；按 1-3 轮与按 1、2、4 轮两种口径判定一致，均为未通过。

经用户确认，快路径**保留在生产调用点**而不回退 chunk-41：它与旧路径严格等价、把最贵的一步降了约 5.4 倍，代价是每帧都重建区域而非按 K4 推迟。剩余性能工作需另立范围，本轮不并入物化优化。

正确性与资源证据（失败门之外的部分全部通过）：

- **集合严格等价**：边界矩阵（各方向三角形、正负分数斜率、经过顶点的行、共享边、混合绕向、退化、负坐标、跨屏坐标）与固定 seed 随机凸多边形，快路径 == chunk-41 == sequential，`RGN_XOR` 全空；另有单像素差异灵敏度反例。真实 Eous 240 帧在 desktop 视图逐帧相等；新增 `region_scan_preview_view_probe` 覆盖 `--bangboo` 实际使用的 LEGACY 视图 + 960×640，240/240 帧相等、零 decline（既有等价测试只覆盖桌面视图）。
- **可见像素覆盖**：新增 `eous_preview_view_gpu_coverage_probe`（LEGACY + 960×640、540 帧、18 个采样相位）经生产 cache 交付后逐点检查 GPU 实际渲染像素，零漏区。另有 24 次真实窗口区域回读 + 24 张截图：重复采集 24/24 零越界，旧 chunk-41 对照 24/24 零越界；区域回读→截图→再回读的 24 帧中 22 帧前后完全一致。原始验收的唯一异常帧（61 像素、alpha 全 255 的不透明深灰、距边界 1-2px、位于区域 bbox 内）判定为区域安装与屏幕合成之间的姿态前进，不是几何缺口。
- **分配失败 fail-closed**：扫描在每帧安装回调内执行，因此所有暂存列表（`rects`、`active`、`runs`、`events`）都经 `push_checked` 按需 `try_reserve` 增长，失败返回错误而不是 `handle_alloc_error` 终止进程；位集与 RGNDATA 缓冲亦为 `try_reserve_exact`。不按预算上限预分配，因为上限是上限而非预期尺寸——探针实测真实帧只填 `rects_p50=682`、`rects_max=935`（约 15 KiB），而按上限预分配是 4 MiB；改按需增长后 P0 微基准配对中位 0.1264，与改动前的 0.1261-0.1268 同档，即这次改动对扫描耗时的作用是中性的、没有可测影响。
- **字节上限是独立检查**：`MAX_SCAN_BYTES` 显式定义为 `MAX_SCAN_RECTS * 16 + 32`。扫描循环内矩形数先触顶，所以该分支由 `region_from_scan` 自身保证——它对调用方交来的任意 `Scan` 都必须成立；新增 `region_from_scan_rejects_a_list_over_the_byte_budget` 覆盖恰好等于上限（可建）与超出一个矩形（拒绝）两侧。
- **凸性判据**：只靠局部叉积符号会漏收自交星形，因此用转向数（绕 +x 射线的方向穿越计数）判定，简单闭合链的转向数为 ±1。测试改用半径 100 的正规 {5/2} 五角星与 {7/3} 七角星，正反两个绕向都拒绝——此前的"七角星"夹具反转后与五角星逐字节相同，实际只测了同一形状。

离线门通过：fmt、all-targets locked/offline clippy（warnings 为错误）、release 构建、144 项 lib 测试通过 + 8 项 ignored；两个 `GetGuiResources` 泄漏测试在独立进程中用 `--ignored --test-threads=1` 实跑通过。证据根 `.artifacts/eous-region-native-20261018-002859-e88823e91f78d698/`：`01-scope.md`、`03-findings-p2.md`、`logs/gate-evaluation.txt`、`logs/compare.json` 与各轮 `logs/{baseline,safe,native}-*.err.log`、`scripts/`。本轮未改资产、贴图、耳动画、相机、动作/社交/漫游、蒙皮、schema、依赖、Cargo.lock、4px 死区、7px 强制边界、8px 余量或冻结 bench 协议。

### 命中轮廓 scratch 阶段结果（2026-10-18）

后续单线程帧内缓冲复用候选减少了中间分配；收集与物化的几何顺序、8px 余量和原生扫描线路径不变。独立冻结旧实现对照 492 帧的 raw 浮点位与完整整数多边形序列全部相同，release 常规测试 243 项通过、10 项 ignored；两个 GDI 资源探针、240 帧原生预览等价和 540 帧/18 相位 GPU 覆盖另行实跑通过。fmt、严格 all-targets clippy、debug/release 构建通过。

**本轮阶段性能门仍未通过，不作为已验收优化交付。** 三轮相同输入交替测量的 p50（ms）：

| 轮次 | 旧收集+物化 | 新收集+物化 | 新收集 | 新物化 | 阶段门 |
| --- | ---: | ---: | ---: | ---: | --- |
| 1 | 31.3633 | 18.6625 | 2.3767 | 16.2630 | 物化 >16ms，未通过 |
| 2 | 31.5699 | 18.6595 | 2.3591 | 16.2640 | 物化 >16ms，未通过 |
| 3 | 27.6627 | 17.6315 | 1.8309 | 15.8014 | 本轮通过 |

合计为直接计时，不是两个中位数相加。预注册要求每轮收集 ≤5ms、物化 ≤16ms、合计 ≤20ms；前两轮物化超出要求，第三轮通过不能抵消。已停在阶段门，未继续实窗帧率测量或最终截图验收；这些 CPU 阶段数字不能当作实际帧率改善。候选源码、冻结基线与证据保留，是否保留候选或继续调整需另行决定。证据：`.artifacts/eous-materialize-20261018-020834/00-protocol.md`、`01-findings-stage.md` 和 `logs/stage-performance.log`。

### 有限排序键新候选：阶段与实窗性能门通过（2026-10-18）

首轮失败保留。继续归因后，在帧内 scratch 基础上预计算有限 f32 点的 u64 数值排序键，并保留原坐标位与 ordinal，保持 signed zero 的稳定代表和完整多边形顺序；裁剪中间值溢出则在发布轮廓前明确拒绝。没有降低资产/贴图质量、关闭动画、扩大命中区域或改变原生建区路径。

新候选 release 常规 247 项通过、12 项 ignored；两个 GDI 探针分别实跑、492 帧冻结位等价、240 帧原生预览与 540 帧 GPU 覆盖通过；fmt、严格 clippy、debug/release 构建通过。相同 5/16/20ms 阶段门一次通过：三轮收集 p50 1.91–2.35ms、物化 13.91–14.36ms，直接合计 15.80–16.68ms。

同 AMD 集显/Dx12、2560×1600 桌面、两只 v18、无输入的三对 35 秒实际窗口对照：

| 配对 | 基线提交/s | 新提交/s | 新回调 p50 ms | 基线间隔 p95 ms | 新间隔 p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 23.2465 | 38.4895 | 24.8783 | 47.2502 | 28.0890 |
| 2 | 23.5145 | 38.2685 | 25.1104 | 45.5279 | 28.2363 |
| 3 | 23.5219 | 38.5861 | 24.8414 | 45.0490 | 28.2502 |

配对提升中位约 64%，新提交率中位 38.49/s；每轮归一模拟丢步比两臂均为 0，原定六项实窗门全部通过。提交率不是 GPU 执行 FPS。最终无 profile release 已重建（SHA256 `3a3c8519b6edadc42de0a8a05ffce7caa75f9df680fb0026911e6ba496730cb2`）；24 张连续截图及 48 次前后区域回读完成，四个空白角均在区域外，主代理亲看总览及问候/交叠/转身/行走原图，未见耳尖截断。35 秒自然退出、无 fallback 或进程残留；截图回读不替代同帧 GPU 覆盖，不代表物理点击/拖拽或双屏复验。证据：`.artifacts/eous-materialize-followup-20261018-030338/01-findings.md`、`logs/stage-performance.log`、`logs/real-window-evaluation.json`。

### 光标、拖拽与边缘互动：本轮实现与验证完成（2026-10-18）

当前工作树已接入光标转头、屏幕/工作区/普通窗口边缘关注、独立待机小动作、拖拽倾向与释放回弹、连续点击反馈。边缘功能目前是关注和姿态反馈，不是窗口碰撞、攀爬或站上窗口。光标与外窗仅做有限频率的只读查询，不读取窗口标题或内容；新姿态仍由同一 RenderFrame 交给渲染、拾取与原生命中。普通应用启用新互动，smoke/bench 保留关闭路径。

14 项互动测试、4 项应用投影/过滤测试及 5 项点击/拖拽定向测试通过，fmt 和 release all-targets 严格 clippy 通过。独立审查发现的转头目标切换突跳已经先红复现并修正为共享平滑过渡，窄范围复审通过。

首轮真实蒙皮测试发现基础 Reaction→Idle 混合时脚底下穿；关闭新互动的同轨迹也失败，不能归因于新待机变体。旧轮 **6 通过、1 失败** 的原始证据保留在 `.artifacts/eous-interactions-20261018-034550-0e23f583/`。随后对 v19 只离线校准受惊动作首尾各 0.15 秒的髋腿轨道，保留网格、贴图、权重、骨架、其它四段动作及运行时混合语义；不增加逐帧接地扫描，也不放宽地板门限。

v19 的 8 项实际资产测试、9600 帧持续待机/恢复中再次点击采样和 6 项校准工具单测通过。890 个新姿态的完整权重/四槽位置误差也通过原预算，详见 `assets/examples/bangboo-eous.md`。本轮完整离线回归为 **272 通过、0 失败、13 ignored**；fmt、release all-targets 严格 clippy、debug/release 构建通过。新增硬件 GPU/native 探针覆盖 26 时刻 × 2 视图 × 2 朝向，共 104 组；主代理亲看 26 帧总览和 4 张原图，未见耳尖截断或头面分离。

同 v19 内嵌资产、同 AMD 集显/Dx12、2560×1600 桌面的三对无 profile 实窗对照如下。口径是完整进程生命期的提交数/墙钟秒，双方都含启动、暖机及退出，不是 GPU 执行 FPS，也不能与上一节暖机后 profile 数字直接相减。

| 配对 | 基线提交/s | 新版提交/s | 新版/基线 |
| --- | ---: | ---: | ---: |
| 1 | 36.91 | 36.73 | 0.9953 |
| 2 | 37.49 | 36.92 | 0.9846 |
| 3 | 37.54 | 37.08 | 0.9878 |

配对比中位 **0.9878（约 -1.22%）**，原定中位 ≥0.90、每对 >0.85 的门均通过。真实应用另完成 24 张连续截图与 48 次前后原生区域回读，四个空白角均在区域外；主代理亲看总览及问候、交叠、转向原图，未见耳尖截断。截图/前后区域并集诊断偏差均为 0，但不替代同帧 GPU 覆盖。本轮未注入物理点击/拖拽，双屏实机继续后延；不是用户美术验收声明。

随后修正一处朝向缺陷：3D 骨骼行走时曾按方向写入镜像，导致围巾上的“01”字样在向左行走时被整体翻转（着色器按顶点属性直通 UV，镜像来自根矩阵）。镜像现在只作用于 2D 精灵，3D 骨架始终保留资产原始朝向。该不变式在 `wander_step`、`render_frame` 与 `replace_model` 三处同时强制，陈旧或外部构造的状态也无法重新引入。

去掉镜像后暴露出第二处缺陷：行走片段把根骨固定在 +25 度朝 +X，原先正是镜像在“显示”左向；只保留不镜像会让邦布左走时倒着走。现在 3D 方向改由 `WanderState::turn`（每 tick 缓动 16%，`-1.0..=+1.0`）经 `DriveInput::turn` 传入 `apply_skeleton_drive`，对根骨施加额外 Y 旋转：`+1.0` 保持原始朝向，`-1.0` 把作者朝向关于正面轴反射成对称角度。旋转而非镜像，因此 UV 不变（围巾仍是“01”）、每个顶点 Y 不变（接地不受影响），头部朝向会减去其继承的根 yaw。对根骨没有旋转通道的其它 3D 资产，该旋转恒为 0（无操作）。

新增回归：`skeleton_walking_left_turns_the_body_without_mirroring_it`、`skeleton_walking_right_keeps_the_authored_orientation`（合成夹具，绑定姿态中立、25 度放在片段里，与真实资产同构）与 `eous_walking_left_faces_left_without_mirroring_the_scarf`（真实 Eous，断言每段连续左走 12 tick 内身体必然转向左，且根矩阵 X 轴恒为 `+1`）。重跑 `--test simulation` 31 项、lib 165 项、`attention` 14 项、`eous_asset` 9 项及 release 全套通过（**275 通过、0 失败、13 ignored**），fmt、严格 clippy、debug/release 构建与硬件 GPU/native 探针、两个 GDI 泄漏测试均通过；14:14 的有界预览完成 24 张截图与 48 次区域回读并自然退出（`captures=24 region_readbacks=48 input=none`），主代理亲看行走帧，左走时身体侧向正确，围巾字样仍为未被横向镜像的“01”。本轮未重跑性能对照，交互、渲染管线其余部分未改动。

标准 release 已更新为本轮版本（SHA256 `A5C532BEEA68BBE8A5CCBC0BE78B816824B3765799CF0075D8CB0466B1CAEACD`，24,467,968 字节），不再与 `attention-candidate-v19.exe` 逐字节相同；冻结候选保留为修正前证据。运行：

```powershell
.\target\release\delicium.exe --bangboo
```

本轮证据：`.artifacts/eous-grounded-reaction-20261018/01-findings.md`、`whole-run-v3/evaluation.json` 及 `whole-run-v3/comparison-provenance.json`（后两者均位于同工件根）。旧失败日志保留，未自动提交或推送，测试实例均已退出。


### 伊埃斯 v21 直腿摇摆步：离线与实窗验收（2026-10-18）

实机观感反馈"走路不太依靠膝盖"后，walk 片段由膝部 44..62 度泵动改为膝整周期固定 44 度、髋以 -28 度基线摆动 ±16 度，抬脚改为大腿局部约 10 毫米的摆动期平移；上身补 spine_01 反向侧倾（Z 10 度、Y 2 度、X 2 度）、head 反向侧倾 5 度、手臂随髋摆动、Ear_02 滞后 0.12 周期。改法同时落在 `tools/convert_eous.py` 的 `walk_angles` / `walk_lifts`（重新导出可复现）和 `tools/reauthor_eous_walk.py`（只重写成品 walk，其它四段与网格/骨架/贴图解包后摘要断言未变）。

离线：`--test eous_asset` 10 项通过——前进摆幅 0.08982/0.08985、侧向 4.5e-8、间距 0.2139、支撑脚最高 0.002975、摆动抬脚 0.0100/0.0100、半周期分离 0.0902/0.0895；交叉渐变离线最差 idle→walk +0.000906、walk→idle -0.0000967，运行时 1800 tick 实测最低 +0.0000748；120 Hz 插值地板 +0.002669；循环首尾逐值相等。`tools/test_convert_eous.py` 12 项通过。release 全套 **333 通过、0 失败、19 ignored**（lib 222、attention 14、eous_asset 10、gpu_render 8、model_pipeline 25、render_coordinates 10、simulation 31、textured_model 13）；fmt 与 release all-targets 严格 clippy 干净。

实窗（`target/release/delicium.exe`，SHA256 `e34d6c1bafc5ee60e6116cd4df454584705c93b5961214cb606631618f68bdfb`，24,466,944 字节）：smoke、pixel_hit、desktop、bangboo、wander、actions 六套 GUI 脚本全部通过；bounded 960x640 预览 24 张截图 + 48 次原生区域前后回读完成并自然退出（`captures=24 region_readbacks=48 input=none`），四个空白角始终在区域外；主代理亲看行走帧（scene-08/10/21/22），姿态为直腿摇摆、无膝部泵动、脚未穿地。`windows_bench.ps1 -Quick` 快速接线通过。

口径：本轮未对照《绝区零》实机视频逐帧核对（会话无联网视频工具），"更接近游戏"仍需用户实机确认；未重跑性能对照，不声称端到端帧率变化；双屏实机继续后延。成品 6894987 字节、SHA256 `8fdeef3b…`，v20 备份 SHA256 `d4dbd78a…` 保留在 `.artifacts/eous-walk-reauthor-20261018/`；实窗证据在同目录 `preview/final-preview-20261018-233506-d289966c301c45b89cdafe187f51b2d0/`。

### 模型库、设置开关、多语言与会话框：离线与实窗验收（2026-10-19）

本轮把邦布从引擎里的特殊分支降为模型库中的一个条目，并新增启动器设置页开关、界面语言与桌宠会话框。范围与边界：模型库为内建清单加 `%APPDATA%\Delicium\models` 用户目录，不做元数据、缩略图或导入向导；语言只有简体中文与 English；会话接口为 OpenAI 兼容的 `/chat/completions`，另有不联网的本地台词兜底；没有下载任何游戏文件或外部资产。

离线门（`fmt`、release all-targets 严格 clippy 与下列测试，全部 `EXIT_CODE=0`）：lib 270 通过 / 19 ignored，`attention` 14、`chat_loopback` 5、`eous_asset` 10、`gpu_render` 8、`model_pipeline` 25、`render_coordinates` 10、`simulation` 31、`textured_model` 13，共 **386 通过、0 失败、19 ignored**。两个 `GetGuiResources` 泄漏测试在独立进程中以 `--test-threads=1` 实跑：`repeated_builds_do_not_grow_the_process_gdi_object_count`、`repeated_chunked_builds_do_not_grow_the_process_gdi_object_count` 均通过（`2 passed; 0 failed; 287 filtered out`）。

新增或改写的测试按模块分布在 lib 测试里：`library.rs` 5 项（内建 id、用户名合法性与上限、目录不递归、内建同名文件被忽略）、`preferences.rs` 8 项（偏好门控、未知字段保留、原子写）、`i18n.rs` 4 项（语言判定与切换）、`app/chat/mod.rs` 10 项（端点拼接、仅回环允许明文 http、恶意 URL 拒绝、key 不进 URL、上下文轮数与截断、OpenAI 响应解析、系统提示词）、`app/chat/session.rs` 9 项（本地兜底、空白消息、中文台词、不安全 URL、在途请求拒绝重发、双角色历史、历史上限与清空、完成回写）。`tests/chat_loopback.rs` 5 项走真实 WinHTTP 传输：回环请求成功、错误状态带端点消息、不跟随重定向、非 JSON 响应按错误处理、会话端到端追加。

八套 Windows GUI 脚本全部通过，其中六套引擎脚本显式指向同一个 `target/release/delicium.exe`（SHA256 见下节）；逐项断言数：`windows_smoke.ps1` 60、`windows_pixel_hit.ps1` 44、`windows_bangboo.ps1` 33、`windows_wander.ps1` 63（另 1 项因本机无右邻副屏 `[SKIP]`）、`windows_desktop.ps1` 57（另 1 项跨屏拖拽 `[SKIP]`）、`windows_actions.ps1` 63、`windows_settings.ps1` 20（19 处断言 + 末尾通过行）、`windows_chat.ps1` 18（16 处断言调用点，其中输入守卫在两次发送各执行一次，另加末尾通过行）。`windows_bench.ps1 -ValidateOnly` 59 项自检通过，未跑基准用例。所有实窗运行均无命中区域 fallback。

- 解耦的直接证据在 `windows_bangboo.ps1` 与 `windows_smoke.ps1`：`windows_smoke.ps1` 断言两个 Add 按钮合计新增恰好 2 个实例；`windows_bangboo.ps1` 进一步断言 `Add 2D` 得到贴图版 2D 示例（`kind=sprite2d`、资产 id 与邦布不同），而桌宠自身追加实例改由模型库 `Eous Add` 完成，日志为 `spawn instance=3 asset=<同一驻留资产> kind=library`，证明复用已驻留资产而不重新读盘。两套脚本的按钮坐标是本次重测修订的：加入命中区域/漫游复选框与模型库区后旧位置已下移，探针截图见 `.artifacts/gate-20261019/probe/control.png`。
- `windows_settings.ps1` 验证设置页：英文标题与重本地化、命中区域/漫游/语言控件存在、未知顶层字段与 `chat` 内未知字段在保存后保留、非 https 远程地址被拒且不会启用远端、Base URL/模型名/API key 写入、已存实例不变、保存提示按页面语言显示。
- `windows_chat.ps1` 默认指向 release 引擎并用临时 `APPDATA` 与本机回环 HTTP 夹具（`127.0.0.1`，由脚本自身持有）驱动真实会话面板：发送前零请求、请求行为 `POST /v1/chat/completions`、API key 只在 `Authorization: Bearer` 而不在请求行、配置的模型名与用户消息到达、等待/回复/本地兜底三态截图有可见差异、运行中把 `chat.enabled` 改为 false 后引擎打印 `applying the configured chat settings: enabled=false` 且不再发起请求、进程以 0 退出。用户真实配置与 APPDATA 未被读写。

实窗证据（release 引擎复跑）：`.artifacts/windows-20261019-022216-6911/`（smoke）、`windows-pixel-20261019-022234-2357/`、`windows-bangboo-20261019-022241-2266-5d8fa9ef1a924cc79a40290b455eb7e4/`、`windows-wander-20261019-022326-6851-9f54180b1e7b42528d811e57eeee1e06/`、`windows-desktop-20261019-022407-6452/`、`windows-actions-20261019-022411-8590-2825e1687eb04bb29e8c7e65a7d2fd8b/`、`settings-page-20261019-020807-0083-5950aa2c0f1c495d86b14d3228864a4b/`、`chat-panel-20261019-021602-7433-61bfbf8be8a545cea77c6e8ae7122d39/`；离线与脚本日志在 `.artifacts/gate-20261019/`（`test.log`、`clippy.log`、`fmt2.log`、`gdi-leak.log`、`*-rerun.log`、`*-release.log`、`bench-validate.log`）。

标准 release 为本轮构建：`target/release/delicium.exe` SHA256 `5558ECADF2D5800A4E4CEF15CC2BA79B589C217A134EA310107970EDC48C6C0C`（24,671,232 字节），`target/release/delicium-launcher.exe` SHA256 `477C66F6FB59FB2CB042DEF675314E3E6813233376A630BAFF2C5DDBF90AF831`（2,092,544 字节）。

口径：会话传输只在回环夹具上验证，**未向任何真实第三方 LLM 服务发送过请求**；API key 以明文存放在 `%APPDATA%\Delicium\config.json`（仅 Debug 输出脱敏），应视为本机凭据。模型库不扫描子目录、不解析元数据；语言仅两种；双屏实机继续后延。本轮未重跑性能对照，不声称帧率变化；未自动提交或推送。

### 单只默认桌宠与设置页非置顶：离线与实窗验收（2026-10-19）

用户现场反馈三点：设置页看起来置顶；`--bangboo` 应默认只生成一只；设置页显示 `Engine: not installed` 且更新检查失败。本轮的实际改动只有两处：`--pet`/`--bangboo` 的起始场景从两只改为一只；设置页增加"非置顶"回归断言。其余两项经实测确认不是代码缺陷，结论见下。

**单只默认。** `src/app/mod.rs` 的 pet 分支原先提交两个位置，现在只提交 `Vec3::new(0.0, 0.15, 0.0)` 一个实例；无参数普通启动仍是四个示例实例，未受影响。第二只不再由引擎预置，而是在控制窗模型库对 `Eous` 点 `Add` 加入：`UiCommand::AddLibrary` 命中同一来源时复用已驻留的 `pet_asset`，日志为 `spawn instance=2 asset=<同一资产> kind=library`。`next_spawn_position` 把第二个实例放在 x=-1.1，距居中的第一只 1.1 世界单位，落在 0.8–1.6 的配对带内，因此问候/玩耍仍按原逻辑触发。

`tests/windows_bangboo.ps1` 相应改写：启动后断言 `spawn instance=` 恰好一次；先完成命中区域、场景视图与穿透背景的基线断言，再从模型库点加第二只，并断言其资产 id 与初始邦布相同；三处角色锚点由 `-0.65` 改为 `0.0`（初始邦布在场景中心，与左侧 1.1 单位的第二只轮廓不重叠）；拖拽释放的世界坐标区间改为 `(0.1, 0.45)`；后续第三次库添加与 `Add 2D` 的坐标随两行实例列表下移至逻辑 y=298 / y=122。整套 34 项断言（含 7 次截图采样）通过：`WINDOWS_RESULT pass=true artifacts=.artifacts/windows-bangboo-20261019-104208-9299-b2df557df9e541ecb492a3afc5382b32`。

**设置页非置顶。** `src/launcher/settings.rs` 创建窗口用的是 `CreateWindowExW(0, ...)`，样式只有 `WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU`，本身没有 `WS_EX_TOPMOST`。实窗探针读取 `GetWindowLongPtrW(h, -20)` 得到 `exstyle=0x100 topmost=False`（`0x100` 是 `WS_EX_WINDOWEDGE`）；把一个普通记事本窗口移到页面中央后，`WindowFromPoint` 命中的是记事本而非页面，证明页面不在置顶带。用户看到的"置顶"来自早期演示截图脚本：它在 01:24:27Z 的捕获调用里用 `SetWindowPos(..., HWND_TOPMOST, ...)` 把页面临时提进置顶带且未恢复。`tests/windows_settings.ps1` 因此新增一条回归断言，在页面打开后直接读扩展样式并断言 `$extended -band 0x8` 为 0；实跑输出 `[OK] The page is not an always-on-top window (ex-style 0x100)`，全套 21 项断言通过（`.artifacts/settings-page-20261019-105112-3866-7adefcdca191437eb22e2fdb9ac9f950/`）。捕获辅助函数在截图期间临时提层的 `Raise`/`Lower` 保留，但它不再影响页面自身的样式。

**`Engine: not installed` 与更新检查失败（诊断，非缺陷）。** 设置页的引擎行读的是 `Shared::target()`，即 `%LOCALAPPDATA%\Delicium\app` 下已安装的引擎版本；本机该目录与 `staging` 都是空的（只有被复用的启动器 exe、日志与备份），所以如实显示未安装。`--settings` 是独立模式（`standalone = options.settings`），不启动引擎，`--engine PATH` 只给"启动桌宠"提供二进制，不会改变引擎行——这是设计如此，不是本地引擎失联。更新检查失败是网络路径问题：本机 `curl https://github.com/...` 在 20–21 秒后返回 `http=000`（`api.github.com` 0.79 秒 200、`objects.githubusercontent.com` 0.95 秒 404 均正常，系统未设代理）；启动器日志对应 `WinHttpSendRequest: the request timed out`，清单解析成功的一次则报 `the transfer ... exceeded its 60 second budget`。除网络外还有一处独立限制：`src/update/winhttp.rs` 的 `TOTAL_BUDGET` 固定 60 秒，24.5 MB 的引擎资产在慢链路上即使连通也可能超时。本轮未改该预算，也未做任何网络配置改动。

**离线门与其余验证。** 全部 `EXIT_CODE=0`：`cargo fmt --check`、release all-targets 严格 clippy、debug 与 release 构建、release 全套测试 **386 通过 / 0 失败 / 19 ignored**（与上一轮同数，本轮只改起始实例数与脚本）。`windows_settings.ps1` 21 项、`windows_bangboo.ps1` 34 项、`windows_chat.ps1` 18 项通过；smoke/pixel_hit/wander/desktop/actions 使用普通四实例场景，不受本轮改动影响，未重跑。

标准 release 为本轮构建：`target/release/delicium.exe` SHA256 `C9C283916D8A69E0183C2CB5B2CBB8BF235EAFACC3FDBECBD42D4EA188C9E1AD`（24,671,232 字节），`target/release/delicium-launcher.exe` SHA256 `B21C66F00D6D20368755DCB5C65AB528E06FBD3DCD3253126FAD9D165B095178`（2,092,544 字节）。

口径：第二只由模型库加入，不再有引擎预置的两只场景；README 三语与 `docs/USAGE.md` 已同步改述。更新检查的 60 秒预算与 `github.com` 可达性仅作诊断记录，未修改代码；未自动提交或推送。

### 版本保留与数据清理：单版本策略与实窗验收（2026-10-19）

用户指令：真实使用时必须销毁旧版本，否则占用空间持续增长；旧版本数据迁移后也要删除，包括旧版本配置文件；不复用或不向下兼容时就迁移并删除。本轮按此实施保留策略，并把一条既有缺陷一并修正。

先澄清一个前提：本项目的磁盘结构里**没有每版本一份的配置或数据**。`%APPDATA%\Delicium` 下只有单一的 `config.json`（`version: 1`）与 `models\` 用户模型目录，所有引擎版本共享；引擎不在自身 exe 旁边写任何文件。因此"旧版本配置文件"在这里对应的是"单一配置文件的前向迁移"，不存在多份旧配置需要删除。真正按版本累积的是 `app\<version>\` 引擎目录（每份约 23.5 MiB）与 `backup\delicium-launcher-<unix>.exe` 启动器备份。

**改动。**

- **引擎版本：只保留状态文件点名的版本。** 新增 `Installer::prune_versions(&[String])`（`src/update/install.rs`）：只把 `scan_installed` 认可的目录（存在有效 `.install.json` 且目录名与标记版本一致）列为候选，先重命名进 `app\.removing-<version>-<nanos>` 再删除。重命名先行的意义是：正在运行的 `delicium.exe` 无法被重命名，所以仍在被映射的版本不会被部分擦除，失败只记警告并在下次启动重试。中断留下的 `.removing-*` 由 `clear_trash` 在下次 prune 时收尾。无标记目录（例如测试用的 `9.9.9`）永不被删除。
- **调用点。** 启动时在 `load_state` 之后执行 `clear_staging()`、`prune_launcher_backups(root, 1)` 与 `prune_engines(...)`；保留集合为 `state.current`、`state.pending` 与"即将启动的 target"三者之并。最后一项是必要的：`target_version` 在状态为空时回退到目录扫描，扫到的版本尚未记为 pending，若不计入保留集合，第一次启动就会删掉刚安装的版本。版本确认健康（`confirm_healthy`）与丢弃失败版本（`discard_failed_version`）后各再 prune 一次。
- **启动器备份：只保留最新一份。** 新增 `update::prune_launcher_backups(root, keep)`（`src/launcher/update.rs`），只匹配 `delicium-launcher-<十进制 Unix 时间戳>.exe`，同一目录下的其他文件不动。`LauncherRecord.backup` 至今没有消费者，保留一份只为手工恢复。
- **修正崩溃回退语义。** 旧实现 `discard_and_roll_back` 在待确认版本连续早崩三次时调用 `InstallState::roll_back()`；但此时失败的是 `pending`，`current` 才是原本能用的版本，`roll_back` 却把 `current` 当失败版本丢掉、换成 `previous`，多回退一代。现在改为 `discard_failed_version`：只 `discard_pending()`，`current` 保持不变；目标改由新增的 `update::state_target`（只看 `state.pending` 与 `state.current`）决定，不再走目录扫描——否则扫描会再次找到刚失败的目录并继续启动它。`roll_back` 已删除。
- **状态字段。** `InstallState::confirm` 不再把旧 `current` 写进 `previous`（这正是"每更新一次就多留一份"的来源），`from_installed` 也不再填该槽。字段本身保留在格式里，使旧 `state.json` 仍能解析；启动器读入后若发现 `previous` 非空，会清空并立即原子保存，日志为 `dropping the recorded fallback version; only one version is kept`。
- **配置迁移。** 本轮不加迁移代码：格式只有 v1，`preferences.rs` 与 `app/config.rs` 都按"读时校验版本、未知版本拒绝改写"工作，写入为整文件合并加原子替换。规则固定为：将来出现更低版本时在内存迁移后重写同一文件；出现更高版本时不得改写（现有行为已是如此）。`models\` 是用户内容，不参与任何清理。

**离线门**（`fmt`、release all-targets 严格 clippy、debug 与 release 构建、release 全套测试，全部 `EXIT=0`）：lib **274 通过 / 0 失败 / 19 ignored**（较上轮 273 增加 1 项），`attention` 14、`chat_loopback` 5、`eous_asset` 10、`gpu_render` 8、`model_pipeline` 25、`render_coordinates` 10、`simulation` 31、`textured_model` 13，合计 **390 通过 / 0 失败 / 19 ignored**。新增测试：`pruning_keeps_the_named_versions_and_unknown_directories`（保留集合生效、无标记目录不动、`.removing-*` 收尾、二次调用为空、keep 全集时不删）、`discarding_a_failed_pending_version_keeps_current`（丢弃后 `current` 不变、重复丢弃为空）、`launcher_backups_are_pruned_to_the_newest`（时间戳排序、非匹配文件名不动、keep 0 的边界）、`the_directory_scan_is_only_a_fallback_for_the_state`（状态为空时 `state_target` 为 `None` 而 `target_version` 仍能扫描到）、`pruning_keeps_the_state_target_even_when_the_state_does_not_name_it`（启动时必须保留即将运行的扫描结果）。`confirm_advances_current_previous_and_watermark` 与 `rebuilding_from_disk_orders_and_caps_the_slots` 改为断言 `previous` 为 `None`。

**实窗验收**（`delicium-launcher.exe --settings --no-update`，合成安装根，临时 `APPDATA`）：预置 `app\0.1.0`、`app\0.1.1`、`app\0.1.2`（各有有效标记）、无标记的 `app\9.9.9`、`state.json`（`current=0.1.2`，`previous=0.1.1`，模拟旧版本写下的回退槽）、三份启动器备份（时间戳 100/200/300）与一份 `notes.txt`、一个残留 `staging\leftover`。启动后 16 项断言全部通过：

- `app\0.1.2` 保留，`app\0.1.1` 与 `app\0.1.0` 被删除，`app\9.9.9` 原样保留；
- `staging\leftover` 被清除；
- `backup\delicium-launcher-300.exe` 保留，200 与 100 被删除，`notes.txt` 保留；
- `state.json` 中 `previous` 写回为 `null`；
- 关闭页面后进程以 0 退出；第二次启动幂等，目录只剩 `0.1.2` 与 `9.9.9`，备份只剩最新一份。

证据目录 `.artifacts/retention-20261019-113842-8582/`（`run=...`，安装树与 `state.json` 均保留）。`tests/windows_settings.ps1` 21 项在改动后复跑通过（`.artifacts/settings-page-20261019-113509-5640-.../`），退出码 0。

标准 release 为本轮构建（引擎二进制链接同一 lib，因此哈希随 lib 变化）：`target/release/delicium.exe` SHA256 `D934ED89C4A127159168F49AE68255B7EBDD0DF690C82472C5285F1756194E4C`（24,693,248 字节），`target/release/delicium-launcher.exe` SHA256 `DF62353235CC39A0F6E141C8996EC9512DAF4CD088C82952671E921A3F43FDF6`（2,125,824 字节）。

口径：本轮未在真实安装根上执行删除（本机 `%LOCALAPPDATA%\Delicium` 未安装引擎，只有启动器、日志与备份），所有删除行为都在合成安装根上验证；日志按天分文件且单文件上限 2 MiB × 3 轮转，本轮未改；未改网络与更新判定，未自动提交或推送。
### 更新下载预算放宽与 README 分语言（2026-10-19）

用户指令：先放宽下载预算；README 的多语言改为按语言分文件，不要把所有语言放在同一个 md 里。

**预算。** 旧实现只有一个 `TOTAL_BUDGET = 60s`，同时约束清单（256 KiB 上限）与引擎资产（24.7 MB）。2026-10-19 实测本机到 `release-assets.githubusercontent.com` 的单连接约 50 KB/s、四条并行合计约 72 KB/s、经本机 Clash mixed 端口约 60 KB/s、`codeload.github.com` 约 28 KB/s，24.7 MB 需要约 8 分钟，因此慢链路上的失败与实现无关，但 60 秒预算确实让"链路慢但可用"也必然失败。现在拆为两个常量：清单 `DOCUMENT_BUDGET = 60s`，资产 `ASSET_BUDGET = 30min`；`request()` 显式接收 `budget`，错误消息报告实际生效的那个值。握手、重定向与接收阶段仍是 `WinHttpSetTimeouts` 的 5/5/10/15 秒，因此该预算只放过持续有数据到达的传输，卡死连接仍会在 15 秒接收超时处失败。`MAX_ASSET_BYTES`（256 MiB）、逐文件 size 匹配与 SHA-256 校验不变，失败仍然不安装任何东西。

**README 拆分。** 原 `README.md` 把拉丁语、英语、中文三份正文放在同一个文件里。现按语言拆为三个文件：根 `README.md` 为英文（GitHub 默认展示），`README.zh-CN.md` 为简体中文，`README.la.md` 为拉丁语；三者页首互相链接，正文逐字保留，文档索引表与"测量数字以记录当日为准"的说明在每个文件内各留一份。相对链接（`docs/*.md`、`LICENSE`、`assets/`）不变，因为三个文件都在仓库根。标题层级从 `###` 改为 `##`，因为原先的 `##` 是语言分节，拆分后不再需要那一层。同时把"目前还没有可下载的 Release"更新为实际下载入口，并把状态一节从"一个带 tag 的发布（v0.1.0）"改为两个（v0.1.0、v0.1.1）。

拆分后修正一处既有悬空引用：`assets/examples/bangboo-eous.md` 原写"详见 `README.md` 的朝向修正段落"，但该段落从未存在于任何版本的 README（权威记录在本文件 2026-10-17 的围巾镜像与左走倒走两条），拆分后又会误导到英文版。现改为指向本文件的朝向修正记录。

**验证。** 离线门：`fmt --check`、`clippy --all-targets -D warnings`、debug/release 构建、release 全套测试全部 `EXIT=0`，**390 通过 / 0 失败 / 19 ignored**（预算改动不涉及可测逻辑，未新增测试；既有测试全部通过）。三个 README 保持 CRLF，未引入裸 LF 或 NUL；用脚本解析三份文件的全部相对链接（14/14/11 条）确认可解析、各自恰好一个 H1、无 emoji。预算改动**未**在真实链路上重跑，理由见下。

**未完成。** 真实网络 E2E 仍未跑通：本机到 Release 资产的链路在测速时只有 50–70 KB/s，本轮只放宽了预算，尚未换网络重跑两阶段脚本。因此"新版启动器在空安装根上自动下载 0.1.1 并进入 Up to date"仍没有新的实测证据。v0.1.1 的 tag、CI、Release 与未认证资产下载已核验（版本 0.1.1、引擎 24,724,480 字节 / `c7cce224…b14667`、启动器 2,126,848 字节 / `883bb734…0110a`）。

口径：本轮改了下载预算常量与 README 文件组织，未改清单协议、版本门控、安装事务或保留策略；未在真实用户安装根上执行任何覆盖或删除。

### v0.1.2：真实链路 E2E 通过、启动器自替换与 main 构建（2026-10-19）

**真实链路 E2E 通过（补上一节的"未完成"）。** 换网络后同一 Release 资产实测 2.77–2.85 MB/s（24,724,480 字节 8.9 秒），比原先的 50–70 KB/s 快约 50 倍。用全新空根 `.artifacts/launcher-e2e-20261019/root3` 跑放宽预算后的 `target\release\delicium-launcher.exe`：

- phase 1（空根真实安装）：8 秒完成清单获取、24.7 MB 引擎下载与逐文件 SHA-256/size 校验，装入 `app\0.1.1`，启动引擎并得到 `engine start confirmed: Confirmed`，12 秒后引擎 `code=0` 干净退出；`state.json` 为 `current.version=0.1.1`、`pending=null`。
- phase 2（已装启动器二次启动）：报 `Up to date (0.1.1)`，未安装任何东西，`app\` 下仍只有 `0.1.1` 一个版本目录（单版本保留生效）。
- 合计 36.2 秒，24 条断言全部 `[OK]`，`RESULT: PASS`；跑完无残留 `delicium*` 进程。安装树 5 个文件，`delicium.exe` sha256 `c7cce224107f1c00f22dbb1e50c98cc6678a0230f33785cf2b31f2a70ab14667` 与 Release 清单一致。

**一处测试脚本误报（非产品缺陷）。** 首次复跑（`root2`）报 `exactly one engine version is kept` 失败，但同一行打印的 `engine versions present: 0.1.1` 与安装树都证明只有一份。根因是 PowerShell 的数组解卷：`$versions = if (...) { @(...) } else { @() }` 赋值时单元素数组仍被解卷成 `String`，`$versions[0]` 取到首字符 `'0'`。改为 `$versions = @()` 后另起 `if` 赋值即通过；产品行为本来就是对的。

**v0.1.2 发布准备。** `Cargo.toml`/`Cargo.lock` 升到 `0.1.2`；`release.yml` 的 `MIN_LAUNCHER_VERSION` 由 `0.1.0` 提到 `0.1.2`。理由：下载预算修复只存在于启动器二进制里，而 `src/launcher/update.rs:116` 只有在 `min_launcher_version > 当前启动器版本` 时才替换启动器镜像；若该值不提，已安装的 0.1.1 启动器会一直用旧的 60 秒预算去下 24.7 MB 引擎。提高到 0.1.2 后，旧启动器会先替换自身镜像、下次启动再装引擎——这条自替换路径此前从未被真实发布触发过。

**main 构建。** `release.yml` 增加 `push: branches: [main]` 触发：分支构建跑同样的 fmt/clippy/build/test，把两个 exe 与 `latest.json` 作为工作流产物上传，但不创建 Release；只有 `vX.Y.Z` tag 才发布。版本解析拆成单独一步，分支构建从 `Cargo.toml` 取版本且不声明 tag，tag 构建仍校验 tag 与 `Cargo.toml` 一致、拒绝预发布格式。

**发布核实。** 两次 workflow 均 `completed/success`：`main` push 的 run `39220976113` 的 `Publish` 步骤为 `skipped`，只上传了 11,209,153 字节的 `delicium-0.1.2-b2830a1eefd025b57689ab652171eff4652fe8e7` artifact；tag run `39221001049` 发布了 `v0.1.2`（`draft=false`、`prerelease=false`），三个资产 `delicium-launcher.exe` 2,126,848 B、`delicium.exe` 24,724,480 B、`latest.json` 685 B 均为 `state=uploaded`，GitHub API 报告的 digest 与匿名拉取的清单逐条一致。清单内容为 `version 0.1.2`、`min_launcher_version "0.1.2"`、launcher sha256 `fd3f7bb3bbd9df9e30324fff1b523877ddd48b4b3f20bdb62e7b64719ac646c0`、engine sha256 `32f6508c3b204ad009ad20d81ceaa2baada3c2dd4aff5e2a9a85ea61fba4bfcd`。本地 release 构建的启动器哈希（`d9e80072…`）与 CI 产物不同，属工具链与路径元数据差异，以 CI 产物为准。

**拉取清单时的连通性现象。** 匿名 `curl` 首次失败（`(28) Connection timed out`）。`Resolve-DnsName github.com` 解析到 `20.205.243.166`，该地址连接超时；`140.82.112.3`、`140.82.113.3`、`140.82.114.3` 均返回 `302`；`netsh winhttp show proxy` 显示直连、无环境代理。用 `--resolve github.com:443:140.82.112.3` 后成功。这说明 push "时好时坏"取决于本次命中的 A 记录，不能因为某次直连成功就认为连通性已恢复。

口径：E2E 仍是在开发机上用隔离 `DELICIUM_INSTALL_ROOT` 跑的，不是全新机器；未在真实用户安装根上执行任何覆盖或删除。`min_launcher_version` 自替换路径目前是"已发布、未执行"——`install_launcher` 分支还没有在预置旧启动器的隔离安装根上跑过，需要在连通性可用时单独验证 `launcher.json` 生成与二次启动装新引擎。
