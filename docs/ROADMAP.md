# Delicium 当前状态与范围外内容

> 逐字移自 README（2026-10-18）；许可证章节随附在此。

## 当前状态与范围外内容

- 2026-10-19 发布 v0.1.2：`Cargo.toml` 升版本，`min_launcher_version` 由 0.1.0 提到 0.1.2，使已安装的 0.1.1 启动器先自替换再装新引擎（预算修复只在启动器二进制里，旧启动器仍是 60 秒预算）；`release.yml` 增加 `main` 分支构建，跑同样的 fmt/clippy/build/test 并把两个 exe 上传为工作流产物，但不创建 Release，只有 `vX.Y.Z` tag 才发布。同日 v0.1.1 的真实链路 E2E 在换网后用全新空根跑通（带宽 2.8 MB/s，24 项断言全过，36.2 秒）。
- 2026-10-19 更新下载预算按用途拆分：清单仍是 60 秒，引擎资产放宽到 30 分钟。原因是本机实测 GitHub Release 资产域名的聚合带宽只有 50–70 KB/s，24.7 MB 引擎需要约 8 分钟，旧的单一 60 秒预算即使链路正常也必然失败。等待数据仍受会话级 15 秒接收超时约束，因此该预算只放过"确实在传输"的慢速下载，不会让卡死连接无限挂起。资产大小上限 `MAX_ASSET_BYTES`（256 MiB）与逐文件 SHA-256 校验不变。离线门 390 通过 / 0 失败 / 19 ignored。同时 README 按语言拆分为 `README.md`（英文）、`README.zh-CN.md`、`README.la.md`，互相在页首链接，不再把所有语言塞进同一个文件。
- 2026-10-19 版本保留与数据清理改为单版本策略：更新确认健康后删除被替换的引擎目录（每份约 23.5 MiB），启动器备份只留最新一份，残留 `staging` 与中断的 `.removing-*` 目录在启动时收尾；无有效 `.install.json` 的目录与 `%APPDATA%\Delicium\models` 用户内容永不删除。同时修正既有缺陷：待确认版本连续早崩时旧代码会多回退一代并丢掉仍可用的版本，现只丢弃 `pending`、`current` 不变，且不再回退到目录扫描。配置只有单一 `version: 1` 文件（共享、合并写入、未知版本拒绝改写），`previous` 槽位保留格式但不再写入并在启动时清除。离线 390 通过 / 0 失败 / 19 ignored，合成安装根 16 项实窗断言全过；未在真实安装根上执行删除。
- 2026-10-19 模型库、设置开关、中英双语与会话框实施并验收：邦布降为模型库条目（内建 5 + 用户目录，`--pet ID|PATH`），`Add 2D/3D` 只加通用示例、邦布追加走 `Eous Add` 复用驻留资产；设置页写入命中区域/漫游/语言/默认角色/对话五字段并轮询热应用（`--pet` 亦生效）；会话按实例存内存历史，远端为 OpenAI 兼容 `/chat/completions`（https 任意、http 仅回环、WinHTTP 系统代理 20 秒、不跟随重定向、key 仅 Bearer 且 Debug 脱敏），未配置时本地台词兜底。离线 386 通过、八套 GUI 与 59 项 bench 自检全绿；会话只在本机回环夹具上验证，未联系任何真实第三方 LLM 服务。
- 2026-10-18 伊埃斯命中区域改走整数扫描线快路径（高密度、严格凸简单多边形适用，其余仍走 chunk-41），与旧路径严格等价、单次原生建区 p50 54.5→8.6 ms；但端到端性能预注册门未通过（提交率 +5.3%，低于 +10%），经用户确认保留接线、剩余性能工作另立范围，见上文。
- 2026-10-16 功能性迭代（点击反应、系统托盘、配置持久化）实施并全门验证通过：普通启动默认 4 实例之外，配置文件接管场景恢复；点击角色转向 + 约 1 秒惊吓小跳（渲染偏移，不改位置，期间暂停游走）；托盘左键切换控制窗、右键菜单含 Quit、关闭控制窗隐藏到托盘；139 项非 GPU + 8 项 GPU 测试、五套 GUI 脚本（60/44/62/51 + bench 59 检查）与 67 项托盘/配置端到端验收全过，见上文。跳过开机自启。
- 2026-10-16 任务栏唤出缺口修复并验收：场景窗口按任务栏停靠边留 2 物理像素缺口，自动隐藏任务栏的驻留唤出恢复正常（修复前被 shell 判全屏而抑制）；全门 0（lib 85+2 ignored）、GUI 53/60/44/62 + bench 59、实机驻留复验任务栏滑出与无应用基线一致，见上文。
- 2026-10-13 本地轮廓验收已通过：fmt、离线 locked clippy、49 项单元/集成测试、3 项 GPU 回归测试、构建，以及两套真实 Windows GUI 脚本。轮廓覆盖测试在硬件 GPU 上检查了 25 个同帧动画样本（8 次缓存复用、17 次重建），另验证视口边界重建和 3 个缓存漂移样本的全部可见像素。`windows_smoke.ps1` 保留增删、拖拽、加载、重载与失败保留覆盖；`windows_pixel_hit.ps1` 验证空白/耳间/腿间物理点击穿透、默认模式拖拽与释放恢复、窗口层级及模式切换。
- 2026-10-14 性能/资源基线与平衡原生区域合并优化的完整 10 组对照均通过；64 实例 pixel 提交率由约 9.7/s 提升至 15.3/s，持续模拟丢步由约 35.39% 降至 0.49%，高实例数仍有性能限制，结果见上文。
- 2026-10-15 漫游开启对照完成：同一构建关闭/开启两轮各 10 组通过（`--bench-wander`/`-Wander`，run plan 与终态 config 双层溯源）；整窗吞吐不变，轮廓路径 16/32/64 提交率下降约 8%-18%，64 实例持续丢弃模拟步约 0.3%→15.9%；资源侧无差异，4 实例复用未塌缩；结果见上文。
- 2026-10-15 区域安装节流（K 帧节奏）实施并全门验证通过：64 实例开启态提交率 13.13→31.86/s、持续 300s 丢步占应执行步 15.93%→2.06%；K=1 档行为不变；`ScriptVersion` 4、`-ValidateOnly` 59 项、114 项非 GPU + 8 项 GPU 测试、四套 GUI（44/52/62/51）全过，见上文。当轮残余丢步来自安装帧 p95/p99 与调度抖动，已由下一日的分块批量安装消除（10+10 组丢步清零）。
- 2026-10-15 安装路径批量化按 Phase 0 决策门**否决并停止**：整帧单次 `CreatePolyPolygonRgn` 比现有平衡合并慢 11.5x（n=10512）且超线性；探索发现「按实例分批（每批约 164 多边形）+ 位宽归并」约 0.60x 生产序基线，为待预注册的候选方向。生产路径零改动，探针保留为 `#[ignore]` 复现装置，见上文。
- 2026-10-16 分块批量安装（chunk=41）重新预注册决策门采纳并实施：真实几何微基准配对中位数 0.332（chunk-82/164/328 = 0.370/0.473/0.694，per-instance 0.599 未胜出）；生产切换后 c64 安装 p50 54.7→17.7 ms（-68%）、提交 32.0→47.5/s、丢步 38→0，持续 300s 384→0，16/32 档与小档同受益、interactive 零漂移；118 项非 GPU（lib 67+2 ignored）+ 8 项 GPU、四套 GUI（44/52/62/51）全过，见上文。
- 2026-10-14 桌面范围（普通启动铺满主显示器物理矩形、控制窗右上锚点、拖拽越旧有界边缘）实施完成并全门验证通过：93 项核心/集成、8 项 GPU、三套 GUI（51/52/44）与 bench 自检 55 项，见上文。当轮仅覆盖主显示器；现普通启动已改为所有显示器物理包围盒（见启动说明）。拖过控制窗与本轮跨度下的性能基线重测仍不作承诺。
- 无 GLB 或其他外部格式导入。
- 自主游走只覆盖水平方向；显式绑定模型的行走动作由实际自主位移触发。竖直游走、与位移距离精确锁相的步态及避障不在范围内。
- 联网只有用户自行配置的可选对话端点（OpenAI 兼容 `chat/completions`，未配置时走不联网的本地台词）；无商店、账号、支付、SQLite，没有遥测或自动上传。
- 模型库只做内建清单加 `%APPDATA%\Delicium\models` 用户目录（不递归、最多 200 条、无元数据/缩略图/导入向导）；界面语言只有简体中文与 English；会话历史只在内存中，不落盘、不跨进程共享。
- 无可视化编辑器。
- 无 IK、物理、morph；基础 GGX/Schlick 材质尚无 IBL、阴影、法线贴图、mipmap/各向异性，不是最终 PBR 管线。
- 遮罩是静态本地多边形，当前不随变形器变形。
- 非 Windows 平台不在第一阶段承诺内（Windows 首发）。
- 原验证示例为自制资产；伊埃斯示例改编自 OSad 的作品，来源、许可声明与改动见 `assets/examples/bangboo-eous.md`，不代表最终渲染质量。

## 许可

除另有说明外，本仓库的源代码、示例模型与文档以 MIT 许可发布，见 [LICENSE](../LICENSE)。MIT 只覆盖本仓库作者的贡献，不覆盖第三方素材：

| 素材 | 许可 | 说明 |
|---|---|---|
| 仓库源代码、文档、`puppet2d*.dmodel.json`、`robot3d*.dmodel.json`、`bangboo.dmodel.json` | MIT | 自制验证资产与工具 |
| `assets/examples/bangboo-eous.dmodel.json` | CC BY 4.0（原作） | 改编自 OSad 的 Bangboo 01 - Eous，授权链与改动见 [`assets/examples/bangboo-eous.md`](../assets/examples/bangboo-eous.md) |

- Eous 及其所属作品的角色与商标权利不随上述模型许可转移，本项目与权利方无关联。
- 若注释、文件头或子目录另有许可声明，以该声明为准。

MIT 按现状提供，不附带担保；这是原型代码，不承诺生产可用性。

## License

Unless otherwise stated, the source code, example models and documentation in this
repository are released under the MIT License; see [LICENSE](../LICENSE). MIT covers only the
contributions of this repository's authors, not third-party material:

- `assets/examples/bangboo-eous.dmodel.json` is adapted from OSad's Bangboo 01 - Eous,
  whose original page declares CC BY 4.0; see `assets/examples/bangboo-eous.md` for
  attribution and the list of modifications.
- Character and trademark rights in Eous and its franchise do not transfer with that model
  licence. This project is not affiliated with the rights holders.
- Where a file header, comment or subdirectory states another licence, that statement governs.

The software is provided "as is", without warranty of any kind.
