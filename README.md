# 日格 Daygrid

**把每一天，安排清楚。** 日格是一款本地个人桌面日历：点击日期记事，为每条事项选择颜色，也能让外部助手通过 MCP 访问同一份日历。无需账号、云服务或模型密钥。

当前为 **0.1.0 开发预览版**，优先验证「桌面界面 ↔ 本地服务 ↔ SQLite ↔ MCP」的最小闭环。完整 v0.1 仍在开发中。

![设计参考](design/preview.png)

## 已实现

- 周一开头的六行月历、月份切换、回到今天、窄窗口日期抽屉。
- 标题、换行备注、六色事项，新建、编辑、改日期、完成与恢复未完成。
- SQLite 持久保存、软删除、最近 100 条修改记录及删除恢复。
- 当前可见月份范围搜索（包含格子里的前后月日期）。
- 6 个真实 stdio MCP 工具：上下文、范围查询、按 ID 查询、新建、编辑、完成状态。
- 独立连接、只读或读写、暂停与永久撤销；配置不包含凭据。
- 事务内幂等回执、事项版本冲突、稳定分页与过期游标检查。

尚未实现：批量确认、MCP 删除与恢复、备份恢复、10 秒撤销、全局搜索与完整筛选、事件订阅、时区设置、签名、公证和自动更新。性能与完整 VoiceOver 验收尚未完成。

## 本地运行

桌面构建目前验证于 **macOS Apple Silicon，macOS 13+**。需要 Xcode Command Line Tools、稳定版 Rust、Node.js 20.19+ 或 22+、pnpm 10.33.2。

```sh
pnpm install --frozen-lockfile
pnpm dev
```

`pnpm dev` 先编译并准备本地服务与 MCP 桥接，再启动 Tauri 和 React 界面。

```sh
# 构建 macOS 应用（开发预览，尚无 Apple 签名/公证）
pnpm build
open target/release/bundle/macos/Daygrid.app

# 仅看浏览器界面预览
pnpm preview
```

浏览器预览明确显示提示，只使用当前页面内存数据；刷新后清空，不建立真实 MCP 连接。只有桌面应用连接真实 SQLite。

## 连接 MCP

1. 打开桌面应用的「助手连接」。
2. 填写连接名称，选择只读或读写，建立连接。
3. 将生成的 JSON 配置复制到 MCP 客户端；按客户端需要调整外层配置格式。
4. 助手先调用 `daygrid_get_context` 获取今天、时区和日历代际，然后用明确日期操作。

桥接程序由客户端启动，连接 UUID 只用于查找本机受保护的凭据。每次业务调用重新检查权限；暂停后无需重启旧桥接进程即可拒绝调用。外部助手可能将读取到的日历内容发送到其模型服务。

已验证：官方 Rust SDK `rmcp 3.5.1` 客户端通过真实子进程 stdio 调用，协商协议 `2025-11-25`。当前明确只开放该协议兼容基线；实际助手客户端接入仍需单独验证。

## 数据与架构

```text
React / Tauri ──┐
                ├── Unix Domain Socket ── daygrid-service ── SQLite
MCP stdio 桥接 ─┘
```

本地服务是数据库唯一写入者；界面和 MCP 共用校验、幂等、版本与历史。事项只属于一个纯日期，改变时区不会移动日期。界面每 750ms 刷新可见数据；后续替换为带序号的事件订阅。

macOS 数据默认在 `~/Library/Application Support/Daygrid/`。目录权限 0700，通信入口和凭据权限 0600。`DAYGRID_DATA_DIR` 可为测试指定隔离目录，桌面与桥接必须使用同一个目录。退出窗口不代表撤销助手连接；无活动调用五分钟后服务退出，后续访问按需启动。

安全边界是当前 OS 用户；持有同一用户文件访问权的进程可能读取本地数据与凭据。本版本不对外开放 HTTP 端口，也不提供任意文件或命令执行工具。

## 开发与验证

```sh
pnpm prepare:sidecars
pnpm --filter @daygrid/ui build
pnpm check
pnpm test
pnpm test:mcp
```

`test:mcp` 使用临时数据库、真实服务进程和 SDK 客户端，验证六个工具、重试防重、同源数据与实时暂停。GitHub Actions 运行检查、测试、MCP 联调和桌面构建。

- [实现架构与范围](docs/architecture.md)
- [验证记录](docs/verification.md)
- [MCP 工具契约](contracts/tools.json)
- [交互设计参考](design/README.md)

工程结构：`apps/desktop` 桌面壳，`packages/ui` React 界面，`crates/daygrid` 领域规则、SQLite、IPC 与两个可执行程序。初始实现将领域和存储组织为独立模块，后续按实际边界拆分 crates。

## License

[MIT](LICENSE)
