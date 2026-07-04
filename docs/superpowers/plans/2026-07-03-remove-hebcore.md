# 去除默认 hebcore：全功能进程内 Core Facade 计划

## 目标

Hebbian v1 默认回到各 surface 进程内直接引用共享 core crate：Desktop / heb CLI / hebweb 只做输入协议与输出渲染，所有核心功能进入同一套函数入口。

成功标准：

- 对话主链路：三端都调用 `surface_session::RuntimeRegistry → SessionRuntime → run_turn`。
- 同步能力：providers / sessions / projects / settings / permissions / skills / subagents / MCP / hooks 等都表达为 `core_rpc::CoreRequest` 并经 `core_rpc::dispatch` 到 `LocalCoreClient`。
- 默认启动：Desktop 不拉起 hebcore；hebweb 不兼任 hebcore；CLI 不再维护复制版 runner。
- 保留文件并发安全：跨进程只共享 `~/.hebbian/`，同 session 活 run 由 `SessionRunGuard` 兜底。

## 架构影响评估

1. **是否与架构.md 相悖？**
   - 已先更新 `docs/架构.md` §0 / §2 / §7，使 v1 默认 in-process 成为新的唯一设计准则。
   - 旧 `hebcore 单核心进程` 目标态被降级为实验远程 transport；未来重启必须先补 seq/replay/gap/HITL query。

2. **是否符合既定设计？**
   - 保留“Surface 是壳、Agent Core 是大脑、三端共享 `~/.hebbian/`、文件锁、`session.jsonl` 唯一历史文件”。
   - `surface-session` 承担活 run；`core-rpc::dispatch` 承担同步 API；二者合起来是 Shared Core Facade。

3. **是否引入新设计 / 需修改架构.md？**
   - 是：引入“全功能 core facade 唯一入口”作为 §7 新设计，已修改架构文档。
   - 不新增对外协议字段；短期主要是默认路径切换与重复 runner 收敛。

4. **会影响哪些其他模块？**
   - `apps/desktop`: 去掉默认 hebcore 客户端路径，新增/持有 `RuntimeRegistry`，Tauri command 改本进程 runtime。
   - `apps/web-server`: 删除 `spawn_hebcore_transport` 默认兼任，invoke 统一走 runtime/dispatch。
   - `apps/cli`: daemon runner 改用 `surface-session`，保留 NDJSON 协议与无人值守策略。
   - `crates/surface-session`: 泛化配置，支持 Desktop/CLI 需要的 turn meta、continue_run、derived_sink、HITL policy。
   - `crates/core-rpc`: 补齐 surface 需要但尚未覆盖的同步 API。
   - `apps/hebcore` / `surface-session::transport`: 标实验或后续移出默认 workspace。

5. **取舍**
   - 最小改动：只禁止 Desktop/hebweb 自动启动 hebcore，但保留 CLI/desktop/web 各自 runner。优点是快；缺点是违背“所有功能同入口”。
   - 干净方案：以 Shared Core Facade 收敛所有核心功能。改动更大，但能满足用户目标，并从设计上消除三端漂移。
   - 本计划采用干净方案，分阶段实施，先去默认 hebcore，再收敛 CLI runner。

## 实施阶段

### Phase 0：文档决策

- 更新 `docs/架构.md`：§0 / §2 / §7 / §13。
- 追加 `docs/changelog.md`。
- 本计划文件记录完整迁移步骤。

验证：`grep hebcore docs/架构.md` 不再把 hebcore 描述为默认目标态。

### Phase 1：泛化 `surface-session`

改动：

- 注释从 “hebcore/hebweb 共用” 改为 “三 surface 共用”。
- `TurnInput` / runtime config 扩展到等价承载 Desktop `chat::SendArgs` 的行为，不能直接丢字段后切换：
  - `meta: Option<MessageMeta>` 或等价字段；支持 wakeup / system notification。
  - `append_user: bool` 或 `TurnKind::{User, Resume}`；支持 continue/resume 不重复 append。
  - `force_automode`：Desktop `//force-automode` 只在 AutoMode 下折叠 judge Ask。
  - `enabled_tools` 优先级：本次输入显式值 > session 配置 > 全局设置。
  - `restrict_tools`：保留元素对话旁支只暴露 `PreviewStyle` 等工具的安全边界。
  - `derived_sink`：标题 / 记忆等 run 收尾派生事件必须走 long-lived 出口，不能因 Tauri invoke channel 生命周期丢失。
  - native side effects：灵动岛、渠道转发、完成提示由 surface adapter 订阅 WireEvent / RunOutcome 触发，不进入 core 业务。
- 引入 HITL policy / adapter：
  - `Interactive`：默认，pending 等 surface 回答，并允许 Desktop/hebweb 直接戳活 run 的 `HitlGate`。
  - `AutoResolve`：CLI `heb run` 用，审批自动拒、问题自动取消并计数。
- 注册 wakeup resume handler 不再依赖 `transport::TransportCtx`，改为基于 `RuntimeRegistry + data_dir + permission_store`。

验证：`cargo check -p surface-session`。

### Phase 2：Desktop 默认回进程内

改动：

- `apps/desktop/Cargo.toml` 加 `surface-session`。
- `apps/desktop/src/lib.rs`：
  - `.manage(RuntimeRegistry::new())`。
  - setup 删除 `hebcore_client::ensure_running` / log forward。
  - `send_message` 调 `RuntimeRegistry::ensure(...).input_tx.send(TurnInput)`。
  - `subscribe_session_events` 订阅 `runtime.state.subscribe()`，复用现有 Desktop sink 转发。
  - `cancel_message` / `inject_user_message` / `approve_permission` / `answer_question` / `set_run_mode` 改戳本进程 runtime state。
- `apps/desktop/src/hitl.rs`：删除或旁路 remote hebcore 代理路径。
- 保留 `hebcore_client.rs` 文件直到后续清理，避免一次性删除过大；但默认路径不可引用它。

验证：

- `cargo check -p hebbian --tests`。
- 至少跑一个 Desktop 相关单测；能 CLI 复现的 core 行为不用 Desktop 真机阻塞本阶段。

### Phase 3：hebweb 禁止默认 hebcore

改动：

- `apps/web-server/src/main.rs` 删除 `spawn_hebcore_transport` 调用与函数。
- `apps/web-server/Cargo.toml` 删除因此不再需要的 `fs2`（若无其他用途）。
- `server.rs` 注释改为本进程 runtime，不再写“升格 hebcore”。

验证：`cargo check -p hebbian-web-server`。

### Phase 4：CLI runner 收敛

改动：

- `apps/cli/Cargo.toml` 加 `surface-session`。
- 将 `daemon.rs` 中复制的 `run_turn` 删除，改为：
  - daemon state 持 `RuntimeRegistry` / `SessionRuntime`。
  - stdout NDJSON 通过订阅 `WireEvent` 后转换成 `DaemonEvent`。
  - `allow/deny/answer/cancel/input` 操作调用 runtime state。
  - `heb run` 的 auto resolve 接入 `surface-session` HITL policy。
- 保留 `DaemonEvent` 作为 CLI 协议；只把业务事件形态从 `WireEvent` 派生。

验证：

- `cargo check -p hebbian-cli`。
- 用 `heb new` 起 session，发送一条无需模型或使用测试 provider 的最小链路；若需要真实 provider，则记录无法本地全量验证的原因。

### Phase 5：hebcore 收尾

改动：

- 默认 surface 不再引用 `hebcore_client` / `surface_session::transport`。
- 将 `apps/hebcore` 标注 experimental，或移出默认 workspace（需单独评估 CI/用户脚本影响）。
- 删除过时文档 `docs/架构图-hebcore.html` 或标历史档案（不在本次强删）。

验证：workspace check 能通过；`git grep "ensure_running\|spawn_hebcore_transport\|hebcore_client::start_run"` 不应命中默认路径。

## 风险与回滚

- Desktop 当前 `chat.rs` 旧 in-process runner 能跑，但不能作为最终方案，因为它不是三端共享入口；只能借鉴已有 derived sink/native 适配。
- CLI runner 复制逻辑多，Phase 4 风险最高；如一次改太大，先让 CLI 通过 `surface-session` 跑 `heb run`，再迁 daemon 交互命令。
- `surface-session::transport` 保留期间要避免默认路径误用；用注释和 grep 检查兜底。
- 如某阶段编译风险过大，可阶段性提交文档 + hebweb 禁默认 hebcore，再继续 Desktop/CLI 收敛，但不能宣称“全功能同入口完成”。
