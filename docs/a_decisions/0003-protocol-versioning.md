# 0003 JSON-RPC 协议版本与能力协商

- 状态：生效
- 日期：2026-10-26
- 相关：AGENTS.md §3.3、§3.4、§13

## 背景

ArkMCL 的 CLI 与后续 GUI 都通过本地 JSON-RPC 访问 `arkmcl-service`。
服务端与客户端可能不同步升级；此前协议没有版本字段，无法判断兼容性。
需要提供最小、可测试的兼容边界。

## 备选方案

1. **每个请求携带版本**：字段冗余，且无法在首次连接时尽早失败。
2. **方法名内嵌版本**：如 `v1.instance.list`，破坏现有 RPC 命名约定，并与 `AGENTS.md` §3.4 的词根规则冲突。
3. **握手式版本（选定）**：`server.info` 返回 `protocol` 与 `capabilities`，客户端首次连接时校验。

## 决定

- 当前主版本：`PROTOCOL_VERSION = 1`（`arkmcl-platform::ipc`）。
- `server.info` 响应新增：
  - `protocol`：整数主版本；
  - `capabilities`：字符串数组，当前为 `account`、`instance`、`launch`、`resource`、`settings`。
- 客户端规则：
  - 首次连接先调用 `server.info`；
  - `protocol` 与客户端主版本不一致 → 拒绝继续并提示升级；
  - 缺失 `protocol` → 视为旧服务端，拒绝继续；
  - 成功后进程内缓存结果，不重复握手。
- 兼容规则：
  - 新增字段、新增方法、新增能力 → 不升主版本；
  - 删除/重命名方法、改变参数或结果语义、改变错误码语义 → 升主版本。
- `capabilities` 仅用于功能探测，不代表协议兼容；缺失某能力时客户端应隐藏或禁用对应功能。

## 代价

- 每个 CLI 进程启动多一次 `server.info` 调用（通常可忽略）。
- 服务端升级后，已运行客户端不会再次握手；重启客户端即可。
- 1.0 之前不承诺 N-1 兼容；1.0 之后需另立 ADR 定义兼容窗口。

## 后续

- 跨平台 transport（Windows named pipe / TCP + token）复用同一握手流程。
- GUI 客户端必须实现同一校验。
- 若未来需要流式订阅，新增能力名与通知方法，不升主版本。
