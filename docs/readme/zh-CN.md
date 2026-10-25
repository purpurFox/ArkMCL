# ArkMCL

Rust 服务端 + CLI + Flutter 前端。

## 目录结构

```text
.
├── Cargo.toml          # Rust workspace
├── crates/
│   ├── core/           # 领域逻辑（平台无关、无 UI 依赖）
│   ├── l10n/           # 本地化目录（zh-CN、en-US）
│   ├── platform/       # 路径解析与 Unix JSON-RPC 传输
│   ├── storage/        # 实例仓储、文件系统与软链接
│   ├── cli/            # CLI，二进制名 arkmcl
│   └── service/        # 守护进程，二进制名 arkmcl-service
├── apps/
│   └── gui/            # Flutter 应用（当前仅维护 Linux）
└── docs/
```

## 环境要求

- Rust stable ≥ 1.85（workspace 使用 edition 2024）
- Flutter stable 3.47+（Dart 3.13+）
- Linux 桌面构建需要 clang、cmake、ninja、pkg-config

## 快速开始

### CLI

```bash
cargo run -p arkmcl-cli -- version            # ArkMCL 0.1.0
cargo run -p arkmcl-cli -- info               # JSON：名称、版本、语言、socket
cargo run -p arkmcl-cli -- --lang zh-CN help  # 中文帮助
```

退出码：成功 `0`，用法错误 `2`，运行时失败 `1`。正常结果写 stdout，诊断与错误写 stderr。

### 服务端

```bash
# 前台运行；生产环境应由 systemd 或包管理器托管
cargo run -p arkmcl-service -- --lang zh-CN

# 另一个终端
cargo run -p arkmcl-cli -- --lang zh-CN service status
cargo run -p arkmcl-cli -- --lang zh-CN service stop

# 实例
cargo run -p arkmcl-cli -- --lang zh-CN instance my-instance --create --name "My Instance" --version 1.20.4
cargo run -p arkmcl-cli -- --lang zh-CN instance fabric-demo --create --name "Fabric Demo" --version 1.20.4 --loader fabric --loader-version 0.15.11
cargo run -p arkmcl-cli -- --lang zh-CN instance --list
cargo run -p arkmcl-cli -- --lang zh-CN instance my-instance --info
cargo run -p arkmcl-cli -- --lang zh-CN instance my-instance --default
cargo run -p arkmcl-cli -- --lang zh-CN instance my-instance --memory 4096 --jvm-args "-Xmx4G -Dfoo=bar"
cargo run -p arkmcl-cli -- --lang zh-CN instance my-instance --version 1.20.4
cargo run -p arkmcl-cli -- --lang zh-CN instance my-instance --loader fabric --loader-version 0.15.11
cargo run -p arkmcl-cli -- --lang zh-CN instance my-instance --loader none
cargo run -p arkmcl-cli -- --lang zh-CN instance --group
cargo run -p arkmcl-cli -- --lang zh-CN launch my-instance --prepare
cargo run -p arkmcl-cli -- --lang zh-CN launch my-instance --start
cargo run -p arkmcl-cli -- --lang zh-CN launch my-instance --terminal
cargo run -p arkmcl-cli -- --lang zh-CN launch --list
cargo run -p arkmcl-cli -- --lang zh-CN launch my-instance --stop

# 账户
cargo run -p arkmcl-cli -- --lang zh-CN account --list
cargo run -p arkmcl-cli -- --lang zh-CN account --refresh
cargo run -p arkmcl-cli -- --lang zh-CN account Player --add
cargo run -p arkmcl-cli -- --lang zh-CN account Ext --add --server https://example.com/auth
cargo run -p arkmcl-cli -- --lang zh-CN account --add   # 微软设备码登录
cargo run -p arkmcl-cli -- --lang zh-CN account Player --default
cargo run -p arkmcl-cli -- --lang zh-CN account Player --remove

# 资源
cargo run -p arkmcl-cli -- --lang zh-CN resource --search sodium --type mods --list-limit 5
cargo run -p arkmcl-cli -- --lang zh-CN resource --download sodium --download-to-instance my-instance --filter-version 1.20.4 --filter-loader fabric
cargo run -p arkmcl-cli -- --lang zh-CN resource --list-remote --filter-vanilla stable --list-limit 5
cargo run -p arkmcl-cli -- --lang zh-CN resource --list-remote --filter-loader fabric --filter-version 1.20.4 --list-limit 5
cargo run -p arkmcl-cli -- --lang zh-CN resource --list-remote --filter-loader forge --filter-version 1.20.4
cargo run -p arkmcl-cli -- --lang zh-CN resource --list-remote --filter-loader neoforge --filter-version 1.20.4
```

- socket 默认位于 `$XDG_RUNTIME_DIR/com.purft.arkmcl/arkmcl.sock`。
- `XDG_RUNTIME_DIR` 缺失时退化到 `$XDG_STATE_HOME/com.purft.arkmcl/run/arkmcl.sock`。
- 当前 RPC：`server.info`、`server.shutdown`、`instance.*`、`launch.*`、`account.add/list/default/remove`。

### Shell 补全

```bash
# bash
arkmcl completion bash > ~/.local/share/bash-completion/completions/arkmcl

# zsh
arkmcl completion zsh > "${fpath[1]}/_arkmcl"

# fish
arkmcl completion fish > ~/.config/fish/completions/arkmcl.fish
```

补全包含命令、选项、实例 ID/名称、账户名与分组；动态值通过 `arkmcl complete <topic>` 获取，服务未运行时静默返回空。

### 本地化

- 支持 `zh-CN` 与 `en-US`。
- 优先级：`--lang` > `ARKMCL_LANG` > `LC_ALL` > `LC_MESSAGES` > `LANG`。
- 不支持的语言标签回退到 `en-US`，并在 stderr 给出警告。
- 目录文件位于 `crates/l10n/locales/`，编译时内嵌。

### GUI

```bash
cd apps/gui
flutter run -d linux
```

当前只针对 Linux 桌面做适配与验证；`apps/gui/web/` 目录虽由模板生成，但未纳入维护范围。

Linux 侧配置：

- 应用 ID（GTK/DBus）：`com.purft.arkmcl`（`apps/gui/linux/CMakeLists.txt`）
- 窗口标题：`ArkMCL`；默认 1280×720，最小 720×480（`apps/gui/linux/runner/my_application.cc`）

## 验证

```bash
# Rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace

# Flutter（在 apps/gui 下）
dart format --output=none --set-exit-if-changed .
flutter analyze
flutter test
```

## 当前状态

已完成：

- Rust workspace：`arkmcl-core`、`arkmcl-l10n`、`arkmcl-platform`、`arkmcl-storage`、`arkmcl-cli`、`arkmcl-service`
- CLI：`account`、`instance`、`launch`、`resource`，支持分组列表、通配符、批量确认与 `--json`，中英双语；服务未运行时自动启动 `arkmcl-service`（`ARKMCL_NO_AUTOSTART=1` 禁用，`service status`/`stop` 不自动启动）；帮助条目按字母升序排列
- 服务端：Unix socket + JSON-RPC，支持 info、shutdown，实例 CRUD、配置更新与锁检查
- 路径解析：XDG 默认、显式环境变量、便携模式（默认禁用）
- 实例仓储：JSON 存储、ID 分配、配置校验、原子写
- 时间统计：实例创建/最近启动/累计运行，账户创建/最近使用/累计运行；用于列表排序
- 软链接：默认全局存档/资源包/光影等链接，冲突不覆盖，mod 按文件链接
- 实例锁：`.arkmcl/lock`，PID + token，陈旧锁自动回收，运行中禁止删除
- 软链接探测：在实例目录实际创建临时链接验证，不可用则禁用共享
- 元数据模型与缓存：`Manifest`、`Version`、`AssetIndex`，统一存放在 `assets/indexes/`；安装器产物仍可回退读取
- 元数据抓取：仅 HTTPS、30s 超时、16 MiB 上限；可变元数据 24h TTL + 条件请求，不可变元数据永久缓存；网络失败回退旧缓存并标记 stale；下载后按 `SHA-1` 校验
- Fabric 支持：loader 版本列表、profile 抓取与基础版本合并；客户端 jar 仍指向基础版本
- Forge/NeoForge：下载官方 installer 并执行，需 Java 与网络；产物写入 `$MINECRAFT_HOME/versions` 与 `libraries`；当前为直接安装，无沙箱回滚
- Fabric 组件：`instance <id> --create --loader fabric` 写入实例 `pack.json`（不下载库）
- 内容下载：客户端 jar、库（含 Fabric Maven 地址）、natives jar 与资源对象；默认 4 并发（1..=8）、断点续传、退避重试与进度通知；`SHA-1` 校验
- 资源下载：mod / resourcepack / shader / datapack / world / plugin 可下载到实例或全局；modpack 仅支持全局。下载落点统一在 `$MINECRAFT_HOME`；用户共享存档仍在 `$ARKMCL_DATA_HOME/saves`，两者不混用
- CurseForge：`--api curseforge` 支持搜索、版本查询与下载；需要设置 `CURSEFORGE_API_KEY`
- 服务端：`--type server`；支持 `fabric`、`forge`、`neoforge` 与 `purpur`，未指定加载器时使用原版服务端；Fabric 下载官方服务端启动 jar，Forge/NeoForge 运行 `--installServer`；支持启动、控制台与停止
- 实例默认值：ID/名称为 `<loader>-<version>`，内存 4096（client ≥512，server ≥256），JVM 参数默认 Aikar's flags，分组默认 `<loader>`；`--create-defaults` 控制默认配置复制，`--move` 修改实例 ID
- mrpack 导入：`instance --import <pack.mrpack|https://...>` 解析 `modrinth.index.json`、解压 overrides、按端过滤并下载文件，创建对应加载器实例；远端仅接受 HTTPS，导入完成或失败后清理缓存
- 资源依赖：Modrinth 递归解析 `required` 依赖，`incompatible` 冲突会报错，结果包含 `resolved`
- `--autoremove`：列出未被任何实例引用的全局 mods/resourcepacks/shaderpacks/datapacks；`--yes` 移动到回收站
- 系统密钥环：优先使用 `secret-tool`，不可用时回退 `tokens.json`
- 全局默认项：保存在 `settings.json` 的 `instance` 字段；不带选择器时 `--memory|--version|--jvm-args|--group` 读写全局默认；带选择器且不带值时输出该实例的当前设置"
- natives 解压：按 `extract.exclude` 过滤，使用 ZIP `enclosed_name` 防路径穿越
- 启动准备：`launch <id> --prepare` 抓取元数据、合并 loader、下载依赖、解压 natives，并返回启动计划
- 启动执行：`launch <id> --start`、`launch <id> --terminal`、`launch --list`、`launch <id> --stop`；控制台转发 stdout/stderr 并接受 stdin，退出码与崩溃报告写入实例状态；日志写入实例 `logs/latest.log`
- Java 探测：`JAVA*_HOME`（如 `JAVA8_HOME`、`JAVA17_HOME`）、`JDK_HOME`、`PATH`、`~/.jdks` 与系统 JVM 目录；解析主版本/架构/供应商，优先精确匹配，否则选择满足要求的最低版本
- 账户：离线（MD5 离线 UUID）、外置认证服务器、微软设备码登录；`accounts.json` 只存元数据，令牌保存在服务进程内存
- 默认实例：`instance <id> --default` 设置；`instance --info`、`launch --start`、`launch --prepare`、`launch --terminal` 省略 id 时使用默认实例
- 实例账户：可选，默认不写入；启动时按“显式账户 > 实例账户 > 默认账户”选择
- 启动计划：平台规则、classpath、natives、JVM/游戏参数与占位符替换（纯逻辑，不启动进程）
- 单元测试与 CLI/服务端集成测试

尚未实现：

- Quilt 组件安装与版本源
- Java 自动下载（按 Q-004 仅提示用户）
- 镜像源与下载源切换（Q-006）
- 并发启动上限（Q-009）
- 离线/外置登录的正版门槛（D-011）
- world 安装
- 导入现有实例目录（非 mrpack）
- 服务端：世界管理、server.properties 默认生成
- GUI 与服务端通信；界面仍显示“后端：未连接”
- 本地 `--list` 目前只扫描全局目录，并按 modpacks 列出实例；不递归扫描实例内的 mods/resourcepacks
- 包管理器分发（Q-012）
- macOS / Windows 支持；Web 未验证

## 许可证

本项目以 **GPL-3.0-or-later**（GNU General Public License v3.0 或更高版本）授权，完整文本见根目录 `LICENSE`。

- 本项目不包含 Minecraft 游戏资源；使用本项目仍需自行拥有合法的 Minecraft。
- `Minecraft` 是 Mojang Studios / Microsoft 的商标，本许可证不授予商标使用权。
- 本项目与 Mojang Studios、Microsoft 或 Purpur 项目无隶属关系。

## 设计边界

- `arkmcl-core` 不依赖 CLI、GUI、操作系统或具体通信方式。
- `arkmcl-l10n` 只负责语言目录与格式化，不包含业务逻辑。
- `arkmcl-platform` 承载路径与 Unix IPC 等平台细节。
- CLI 只通过 JSON-RPC 访问服务端，不复制业务规则。
