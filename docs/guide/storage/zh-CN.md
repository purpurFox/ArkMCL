# 存储布局

- 状态：生效
- 日期：2026-10-24
- 对应决策：`docs/a_decisions/0002-storage-layout.md`

## 路径变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `$ARKMCL_CONFIG_HOME` | `$XDG_CONFIG_HOME/com.purft.arkmcl/`（通常 `~/.config/com.purft.arkmcl/`） | 可手工编辑，不含令牌 |
| `$ARKMCL_DATA_HOME` | `$XDG_DATA_HOME/com.purft.arkmcl/`（通常 `~/.local/share/com.purft.arkmcl/`） | 实例与用户数据，备份对象 |
| `$MINECRAFT_HOME` | `$HOME/.minecraft` | 游戏资源与全局内容，默认与官方启动器共用 |
| `$ARKMCL_STATE_HOME` | `$XDG_STATE_HOME/com.purft.arkmcl/`（通常 `~/.local/state/com.purft.arkmcl/`） | 日志与崩溃报告，可删除 |
| `$ARKMCL_RUNTIME_DIR` | `$XDG_RUNTIME_DIR/com.purft.arkmcl/`（通常 `/run/user/$UID/com.purft.arkmcl/`） | socket 与 pid，重启即清 |

环境变量优先；未设置时按上表默认值；便携模式见下一节。`$XDG_RUNTIME_DIR` 缺失时不静默改用 `/tmp`，应报错或退化到状态目录并警告。

## 便携模式

便携模式默认禁用。启用方式（任一）：

- 可执行文件旁存在名为 `portable` 的文件；
- 环境变量 `ARKMCL_PORTABLE=1`。

启用后，所有路径相对于**可执行文件所在目录**（不是当前工作目录）：

```text
<exe-dir>/
├── arkmcl
├── .arkmcl/                  # 所有 ARKMCL_* 根都指向这里
│   ├── settings.json
│   ├── accounts.json
│   ├── instances/
│   ├── saves/
│   ├── screenshots/
│   ├── servers.dat
│   ├── usercache.json
│   ├── usernamecache.json
│   ├── realms_persistence.json
│   ├── config/
│   ├── options.txt
│   ├── logs/
│   ├── crashes/
│   ├── arkmcl.sock
│   └── arkmcl.pid
└── .minecraft/
    ├── assets/
    ├── libraries/
    ├── versions/
    ├── natives/
    ├── cache/
    ├── resourcepacks/
    ├── server-resource-packs/
    ├── shaderpacks/
    ├── saves/
    ├── datapacks/
    ├── modpacks/
    ├── plugins/
    └── mods/
```

- `ARKMCL_CONFIG_HOME`、`ARKMCL_DATA_HOME`、`ARKMCL_STATE_HOME`、`ARKMCL_RUNTIME_DIR` 全部为 `<exe-dir>/.arkmcl`。
- `MINECRAFT_HOME` 为 `<exe-dir>/.minecraft`。
- 路径优先级：显式环境变量 > 便携模式 > XDG 默认。
- 标记存在但目录不可写时直接报错，不静默回退到 XDG。
- 便携模式不改变目录内部结构，只改变根位置。

## 配置

```text
$ARKMCL_CONFIG_HOME/
├── settings.json      # 语言、代理、下载源、并发、默认内存
├── accounts.json      # 账户索引：id、类型、档案名（无令牌）
└── tokens.json        # 刷新令牌与访问令牌；权限 0600
```

## 用户数据

```text
$ARKMCL_DATA_HOME/
├── instances/
│   ├── groups.json                 # 实例分组索引（可手工编辑）
│   └── <instance-id>/              # 实例根即游戏目录
│       ├── .arkmcl/
│       │   ├── instance.cfg        # 实例配置；含 type、lwjgl、created_at/last_launched_at/run_seconds
│       │   ├── pack.json           # 组件链
│       │   ├── icon.png            # 可选，用户提供
│       │   ├── state.json          # 状态枚举：notinstalled/ready/running/corrupted
│       │   └── lock                # 运行中互斥锁
│       ├── resourcepacks/          # 默认软链接到全局
│       ├── server-resource-packs/  # 默认软链接到全局
│       ├── shaderpacks/            # 默认软链接到全局
│       ├── saves/                  # 默认软链接到全局
│       ├── screenshots/            # 默认软链接到全局
│       ├── servers.dat             # 默认软链接到全局
│       ├── usercache.json          # 默认软链接到全局
│       ├── usernamecache.json      # 默认软链接到全局
│       ├── realms_persistence.json # 默认软链接到全局
│       ├── mods/                   # 按用户选择链接到全局文件
│       ├── logs/
│       └── crash-reports/
├── saves/                          # 全局存档（用户数据）
├── screenshots/                    # 全局截图
├── servers.dat                     # 全局服务器列表
├── usercache.json                  # 全局玩家档案缓存
├── usernamecache.json              # 全局玩家档案缓存
├── realms_persistence.json         # 全局 Realms 客户端状态
├── eula.txt                        # 服务端全局默认（链接目标）
├── ops.json
├── whitelist.json
├── banned-players.json
├── banned-ips.json
├── server.properties               # 以下复制到新服务端实例
├── bukkit.yml
├── spigot.yml
├── purpur.yml
├── config-server/                  # 复制为实例 config/
├── config/                         # 默认 mod 配置，创建实例时复制
└── options.txt                     # 默认游戏选项，创建实例时复制
```

## 游戏资源

```text
$MINECRAFT_HOME/                    # 默认 ~/.minecraft
├── assets/
│   ├── indexes/                    # 所有 ArkMCL 元数据
│   │   ├── <index-id>.json
│   │   ├── version_manifest_v2.json
│   │   ├── versions/<id>/<id>.json
│   │   └── loaders/fabric/<game>-<loader>.json
│   ├── objects/<xx>/<sha1>
│   └── virtual/legacy/             # 仅 virtual 资源版本
├── libraries/<maven-path>
├── versions/<id>/
│   ├── <id>.json                   # 安装器产物；缓存读写见 assets/indexes/versions/
│   └── <id>.jar
├── natives/<version>/<platform>/
├── bin/                            # 1.6 之前
├── resources/                      # 更老版本
├── cache/                          # installer 与 paperclip 缓存、安装日志
├── resourcepacks/                  # 全局资源包
├── server-resource-packs/          # 全局服务器资源包
├── shaderpacks/                    # 全局光影
├── saves/                          # 世界下载目标（非用户存档）
├── datapacks/                      # 数据包下载目标
├── modpacks/                       # 整合包下载目标（仅全局）
├── plugins/                        # 服务端插件下载目标
└── mods/                           # 全局 mods
```

所有 ArkMCL 管理的元数据都在 `assets/indexes/` 下；`assets/java`、`assets/logging` 保留为辅助目录，官方启动器会忽略这些未知目录。安装器写入的 `versions/<id>/<id>.json` 仍会被读取作为回退。

### 元数据缓存与刷新

| 内容 | 路径 | 刷新策略 |
|---|---|---|
| 版本清单 | `assets/indexes/version_manifest_v2.json` | TTL 24 小时；过期后条件请求 |
| Fabric loader 列表 | `assets/indexes/loaders/fabric/<game>-loaders.json` | TTL 24 小时；过期后条件请求 |
| 版本 JSON | `assets/indexes/versions/<id>/<id>.json` | 不可变，永久缓存 |
| 资源索引 | `assets/indexes/<id>.json` | 不可变，永久缓存 |
| Fabric profile | `assets/indexes/loaders/fabric/<game>-<loader>.json` | 不可变，永久缓存 |

- 每个缓存文件旁有 `<文件>.meta.json`，记录 `etag`、`last_modified` 与 `fetched_at`。
- 过期后用 `If-None-Match` / `If-Modified-Since` 校验；`304` 只更新时间戳。
- 网络失败但存在旧缓存时回退旧内容，并在 `launch.prepare` 结果中返回 `"stale": true`。
- `ARKMCL_META_TTL` 可覆盖 TTL（秒；`0` 表示永不过期）。
- 升级兼容：旧版 `assets/version_manifest_v2.json` 首次读取时复制到新位置；旧的 `assets/loaders/fabric/` 不再读取，按需重新抓取。

### 下载行为

- 默认 4 个并发，范围 `1..=8`；`ARKMCL_DOWNLOAD_CONCURRENCY` 可覆盖。
- 临时文件为 `<目标文件>.part`；中断后按 `Range: bytes=<已下载长度>-` 续传，服务器不支持时自动从头重下。
- 失败按 250ms 起倍增退避重试，最多 4 次；`429`/`5xx`、连接重置与超时可重试，`404`/`403` 不重试。
- 校验失败或超过大小上限时删除 `.part` 后重试。
- 进度通过 JSON-RPC 通知（`method = "progress"`）推送；CLI 在 TTY 上显示 pacman 风格进度，非 TTY 或 `--json` 时只在任务完成时输出一行。
- 社区资源（`resource.download`）对同名文件采用覆盖语义；游戏内容采用“`SHA-1` 匹配则跳过”。

## 状态与运行时

```text
$ARKMCL_STATE_HOME/
└── run/        # 仅当 XDG_RUNTIME_DIR 缺失时的 socket 回退

$ARKMCL_RUNTIME_DIR/
├── arkmcl.sock
└── arkmcl.pid
```

## 实例默认值

- ID / 名称：未指定时为 `<loader>-<version>`（原版为 `vanilla-<version>`），冲突追加 `-2`、`-3`
- 版本：未指定时为最新稳定版
- 内存：默认 4096 MiB；client ≥ 512，server ≥ 256
- JVM 参数：默认 Aikar's flags；`--jvm-args default|none|"<args>"`
- 分组：默认 `<loader>`
- `--create-defaults <bool>` 控制是否复制默认配置（默认 true）
- `--move <to>` 修改实例 ID（即目录名）

全局默认项保存在 `settings.json` 的 `instance` 字段，缺省时使用内置值（4096 MiB、Aikar's flags），已有值不会被替换。

不带实例选择器时：`--version|--memory|--jvm-args|--group <值>` 修改全局默认；这些选项不带值则输出当前全局值。带选择器时，上述选项以及 `--name`、`--loader`、`--loader-version`、`--lwjgl`、`--type`、`--account`、`--group` 不带值会输出该实例的当前设置；其中 `loader`、`loader-version`、`lwjgl`、`account`、`type`、`name` 不能作为全局默认项。

## 实例软链接策略

`arkmcl instance <id> --type <client|server>` 决定可用链接项；`--linking` 只接受该类型支持的名称。

### client

| 名称 | 实例路径 | 目标 |
|---|---|---|
| `usercache` | `usercache.json`、`usernamecache.json` | `$ARKMCL_DATA_HOME/` |
| `saves` | `saves/` | `$ARKMCL_DATA_HOME/saves/` |
| `screenshots` | `screenshots/` | `$ARKMCL_DATA_HOME/screenshots/` |
| `servers` | `servers.dat` | `$ARKMCL_DATA_HOME/servers.dat` |
| `realms` | `realms_persistence.json` | `$ARKMCL_DATA_HOME/realms_persistence.json` |
| `resourcepacks` | `resourcepacks/` | `$MINECRAFT_HOME/resourcepacks/` |
| `resourcepacks-server` | `server-resource-packs/` | `$MINECRAFT_HOME/server-resource-packs/` |
| `shaderpacks` | `shaderpacks/` | `$MINECRAFT_HOME/shaderpacks/` |

`mods/` 不在默认链接内：由用户在实例 `mods/` 中自行创建到全局 `mods/<模组>` 的单文件链接。
`config/` 与 `options.txt` 在创建时从 `$ARKMCL_DATA_HOME` 复制，已存在不覆盖。

### server

| 名称 | 实例路径 | 目标 |
|---|---|---|
| `usercache` | `usercache.json`、`usernamecache.json` | `$ARKMCL_DATA_HOME/` |
| `cache` | `cache/` | `$MINECRAFT_HOME/cache/` |
| `libraries` | `libraries/` | `$MINECRAFT_HOME/libraries/` |
| `versions` | `versions/` | `$MINECRAFT_HOME/versions/` |
| `eula` | `eula.txt` | `$ARKMCL_DATA_HOME/eula.txt` |
| `ops` | `ops.json` | `$ARKMCL_DATA_HOME/ops.json` |
| `whitelist` | `whitelist.json` | `$ARKMCL_DATA_HOME/whitelist.json` |
| `banned-players` | `banned-players.json` | `$ARKMCL_DATA_HOME/banned-players.json` |
| `banned-ips` | `banned-ips.json` | `$ARKMCL_DATA_HOME/banned-ips.json` |

服务端实例创建时还会从 `$ARKMCL_DATA_HOME` 复制 `server.properties`、`bukkit.yml`、`spigot.yml`、`purpur.yml`，并把 `config-server/` 复制为 `config/`。

所有链接一律使用**相对路径**，移动整个数据目录后仍然有效。

`arkmcl instance <id> --linking <all|none|名称列表>` 用于显式创建或移除链接；不带值时输出当前状态（linked/real/missing/foreign）。创建时可用 `--linking all|none` 控制是否建立默认链接；未指定时使用全局默认（默认 `all`）。不带选择器的 `--linking all|none` 修改该全局默认：

- `all`：创建全部默认链接；
- `none`：移除所有指向全局目标的链接；
- `saves,resourcepacks`：只处理指定条目。

只删除指向允许根的链接；实例中的真实目录或指向别处的链接会记为 `skipped`。
`mods/` 不由启动器批量链接，用户可在实例 `mods/` 内自行选择性创建到全局 `mods/<模组>` 的链接，以复用资源。

服务端实例（`--type server`）：`purpur` 与 `fabric` 在实例根写入 `server.jar`（Purpur API / Fabric 服务端启动 jar），原版从版本元数据的 `downloads.server` 下载；`forge`/`neoforge` 运行 installer 的 `--installServer`，通过 `libraries/.../unix_args.txt` 启动。原版与 Purpur/Fabric 的命令为 `java <jvm_args> -jar server.jar --nogui`，Forge/NeoForge 为 `java <jvm_args> @libraries/.../unix_args.txt nogui`。

LWJGL：`arkmcl instance <id> --lwjgl <version>` 把该实例的 `org.lwjgl` 库切换到指定版本（Maven Central，校验和留空），在 `launch.prepare` 时生效；`--lwjgl default` 恢复版本 JSON 的默认值。只影响该实例，不改动其他版本的库文件。

分组：`groups.json` 保存分组名，成员关系记录在各实例的 `instance.cfg`。`arkmcl instance --group-move <from> <to>` 把 `<from>` 中的全部实例移动到 `<to>`，目标分组不存在时自动创建，源分组清空后自动删除。

软链接不可用时（EPERM、EOPNOTSUPP、Windows 无权限等）：

- 全局共享整体禁用，**不提供复制降级**。
- 新建实例使用本地真实目录（`saves/`、`resourcepacks/` 等各自独立）。
- 已存在的链接不自动转换；报告“共享不可用”，由用户决定是否迁移。
- 全局目录仍保留，但不再被新实例引用。

冲突规则：

- 实例中已存在真实文件/目录时，**不覆盖**，标记为“未链接”，由用户选择迁移或保留。
- 删除实例只删除链接本身，绝不删除链接目标。
- 备份记录链接路径与目标，不跟随链接复制内容；恢复时重建链接。
- 链接目标必须位于允许根（`$ARKMCL_DATA_HOME`、`$MINECRAFT_HOME`）内，防止路径穿越。
- 优先相对链接；跨根或跨文件系统时使用绝对链接，并在移动数据目录后提供修复动作。

## 权限与写入

- 目录 `0700`；配置文件 `0600`；socket `0600`。
- 配置文件与实例元数据使用“临时文件 + fsync + rename”原子写。
- 删除实例默认移动到 XDG 回收站（`$XDG_DATA_HOME/Trash`，含 `.trashinfo`）；跨文件系统时拒绝删除而不是复制。
- 下载临时文件与目标同目录（`<目标文件>.part`），完成后校验再 rename 到最终位置；`$MINECRAFT_HOME/cache/` 用于 installer 与 paperclip 缓存、安装日志。
- 不在 `$MINECRAFT_HOME` 写入官方启动器私有文件（`launcher_profiles.json`、`launcher_accounts.json` 等）。例外：安装 Forge/NeoForge 时，若 `launcher_profiles.json` 缺失会创建最小占位文件，以满足 installer 的前置检查。

## 与官方 .minecraft 共存

- 默认共用 `~/.minecraft`，可被 `$MINECRAFT_HOME` 覆盖到独立目录。
- 只写 `assets/`、`libraries/`、`versions/`、`natives/`、`cache/` 与全局内容目录；不修改官方启动器私有状态。
- 若用户希望隔离，设置 `MINECRAFT_HOME=$XDG_DATA_HOME/com.purft.arkmcl/minecraft` 即可。
