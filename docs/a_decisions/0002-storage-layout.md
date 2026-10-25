# ADR-0002：存储布局（包名作用域 + 共享游戏资源）

- 状态：已接受
- 日期：2026-10-24
- 决策者：Mr. Fox
- 取代：ADR-0001 中与存储路径相关的部分

## 背景

原设计使用 `~/.config/arkmcl`、`~/.local/share/arkmcl`、`~/.cache/arkmcl`，实例通过 `.minecraft/` 子目录承载游戏数据，组件链使用 `mmc-pack.json`。

新的布局改为：

- 配置、数据、状态、运行时使用包名 `com.purft.arkmcl` 作为 XDG 子目录。
- 游戏资源集中到 `$MINECRAFT_HOME`（默认 `~/.minecraft`），与官方启动器共用。
- 实例根目录**就是**游戏目录，元数据收进 `.arkmcl/`。
- 全局存档、截图、资源包、光影和 mods 通过软链接共享到实例。

## 决策

### 路径根

| 变量 | 默认值 | 内容 |
|---|---|---|
| `ARKMCL_CONFIG_HOME` | `$XDG_CONFIG_HOME/com.purft.arkmcl` | settings.json、accounts.json |
| `ARKMCL_DATA_HOME` | `$XDG_DATA_HOME/com.purft.arkmcl` | 实例、全局存档、默认配置 |
| `MINECRAFT_HOME` | `$HOME/.minecraft` | assets、libraries、versions、natives、全局内容 |
| `ARKMCL_STATE_HOME` | `$XDG_STATE_HOME/com.purft.arkmcl` | 日志、崩溃报告 |
| `ARKMCL_RUNTIME_DIR` | `$XDG_RUNTIME_DIR/com.purft.arkmcl` | socket、pid |

环境变量优先；未设置用 XDG 默认。`XDG_RUNTIME_DIR` 缺失时报错或退化到状态目录并警告，不静默使用 `/tmp`。

### 实例布局

- 实例根 = 游戏目录；不再有 `.minecraft/` 子目录。
- 元数据在 `.arkmcl/`：`instance.cfg`、`pack.json`、`icon.png`、`state.json`、`lock`。
- 分组文件为 `instances/groups.json`。
- 组件链文件为 `pack.json`，不再直接使用 `mmc-pack.json`。

### 全局内容与软链接

- 全局存档、截图、服务器列表、玩家缓存、资源包、光影放在 `ARKMCL_DATA_HOME` 或 `MINECRAFT_HOME`。
- 新实例默认为这些路径建立软链接；`mods/` 由用户选择链接到全局文件。
- `config/` 与 `options.txt` 在创建实例时复制，不做链接。
- 已存在真实文件时不覆盖，标记为未链接，由用户迁移。
- 删除实例只删除链接，不删除目标。
- 备份记录链接关系，不跟随链接复制；恢复时重建链接。

### 便携模式

- 默认禁用。
- 启用方式：可执行文件旁存在 `portable` 文件，或设置 `ARKMCL_PORTABLE=1`。
- 启用后所有 `ARKMCL_*` 根为 `<exe-dir>/.arkmcl`，`MINECRAFT_HOME` 为 `<exe-dir>/.minecraft`；相对路径以可执行文件目录为基准。
- 路径优先级：显式环境变量 > 便携模式 > XDG 默认。
- 标记存在但目录不可写时错误退出，不静默回退。

### 软链接不可用

- 禁用共享，不复制全局内容。
- 新建实例使用本地真实目录。
- 已有链接不自动转换；报告并交给用户决定。
- Windows 等不支持链接的平台因此默认不共享。

## 影响

- `PackService` 需要在 MultiMC 布局与本地布局之间转换：`mmc-pack.json` ↔ `pack.json`，`.minecraft/` ↔ 实例根。
- `FileSystem` 端口新增软链接能力；`LocalFileSystem` 必须限制链接目标在允许根内。
- `XdgPaths` 扩展为解析五个根目录，并负责创建与权限检查。
- 实例删除、备份、导入导出都必须区分“链接”与“真实文件”。
- 与官方 `.minecraft` 共用资源目录会互相可见；ArkMCL 不写官方启动器私有文件，但用户可能看到 ArkMCL 的 `assets/loaders`、`assets/java`、`assets/logging`。
- 软链接在 Windows 需要额外权限；当前 Linux 优先，Windows 后续需提供复制降级并警告。

## 迁移

- 新实例直接按新布局创建。
- 旧的 MultiMC 风格实例导入时，将 `.minecraft/` 内容提升到实例根，`mmc-pack.json` 转为 `pack.json`，并建立全局链接。
- 已有实例若存在真实 `saves/` 等目录，不自动替换；提供“迁移到全局并建立链接”的显式操作。

## 未决

- 全局链接的时间戳与同步冲突处理。
- 便携标记文件的命名是否需要可配置。

## 参考

- 存储指南：`docs/guide/storage/zh-CN.md`
- 设计工作簿：`docs/a_decisions/0-现型.xlsx`
