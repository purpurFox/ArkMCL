# 命名规则：词频正序 + 语义守卫

- 状态：生效
- 生效日期：2026-10-23
- 适用范围：导出的类型名（Rust `struct`/`trait`/`enum`、Dart `class`）
- 不适用：方法、变量、参数、局部名、模块名、crate 名

## 规则

1. 将复合类型名拆成词（CamelCase 边界；`NeoForge` 视为 `Neo` + `Forge`）。
2. 查下方冻结词频表（小写；未收录的词按 0 计）。
3. 按词频升序排列；词频相同按字母升序。
4. **语义守卫**：重排后如果中心词或修饰关系改变，保留原名，不强行重排。
5. 词频表是快照，不按实时仓库重算；调整必须走新的 ADR。

## 判定示例

| 候选 | 结果 | 理由 |
|---|---|---|
| `MinecraftComponent` → `ComponentMinecraft` | 允许 | `Component` 是类别标签，指称不变 |
| `FileSystem` → `SystemFile` | 拒绝 | “文件系统”变成“系统文件” |
| `InstanceRepository` → `RepositoryInstance` | 拒绝 | “实例仓储”变成“仓储的实例” |
| `LaunchPlan` → `PlanLaunch` | 拒绝 | 名词“启动计划”变成动词短语 |
| `LaunchViewModel` → `ModelLaunchView` | 拒绝 | 中心词和修饰关系都改变 |

## 2026-10-24 全量复核

复核范围：当前 60 个导出的 Rust 复合类型与 2 个 Dart 类。结论：无需改名；下列名称按词频会重排，但均触发语义守卫。

| 现名 | 词频顺序 | 候选 | 决定与理由 |
|---|---|---|---|
| `ConfigError` | config 3 → error 0 | `ErrorConfig` | 保留：中心词是 error，“配置错误”不能变成“错误的配置” |
| `DeviceCode` | device/code 均 0，字母序要求 code 在前 | `CodeDevice` | 保留：OAuth“设备码”是固定术语，重排后中心词改变 |
| `FileSystem` | file 34 → system 2 | `SystemFile` | 保留：规则示例已说明 |
| `IdError` | id 39 → error 0 | `ErrorId` | 保留：中心词是 error |
| `JsonInstanceRepository` | json 77 → instance 3 → repository 0 | `RepositoryInstanceJson` | 保留：`InstanceRepository` 重排已被拒绝，`Json` 前置同样改变修饰关系 |
| `LibraryDownloads` | library 18 → download 1 | `DownloadsLibrary` | 保留：“库的下载”不能变成“下载库” |
| `LocalFileSystem` | local 4 → file 34 → system 2 | `SystemLocalFile` | 保留：`file system` 是复合中心词 |
| `RuntimeInfo` | runtime 10 → info 0 | `InfoRuntime` | 保留：“运行时信息”不能变成“信息运行时” |

其余多词类型按词频已有序：`AccountId`、`ArgumentValue`、`AssetIndex`、`AssetObject`、`JavaVersion`、`NewInstance`、`ArkmclApp`、`HomePage`；单词类型不适用排序。

## 冻结词频快照（2026-10-23）

语料：仓库文本文件（`.rs`、`.dart`、`.md`、`.toml`、`.yaml`、`.json`、`.cc`、`.h`），
排除 `.git`、`target`、`build`、`.dart_tool`、`.idea` 与二进制设计文件。

| 词 | 次数 | 词 | 次数 | 词 | 次数 |
|---|---:|---|---:|---|---:|
| account | 1 | component | 1 | dependency | 1 |
| download | 1 | fabric | 1 | keyring | 1 |
| launch | 1 | meta | 1 | mirror | 1 |
| offline | 1 | provisioner | 1 | quilt | 1 |
| minecraft | 2 | process | 2 | resolver | 2 |
| state | 2 | system | 2 | world | 2 |
| xdg | 2 | bus | 3 | config | 3 |
| instance | 3 | rpc | 3 | runner | 3 |
| local | 4 | neo | 4 | http | 5 |
| mod | 6 | profile | 6 | forge | 8 |
| java | 8 | api | 10 | runtime | 10 |
| service | 10 | index | 12 | library | 18 |
| policy | 20 | app | 21 | source | 27 |
| store | 31 | file | 34 | version | 36 |
| id | 39 | cli | 52 | event | 53 |
| set | 74 | json | 77 | view | 82 |
| list | 118 | flutter | 452 | | |

未收录的词按 0 计。因此包含零频词的名称在排序时会把这些词排在最前；是否采用仍由语义守卫决定。

## 已批准的变更

| 原名 | 新名 | 日期 |
|---|---|---|
| `MinecraftComponent` | `ComponentMinecraft` | 2026-10-23 |
| `ForgeComponent` | `ComponentForge` | 2026-10-23 |
| `FabricComponent` | `ComponentFabric` | 2026-10-23 |
| `QuiltComponent` | `ComponentQuilt` | 2026-10-23 |
| `NeoForgeComponent` | `ComponentNeoForge` | 2026-10-23 |
说明：当前实现使用单一 `Component { uid, version }`；表中的专用组件类型在现有代码中不存在，保留为历史记录。


## 变更流程

- 新增类型名先按规则生成候选，再通过语义守卫；两者冲突时以语义为准。
- 批量重命名必须提供旧名→新名映射，并全量搜索引用。
- 不得为了追求词频顺序而修改方法、变量或模块名。
- 修改本快照需要新的决策记录，并在 ADR 中说明理由。
