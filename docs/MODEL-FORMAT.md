# Delicium 模型格式规范

> 本文件保存 `delicium.model` 的完整规范，逐字移自 README（2026-10-18）。
> 英文 README 只给摘要，此处为权威文本。

`format` 固定为 `delicium.model`，文件扩展名约定为 `.dmodel.json`。支持严格的 `0.1`（原颜色路径）与 `0.2`（增加内嵌 PNG、UV、基础材质与可选动作绑定），未知版本和字段拒绝；`0.1` 不接受 `0.2` 的新字段。文档完全自包含：不含文件路径、URL、脚本或可执行内容，不引用外部资源。

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

### 动作绑定与播放（`0.2` 可选扩展）

当前构建的 `0.2` 支持顶层显式绑定；旧构建会因未知字段拒绝含此扩展的模型，`0.1` 仍不接受该字段。

```json
"actions": {
  "idle": "idle", "walk": "walk", "reaction": "reaction",
  "greet": "greet", "play": "play"
}
```

声明后 `idle` / `walk` / `reaction` 三项均必填，`greet` / `play` 可选（省略或null表示不绑定）；自动社交要求双方同时具备两个可选槽。所有声明值精确引用当前模型对应类型的 `animations` 名称。缺失、空名、被引用名称重名或未知字段均拒绝；不同动作可复用一个动画。未声明绑定的模型仍按原顺序采样所有动画、后写覆盖，不按名称猜测动作。

- 优先级：活跃点击反应 `reaction` > 成对社交 `greet` / `play` > 本 tick 发生实际自主位移的 `walk` > `idle`。关闭漫游、拖拽悬挂、没有可见横段时不原地播放行走；拖拽不额外定义新动作，也不取消已经触发的点击反应。
- Idle / Walk 使用每实例独立的局部时间并循环；进入新动作从起点播放，初始Idle相位按实例ID分散。Reaction沿用既有约1秒点击窗口，以实秒时间单次播放、钳制在末帧：短片段保持末姿态，长片段超出窗口的尾部不会播放。重复点击会重启反应和既有hop。
- 切换使用0.15秒固定交叉淡入，参数线性混合、局部关节旋转slerp，最后叠加一次当前视线/头部互动。动作不写世界位置；小跳仍只是呈现偏移，渲染、拾取和原生命中共用同一姿态。
- 默认2D贴图示例以身体起伏/摇摆和耳朵反摆表达步态（没有独立腿网格）；3D贴图示例使用现有骨骼的四肢交替摆动。几何拓扑、图片和材质保持不变。原0.1示例与冻结benchmark负载不变；这不是IK或足底无滑步系统。

社交使用双方同步的局部实秒时间并Clamp：问候1.6秒，玩耍2.2秒，结束或中断后冷却12秒。两只角色中心距离0.8至1.6世界单位、Y差不超过0.15、Z差不超过0.2，且均未被拖拽、点击反应或冷却占用时，按距离及实例ID确定性配对；不会一只同时加入两组。社交期间冻结双方自主位移，不改用户拖拽悬挂标志。点击、拖拽、成功移位/换模或移除任何一方都会成对取消；失败请求不破坏旧配对。`--no-wander` 仅关闭位移，仍允许社交。旧无社交槽示例不参加，不改变其随机流。

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

