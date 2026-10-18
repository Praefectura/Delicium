# 伊埃斯模型来源与离线制作

<!--
Eous / Bangboo 01 - Eous, adapted from OSad's model.
Source: https://sketchfab.com/3d-models/bangboo-01-eous-a3dbb1c10b4c4f1b97a47fa798239af9
Model licence declared on the source page: CC BY 4.0
Changes: mesh merge and split by normal/UV seams, 22-bone hierarchy retained,
2048px texture embedded, head/face weights adapted, four-slot weight refit,
idle/walk/reaction/greet/play clips authored or rebuilt (see below).
Character and trademark rights are not granted by that model licence.
-->

`bangboo-eous.dmodel.json` 是 OSad 的 **Bangboo 01 - Eous** 的本地适配版。

- 作者：OSad
- 原作页面：https://sketchfab.com/3d-models/bangboo-01-eous-a3dbb1c10b4c4f1b97a47fa798239af9
- 原作页面声明的模型许可：Creative Commons Attribution 4.0 International
- 许可全文：https://creativecommons.org/licenses/by/4.0/
- 角色：伊埃斯（Eous），出自《绝区零》。这是创作者上传的角色模型，不是官方提供的模型；上述模型许可不表示角色权利人的官方授权。

## 本地适配内容

- 从用户提供的 FBX 中保留角色的16个网格及22骨层级，排除展示凳子及其贴图。
- 合并网格，按法线和 UV 接缝拆点；成品11331顶点、18224三角。
- 转换为右手 Y 向上、+Z 正面，站立高度1.65世界单位；重设骨骼静止坐标轴时保持骨蒙皮矩阵等价，使局部 Y 轴视线驱动正确偏航。随后另做下述头面权重适配，不宣称最终逐顶点形变与原作无损等价。
- 原4096像素角色贴图缩为2048像素并内嵌，UV 转为左上原点；保留原配色、围巾、缝线和背部配件。使用现有不透明基础材质，不增加自发光或法线贴图渲染。
- 复用原待机骨架动画（v20 起为 59/60 秒的完整循环，见下文），修正脚底下探与尾部静止帧；额外制作行走、点击反应、挥手、玩耍四段动作。头面蒙皮适配会有意改变这些轨迹下的头面位置。
- 行走为0.8秒前后小碎步：髋绕X轴以-22度为基线、左右错开半周期摆动正负13度；膝以44度为基线，在摆动阶段平滑再屈18度。入口相位前移四分之一周期，让起步从髋部中立摆幅进入，兼顾待机的屈膝姿态与点击反应结束后的再起步。根骨保持Y轴25度朝向；相机仍是正交，不模拟近大远小。两个行走方向不再靠整实例X镜像：镜像会连带翻转围巾上的“01”，因此左走改为对根骨叠加额外Y旋转（关于正面轴把作者朝向反射成对称角度），详见 `README.md` 的朝向修正段落。
- 行走按实际蒙皮最低点校准公共根骨高度，关键帧目标余量0.0027世界单位，不用整身跳动替代迈步。源模型两脚踝骨没有顶点权重，踝轨道虽保留髋/膝角度补偿，可见抬脚来自膝部蒙皮；检查使用真实脚底顶点，不以脚踝骨原点代替。
- 原作眼睛平均仅约11%权重在head，直接叠加单head大角度视线驱动会拉扯头罩与眼环。按UV/法线拆点后的拓扑岛区分面板、外壳、耳部，再映射回焊接源顶点统一适配权重，避免UV缝裂；不是按高度裁头。眼环、面板和头罩主体统一跟随head，头罩下方127个源顶点以两环拓扑距离向躯干渐变。保留耳链与耳部原完整权重、围巾及其他网格，不修改全局60度转头上限。body共1052个、eyes共480个源顶点的权重有意改变。
- 在适配后的完整权重上，将288个五权重顶点离线拟合为四槽。v18的2406个采样姿态（含0、正负30及60度偏航）中，最大位置差0.00731596世界单位、全顶点平均差0.00000709，低于原0.0075/0.0005预算；不是全姿态无损保证。后续v19没有改权重，其改变的姿态另做下述预算检查。
- 两类差异分开记录：原完整权重到有意头面适配的最大采样位置改变约0.435232世界单位；上述0.00731596是适配后完整权重到四槽的近似误差，不能用来宣称未改变原作形变。转换报告保留选区、过渡顶点及两类统计。
- 报告中的简单 top4 基线只采原待机及重建 clip 前的自制动作，不含叠加 head 驱动；不能与最终2406姿态误差直接作同域改善比较。转换器现另输出 `naive_top4_pose_samples` 和 `naive_top4_sample_scope` 标明这一口径；已留存的 v9 原始报告不追改，最终预算判定仍取包含偏航的全部采样。

原始 FBX 和4K贴图不随此说明复制进项目。旧自制九骨资产 `bangboo.dmodel.json` 及其生成器保留，不被此资产覆盖。

## v19：受惊动作的接地衔接

2026-10-18，持续待机与连续点击回归发现：v18各片段分别接地，并不保证 Reaction→Idle 的局部关节混合始终接地。v19仅校准 Reaction 前后各0.15秒的 hips、双侧 Leg/Knee/Foot 轨道，中间手势、五动作绑定、其它四片段以及全部网格/权重/UV/贴图/骨架逐值保留。原运行时混合和0.15秒过渡不变，不增加逐帧网格扫描或全局常数抬升。

- v18原文件保留在本地工件 `source-v18.dmodel.json`，SHA256 `cd5fa0bb820fd5ca6b165a3bbf58cc737ba717c716737ecc2ed4c752f38bf299`。
- v19成品6897169字节，SHA256 `bf094a3f33853b3e1828edfeea2f6c7a5559a6a1a768575e63196ac9aec762b2`。
- 首尾髋腿姿态桥接至待机入口；只对改变的离线关键帧，依据实际蒙皮最低点补足0.002世界单位净空。最大额外根轨道校正约0.003094；不是统一抬高整个模型。
- 从原FBX重建适配后的完整权重，与成品11331导出点、54672索引、UV及22骨静止姿态对齐；9592个源顶点全部有对应点。以去除UV/corner重复后的源顶点域计算均值，不重新拟合或更改四槽权重。
- 新Reaction的121个120Hz样本及57个离线恢复探针，各叠加0、正负30和60度head偏航，共890姿态：最大局部位置误差0.00674827、全源顶点平均0.00000901，通过原0.0075/0.0005预算。该统计是新姿态的补充检查，不与旧2406样本混成一个均值。
- 真实Simulation持续待机、两种半步显示采样、不同点击间隔及恢复中再次点击，共9600帧，最低脚底+0.00172514，原地板-0.0005未放宽。离线恢复探针不是运行时f32时钟的精确重放。

校准工具：`tools/recalibrate_eous_reaction.py`；完整权重、对齐与校准报告保留于 `.artifacts/eous-grounded-reaction-20261018/`。这只是资产与衔接验证，不代替整个互动版本的GPU、性能和实窗验收。

## v20：待机循环不再从静止帧起跳

2026-10-18 的实机画面反馈：邦布原地左右摇晃时，摆到最左边有明显的姿态 reset 感。根因在源 FBX 与转换器，不在运行时。

- 源动画 `Armature|Take 001|BaseLayer` 的 hips 有 61 个关键帧（帧1..61），其余骨只有 60 个（帧2..61）。帧1 是导出器的绑定姿势，其 hips 局部变换等于静止绑定（误差 5.6e-8），不是动画帧。
- 转换器把帧1 当成 t=0 采样，于是每个循环里都塞进一个静止姿势；同时又对尾部 55..61 号关键帧做了一段向该静止姿势混合的“缝合”处理。两者叠加，使每轮循环末尾趋向静止，然后一跳回到摆动，最大单帧根旋转跳变 0.0759 rad（约 4.35 度）。
- 实测源帧61 与帧2 在全部 22 根骨上逐位相同，因此真正的循环是帧2..61，共 59 个间隔 = 59/60 秒。

修正内容：

- 待机片段改为 60 个关键帧、周期 0.9833333 秒，首尾重合，不再包含绑定帧；尾部缝合混合整段删除，不改任何源动画轨迹。
- 转换器保持源时间轴仍为 60 Hz 采样，运行时固定步进与 0.15 秒过渡不变；不新增逐帧处理。
- 四槽权重拟合集因此少了被删除的 5 个尾部关键帧（2406 → 2401 姿态）。拟合是极小极大问题且有退化最优解，41 个髋/腿根带顶点（模型高度 0.216..0.403 处）落到另一组等价解上。用同一批姿态对比两套权重，最大形变差 0.00167 世界单位，远低于 0.0075 预算。
- Reaction 在新的待机入口上重新接地（入口键 0，即新片段的 t=0）。walk、greet、play 三段与网格、索引、UV、骨架、材质、贴图逐值不变。

验收数据：

- 引擎级 1400 帧实测：root 单帧位移峰值 0.007471 → 0.000604，接缝处的方向反转完全消失（接缝邻域从 +0.000182 平滑单调过渡到 -0.000151）。
- 新增回归 `tests/eous_asset.rs::eous_looping_clips_never_snap_back_through_a_rest_pose`：以片段自身中位步长为基准，要求最大单帧 root 旋转步长不超过 4 倍。旧资产为 12.5 倍（失败），新资产通过。
- Reaction 重接地：120 Hz 地板余量 +0.00181，离线恢复余量 +0.00124，均高于 -0.0003 阈值；新姿态拟合最大 0.00613、平均 0.0000091，通过 0.0075/0.0005 预算。
- 离线门：fmt、`clippy --all-targets -D warnings`、debug/release 构建、release 全套测试 276 通过 / 0 失败 / 13 忽略。

成品 6893849 字节，SHA256 `d4dbd78aa4a3e07617ebf615df451f1bd5edba0de237a374511464ef19b3edc8`；v19 保留在 `.artifacts/eous-idle-loop-20261018/shipped-v19-before-fix.dmodel.json`（`bf094a3f…`）。这是画面与循环连续性修正，不代替双屏实机与用户观感验收。

## 再制作

使用本机已有 Blender 5.2.2 的独立后台进程，禁用自动脚本。自写转换器与下载包分目录；不从输入目录启动解释器，不执行包内插件。

```powershell
& 'C:\Program Files\Blender Foundation\Blender\blender.exe' `
  --background --factory-startup --disable-autoexec --python-exit-code 9 `
  --python "$PWD/tools/convert_eous.py" -- `
  --fbx '<隔离输入目录>/source/bangbooExport.fbx' `
  --textures '<隔离输入目录>/textures' `
  --output '<新输出目录>/bangboo-eous.dmodel.json' `
  --atlas '<新输出目录>/eous-atlas.png' `
  --report '<新输出目录>/conversion-report.json'
```

输出目录必须事先存在，三个输出文件必须不存在。转换器拒绝未知骨架、未知材质连接、超限、非法 UV/权重和超过既定误差预算的拟合；不会覆盖输入或已有输出。无新增 Rust 或 Python 安装依赖，使用 Blender 自带 bpy、mathutils 和 NumPy。

上面的转换器输出v18基础资产；v19随后使用clip-only后处理，不是重新导出网格。用工件中的 `scripts/export_full_weights.py` 在同样的独立Blender安全参数下，传入 `--fbx`、基础 `--model`、可信 `--converter`、新的 `--output` npz与 `--report`，得到适配完整权重缓存；再运行：

```powershell
python -I tools/recalibrate_eous_reaction.py `
  --input '<基础输出>/bangboo-eous.dmodel.json' `
  --output '<新输出>/bangboo-eous-v19.dmodel.json' `
  --report '<新输出>/reaction-v19-report.json' `
  --weights '<完整权重缓存>/full-weights-v18.npz' `
  --entry-key 0
```

后处理使用已有NumPy，要求输入、成品、报告路径互不冲突且两个输出均不存在。`--weights`用于实际验证新姿态的完整/四槽误差；不传该参数只会得到未完成拟合验收的候选，不能视为已经通过该预算。

`--entry-key` 指向待机片段里反应结束、恢复播放时应回落的姿态键。v18/v19 用 1，因为那时的键 0 是绑定帧；v20 起待机片段从真正的动画首帧开始，键 0 就是回落目标。用错值会把反应尾巴接到一个偏离的姿态上，而离线预算仍会通过，所以必须与基础资产的待机片段一致。权重缓存也必须针对新基础资产重建（`export_full_weights.py` 的 `--model` 传新基础输出），否则 `validate_fit` 的逐位权重校验会失败。
