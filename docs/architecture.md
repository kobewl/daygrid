# 实现架构与范围

当前为 0.2.0。下面先写 0.2.0 相对 0.1.0 的变化，再保留原有的一致性规则与边界说明；与旧描述冲突之处以本节为准。

## 0.2.0 架构要点

```text
React / Tauri ──┐
                ├── Unix Domain Socket ── daygrid-service ── SQLite（唯一写入者，WAL）
MCP stdio 桥接 ─┘
```

- **单一真值契约**：`tools::catalog()` 生成 `contracts/tools.json`，同时驱动 MCP 的工具列表；`maintenance` 测试阻止契约与代码漂移。
- **确认流**：助手的删除（或设置为「所有写入」时的任意写入）不直接执行，而是写入 `proposals`（pending / approved / rejected / expired / failed，10 分钟过期）。桌面批准时重新检查日历代际与事项版本，通过后才执行并记入历史。MCP 侧用 `daygrid_get_proposal` 长轮询（`wait_seconds` ≤ 25）得到结果。
- **Schema v2**：新增 `proposals`、`activity`（每次 MCP 调用的审计：连接、工具、结果、耗时）；设置存放在 `metadata` 表；迁移前复制 `*.pre-v2.bak`；拒绝打开来自未来版本的数据库。
- **备份**：每天首次启动、升级迁移前、手动三种触发，使用 `VACUUM INTO` 生成一致性快照，保留数量有上限。
- **日志**：`tracing` 按天轮转保留 7 天，`DAYGRID_LOG` 控制级别；只记录操作类型、耗时、错误码，不写入标题与备注。
- **服务生命周期**：`poll(2)` 接受连接并在空闲五分钟后退出；版本不一致或旧服务通过 macOS 对端 PID（`LOCAL_PEERPID`）识别并替换；IPC 验证结果缓存 30 秒以减少往返。
- **客户端接入**：`clients.rs` 对 Cursor 与 Claude Desktop 的 JSON 做最小合并（只改 `mcpServers.daygrid`，先备份，无效 JSON 拒绝改写）；Claude Code 与 Codex 只生成命令，不改它们的配置。
- **MCP 桥接**：9 个工具、3 个资源（含模板）、2 个提示词；`--check` 自检；每次业务调用重新鉴权。
- **界面**：`useCalendar` 集中数据与乐观更新，写入走串行队列保证事项版本始终最新，`epoch` 丢弃过期响应；按月取范围并在空闲时预取前后各一个月；`desktop_pulse` 每 2 秒读取几个整数，仅在有变化时拉取，窗口在后台时暂停；主题用 CSS 变量与 `[data-theme]`，`system` 跟随 `prefers-color-scheme`。

## 0.2.0 仍未实现

批量计划与事务、MCP 侧恢复与历史查询工具、备份恢复界面、全局筛选、事件订阅、时区设置、Apple 签名与公证、自动更新。

## 多人协同（Unreleased，schema v4）

- **拓扑**：每台电脑仍是「本地服务 + SQLite」，可选地连接一个 `daygrid-server`（axum + SQLite，服务端权威）。同步引擎在 `crates/daygrid/src/sync/`（`hlc` 混合逻辑时钟、`merge` 字段合并、`client` 推拉循环、`runtime` 连接管理），本地存储侧在 `store/sync_ops.rs`。
- **核心不变量**：共享事项的本地内容 = 服务器快照（`sync_items`）+ 出箱（`sync_outbox`）叠加，由 `rebuild_local` 重算。所有本地写入走 `reconcile_item`：把本地内容与「有效状态」做差，产出 Create / Restore / Update / Delete 操作，每个字段带 `prev`（修改前的值）。因此被拒绝、LWW 落败、重放的操作都不会造成漂移。
- **合并**：服务端按字段处理。`prev == 当前值` 直接采用；否则以 `prev`（缺省取 `base_rev` 的历史版本）为共同祖先，备注做 diffy 三方合并，其余字段 / 合并冲突回退到 HLC 较新者（LWW）并写入冲突记录。不做任何按用户身份的短路。删除优先，恢复按 LWW。
- **幂等**：每个操作有 `op_id`，服务端 `applied_ops` 记录（含用户与内容摘要）；同一 `op_id` 内容不同返回 `OP_ID_REUSED`。
- **可见性**：`Item.visibility`（`private` 默认 / `shared`）只存在本机事项 JSON 里，私有事项从不产生操作。`desktop_set_visibility`（仅桌面通道）：共享 → 私有 = 服务端软删除并保留本地副本，私有 → 共享 = Restore + 差异 Update。MCP 创建的事项恒为私有。
- **团队切换**：`sync_last_team`（`url|workspace`）变化时所有事项转私有、丢弃快照与冲突，并把曾经共享过的事项排除在 `share_existing` 之外；同一团队重连则保留快照并对账离线期间的编辑与删除。
- **角色**：owner / editor / viewer 随 `PullResponse.role` 到达；降级为 viewer 时丢弃出箱并记为 `read_only` 冲突、回滚本地；viewer 仍可写私有事项。
- **已知限制**：SSE 的 30 秒令牌复核已实现但没有端到端测试。多人协同本身没有改动 MCP 契约；同一版本里的「提醒」把 `contract_version` 升到 `0.3`（见下一节）。

## 提醒（Unreleased，schema v5）

```text
界面 / MCP / 同步线程 ─IPC─▶ daygrid-service / Store ──同一事务──▶ items.body.remind_at + reminders 表
                                        │ 每次调用后：最早待触发时间变了才写
                                        ▼
                             <data_dir>/reminders.next   {"v":1,"next_due_ms":…}
                                        │ kqueue（目录 NOTE_WRITE），超时 ≤ 60 s
                                        ▼
                 桌面进程 · reminders 线程 ──到点──▶ desktop_take_due_reminders ──▶ macOS 通知横幅
```

- **数据表示**：`remind_at` 是事项 JSON 里的普通字段，唯一真值。规范形式 `YYYY-MM-DDTHH:MM:00±HH:MM`，偏移取日历时区在该时刻的偏移，分钟精度；`null` 表示不提醒。字符串没有歧义，前 16 位就是日历时区的墙上时间，界面不用做时区换算。输入接受日历时区的本地串（`YYYY-MM-DDTHH:MM`，可带秒，`T` / `t` / 空格分隔）或带 `Z` / `±HH:MM` 的 RFC 3339；秒舍去；夏令时回拨取较早的时刻，跳过的时段报错。直接写入时不能早于当前 60 秒以上；批准确认请求时不再检查，过期的提醒直接作废。
- **不进入团队同步**：同步层的结构体都是 `deny_unknown_fields`，把它加进 `sync::FIELDS` 会让旧客户端整体同步失败；而且提醒本来就是个人设置（同事设的 8 点提醒不该在我这里弹），「是否已触发」也只能按设备记。所以它和 `visibility` 一样只存在本机：`reconcile_item` 只比较 `FIELDS`，改提醒不产生 outbox 操作。同事改了共享事项的日期时，`rebuild_local` 把提醒按同样的天数平移（时刻不变），同样不产生 outbox；同事删除事项时删掉提醒行。只读成员可以只改提醒（`ItemFields` 和 `visibility` 不变时放行）。
- **`reminders` 表的不变量**：`reminders(item_id, due_ms, fired_at)` 是派生索引。一个事项在表里有一行，当且仅当它未完成、未删除、有提醒，且 `due_ms` 等于 `remind_at` 的毫秒时刻。所有写入路径（`apply`、`rebuild_local`、迁移 backfill）都在同一事务里调用 `sync_row` 维护它。时刻没变时保留 `fired_at`（只触发一次）；时刻变了、重新打开或恢复时重新生效，但新时刻早于「现在 − 60 秒」就直接标记作废，不补发。下一个提醒用部分索引 `reminders_pending` 查 `MIN(due_ms)`，O(log n)。
- **唤醒链路**：服务在每次调用结束时求一次最早待触发时间，只有值变了才原子改写 `reminders.next`（tmp + rename，0600，只含时间戳）。桌面线程用 kqueue 监视数据目录，超时取「到下次触发的剩余时间」和 60 秒中较小的那个；目录一变就醒来读提示，值变了才采用。到点后调用 `desktop_take_due_reminders`（只限桌面通道）：服务在一个事务里先标记 `fired_at` 再返回（最多 200 条），所以每个提醒最多触发一次；24 小时内错过的补发并标「错过的提醒 · 」，更早的静默作废。同一批超过 3 个时，最早的 3 个各发一条，其余合并成「还有 N 个提醒」。
- **为什么由桌面进程触发**：服务按需启动、空闲 5 分钟退出，而且没有 bundle 身份；MCP 桥接可能有多个也可能没有。只有桌面主进程在应用运行期间一直存在，并且能以「日格」的身份发通知。桌面进程也不直接读 SQLite：服务仍是唯一访问数据库的进程，提示文件只是一个时间戳。
- **关窗改为隐藏**：macOS 上关闭主窗口（红灯 / ⌘W）只隐藏窗口，点程序坞图标（`RunEvent::Reopen`）重新显示；⌘Q 才退出。隐藏时外壳发 `daygrid://window-hidden`，界面停掉 2 秒一次的 `desktop_pulse` 和跨日计时器，不挂任何周期计时器；`daygrid://window-shown` 或窗口获得焦点时立即刷新一次。这样隐藏后服务仍能照常空闲退出。代价：隐藏期间新的待确认请求不会让程序坞图标跳动，重新显示窗口后才出现。全屏时直接隐藏会留下一个黑色的全屏空间，所以 `CloseRequested` 时若窗口是全屏，先 `set_fullscreen(false)`，收到 AppKit 的 `NSWindowDidExitFullScreenNotification` 后（推迟一轮事件循环）再隐藏（`apps/desktop/src/hide_on_close.rs`）；tao 的 `is_fullscreen()` 在动画开始时就变成 false，`Resized` 也会在动画中途触发，都不能用来判断结束。退出动画期间再按 ⌘W 不做事，3 秒内没收到通知视为退出失败，下次关窗直接隐藏；点程序坞图标会清掉待隐藏标记，窗口以普通大小重新出现；⌘Q 不受影响。
- **性能取舍**：没有按秒轮询；空闲时只有一个名为 `reminders` 的线程阻塞在 `kevent` 上。60 秒的超时上限用来兜底系统睡眠唤醒、改系统时钟和提示文件丢失的情况（最多 60 秒内补上）；数据目录里其他文件的变化也会唤醒它，代价只是读一次不到 100 字节的文件。窗口隐藏时保留 App Nap（不关 App Nap、不常驻 activity），计时器可能被系统推迟。设计里的回退方案（提前 5 分钟开始一个短时 activity）**这次没有实现**，要等在打包应用上实测推迟时间后再决定（见 `docs/verification.md`）。实测的空闲 CPU、内存和线程开销也还没有记录。性能基线：1 万条事项、1 千个提醒时，`next_reminder_due` 和一次取满 200 条的 `take_due` 都在测试阈值内（1 ms / 10 ms）。
- **点击通知的限制**：`tauri-plugin-notification` 在桌面平台上没有点击回调。点击通知时系统会激活日格，但如果没有触发 `Reopen`，隐藏的窗口不会自动出现，需要再点一次程序坞图标。
- **投递无法检测**：插件在 macOS 上不反馈投递结果（`show()` 吞掉错误），通知是 at-most-once，丢了不重发。设置页「发送测试通知」成功只代表请求已发出，看不到时引导用户去「系统设置 › 通知 › 日格」检查。
- **前台也弹横幅**：日格是活跃应用时，macOS 默认不弹 NSUserNotification 横幅。启动时给 mac-notification-sys 的 `NotificationCenterDelegate` 类补上 `userNotificationCenter:shouldPresentNotification:`（固定返回 YES，只新增、不替换），所以窗口可见、⌘W 隐藏后没切走、在后台三种状态都弹横幅并播放默认提示音。桌面测试会检查这个类仍然存在。
- **锁定通知依赖版本**：`tauri-plugin-notification =2.4.0`，并用 `cargo update --precise` 把 `notify-rust` 锁在 4.18.0、`mac-notification-sys` 锁在 0.6.15。「`show()` 吞错」「默认提示音参数」「delegate 类名」都依赖这几个版本的源码行为，升级任何一个都要重新核对，并跑类名测试。
- **契约 0.3**：`daygrid_create_item` / `daygrid_update_item.patch` 接受 `remind_at`（patch 里 `null` 清除、不传不变，只改日期时平移）；所有返回事项的工具都带 `remind_at`（升级前写下的回执和确认结果重放时补 `null`）；新增只读工具 `daygrid_list_reminders`（共 10 个工具）；`daygrid_get_context` 新增 `now`（日历时区，带秒）。`contracts/item.schema.json` 也由代码生成（`$id` 为 `urn:daygrid:item:2`）。
- **Schema v5**：新增 `reminders` 表。迁移前生成并校验 `calendar.db.pre-v5.bak`，备份失败或 backfill 失败时版本号不变。0.4.0（v3）会一步升到 v5。

## 0.4.0：休息日标记与备注链接

- **Schema v3**：新增 `day_marks(date, kind, updated_at)`，只存手动标记（`rest` / `work`）。`desktop_list_day_marks` 按范围读取，`desktop_set_day_mark` 写入，`kind=default` 删除；两者只对桌面通道开放，不在 MCP 工具里。迁移前用 `VACUUM INTO` 写临时文件，只读校验（`integrity_check` 为 ok、`user_version` 等于迁移前版本）、fsync 后原子改名为 `*.pre-v3.bak`；已有同名备份同样校验，无效则替换。备份失败时 `Store::open` 返回 `STORAGE_ERROR`，不执行迁移，数据库保持原版本。
- **默认的休 / 班不入库**：周末与法定假日、调休补班由前端 `lib/workday.ts` 推算，数据在 `lib/holidays.ts`（2025–2026，出处写在文件里）。优先级：手动标记 > 法定放假 / 补班 > 周末。
- **标记读写协调**：`lib/dayMarkCache.ts` 串行读取与保存，缓存已确认状态并叠加尚未完成的用户选择；保存失败只移除对应的乐观选择，后续选择继续生效，全部失败恢复已确认状态。
- **备注仍是纯文本**，渲染时才解析 Markdown（`react-markdown` + `remark-gfm` + `remark-breaks`，不解析原始 HTML）。`lib/linkify.ts` 在渲染前把裸网址包成 `<...>`，让网址边界由「URL 合法 ASCII 字符」决定，避免紧挨的中文被吞进链接。
- **链接**：只放行 `http`、`https`、`mailto`。桌面端通过 Tauri 命令 `open_external` 调用系统 `open`，Rust 端再校验一次；全局点击兜底保证任何漏网的链接都不会让 WebView 自己导航。

---

以下为 0.1.0 的决策记录，仍然有效的部分保留，已被取代的描述已标注。

## 本轮决策

2026-10-11：最小开发预览保留 Tauri 2、Rust、React、TypeScript、SQLite 和 stdio MCP 路线。先交付单条事项闭环，不将设计原型标为正式功能。

- `crates/daygrid/src/domain.rs`：纯日期、Unicode 标量值长度、输入字段与事项规则。
- `store.rs`：唯一写入者内部的 SQLite 仓储；提交、历史和回执在同一事务。
- `ipc.rs`：受保护的本地路径、凭据、有限超时与服务按需启动。
- `daygrid-service`：OS 文件锁保证单实例；串行调度请求，SQLite WAL + synchronous=FULL。
- `daygrid-mcp`：官方 rmcp SDK 协议适配，（0.1.0 为六个工具，0.2.0 已扩展为九个）。
- `apps/desktop`：受限 Tauri command 调用；连接创建时将一次性凭据保存到本机文件，不送入前端。
- `packages/ui`：正式桌面数据调用和明确标记的浏览器内存预览。

## 一致性规则

每次操作先验连接权限。写入检查 `calendar_generation`，然后匹配幂等回执，再校验事项版本。新请求使用新 UUID，重试沿用原键；同键不同工具或内容拒绝。版本冲突包含当前内容，编辑草稿保留，让用户核对后重新保存。

有效修改才递增 revision 和 history sequence。无变化更新返回 changed=false，仍保存幂等回执。软删除保留 ID；恢复递增版本并生成新历史。（已取代：0.2.0 开放删除，但必须经确认流；恢复与批量仍未开放。）

当前回执长期保留；尚无自动清理策略。请求摘要使用工具名与 JSON 参数的稳定键序列，标题首尾空白虽然会被领域层去除，但不同原始参数仍视为不同请求，调用方重试必须保持原始参数。

列表范围最多 366 天，每页最多 100 条，按日期、未完成优先、创建时间和 ID 排序。游标绑定查询、数据代际、历史序号与分页位置，并以服务本机秘密校验；期间发生有效修改会使旧游标失效。

## 与完整设计的差异

首轮以一个 Rust crate 中的模块保持领域、存储和进程边界，暂不建立五个小 crate。数据库使用受事务保护的业务 JSON 快照与必要排序字段。完整 schema checksum 迁移、恢复数据代际递增、备份及计划表将在相关能力实现时新增。

（已取代：0.2.0 改为 2 秒 pulse 轮询，见上。）窗口刷新使用查询序号防止过时响应覆盖新页面。活动 MCP 桥接每分钟健康检查服务；服务按最后一次请求空闲五分钟退出。后续切换持久 IPC 订阅和连接引用生命周期。

授权整份个人日历，无账号和多租户。连接状态只能从受限桌面通道修改；旧授权进程每次调用仍需服务端检查。此方案无法隔离已经控制同一 OS 用户的本机进程。

MCP 兼容基线是 2025-11-25，使用 SDK 3.5.1；不宣称该协议是最新版本。新协议、实际助手客户端配置和兼容列表将依据运行证据增补。

## 后续顺序

1. 用户实际 MCP 客户端接入，覆盖窗口关闭、重连、写入同步与冲突草稿。
2. 待确认计划、删除与批量事务、确认与拒绝、过期和版本检查。
3. 一致性备份、完整恢复、代际失效与迁移故障恢复。
4. 全局搜索、筛选、撤销、时区设置、事件订阅及性能与无障碍验收。
5. 独立签名、公证、更新与安装升级验证。

完整 v0.1 只有在上述必选功能及正式验收通过后才能宣称完成。
