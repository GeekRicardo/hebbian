# 单文件回退与反回退 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修改文件 sidebar 支持每个文件独立「回退 / 恢复」，同时保留整次 Run 回退。

**Architecture:** 现有 §4.13 只支持整 Run 回退，本计划把状态粒度扩展到 `TurnFileChange`：每个文件记录 `reverted/reverted_at_ms`，Run 的 `reverted` 由所有文件状态派生/同步。后端复用 edits-worktree 现有 before/after sha 和 per-file lock，不新增影子仓机制；前端通过新 Tauri command 调用单文件回退/恢复。

**Tech Stack:** Rust agent-core edits-worktree、Tauri command、React + TypeScript、inline Node sanity test、cargo tests。

---

### Task 1: 架构和类型扩展

**Files:**
- Modify: `docs/架构.md`
- Modify: `crates/protocol/src/event.rs`
- Modify: `crates/agent-core/src/edits/metadata.rs`
- Modify: `apps/desktop/frontend/src/desktop/ui/types.ts`

- [ ] **Step 1: 更新架构 §4.13.10 / §13**

把 `docs/架构.md:2709-2715` 改为：

```markdown
数据源：`listEdits(currentSessionId)`，按 Run 完成时间倒序。空数组 → sidebar 显示空状态。每条 Run 渲染为一个分组：

- 标题："最新一次修改" / "较早的修改" + 完成时间 + 文件数
- 组内每个文件一张卡片，展示文件名、action 角标（create/modify/overwrite/delete）、大小变化、单文件回退/恢复按钮；delete 类只标红"已删除"不渲染 diff
- 点击文件卡片 → DiffViewer 展示净变化，数据来自 `diffEdit(sessionId, runId, realPath)`
- 点击文件回退按钮 → `revertEditFile(sessionId, runId, filePath)`；只反向应用该文件的 before/after
- 点击文件恢复按钮 → `restoreEditFile(sessionId, runId, filePath)`；只正向应用该文件的 before/after
- 点击 Run 回退按钮 → `revertEdit(sessionId, runId)`；等价于对所有未回退文件逐个执行单文件回退；所有文件已回退后 Run 置灰
```

并在 §13 追加一行决策：

```markdown
| 4.13 | 单文件回退/恢复 | 在保留整 Run 回退心智的基础上，允许 sidebar 对单个文件执行 before↔after 切换；状态落在 TurnFileChange 上，Run.reverted 由全部文件状态汇总。理由：用户在修改文件栏逐文件检查 diff 时，最自然的修正粒度是当前文件。 |
```

- [ ] **Step 2: 扩展 Rust metadata 类型**

`crates/agent-core/src/edits/metadata.rs` 中 `TurnFileChange` 增加：

```rust
#[serde(default)]
pub reverted: bool,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub reverted_at_ms: Option<i64>,
```

`From<TurnFileChange> for protocol::TurnFileChange` 同步填充字段。

- [ ] **Step 3: 扩展 protocol 类型**

`crates/protocol/src/event.rs` 中 `TurnFileChange` 增加同名字段，使用 serde default 保持旧 jsonl 兼容。

- [ ] **Step 4: 扩展前端类型**

`apps/desktop/frontend/src/desktop/ui/types.ts` 的 `TurnFileChange` 增加：

```ts
reverted?: boolean;
reverted_at_ms?: number | null;
```

- [ ] **Step 5: 运行类型检查**

Run: `cargo check -p protocol && pnpm exec tsc --noEmit`（第二条在 `apps/desktop` 目录）

Expected: 两条都通过；若旧构造点缺字段，补 `reverted: false, reverted_at_ms: None`。

### Task 2: 后端单文件 apply 能力

**Files:**
- Modify: `crates/agent-core/src/edits/mod.rs`
- Modify: `crates/agent-core/src/edits/metadata.rs`
- Test: `crates/agent-core/src/edits/mod.rs`

- [ ] **Step 1: 写失败测试**

在 edits 测试模块新增：创建临时 workspace 文件 `a.txt` 和 `b.txt`，begin/finalize 得到一个 run，两文件都有 modify；调用 `revert_file(&entry.files[0])` 后断言只有 `a.txt` 回到 before，`b.txt` 保持 after；再调用 `restore_file(&entry.files[0])` 断言 `a.txt` 回到 after。

- [ ] **Step 2: 跑红测**

Run: `cargo test -p agent-core edits::tests::revert_and_restore_single_file_only --lib`

Expected: FAIL，提示 `revert_file` / `restore_file` 不存在。

- [ ] **Step 3: 抽 per-file apply helper**

把 `revert_run` 内单文件逻辑抽为：

```rust
pub async fn revert_file(&self, run_id: &str, file: &TurnFileChange) -> AppResult<()>;
pub async fn restore_file(&self, run_id: &str, file: &TurnFileChange) -> AppResult<()>;
```

`revert_file` 方向 `after -> before`；`restore_file` 方向 `before -> after`。

- [ ] **Step 4: metadata 状态函数**

新增：

```rust
pub fn mark_file_reverted(&self, run_id: &str, file_path: &str, reverted: bool) -> AppResult<RunEditEntry>;
```

函数更新目标文件状态；`entry.reverted = entry.files.iter().all(|f| f.reverted)`；返回更新后的 entry 供 Tauri emit/store 使用。

- [ ] **Step 5: 跑绿测**

Run: `cargo test -p agent-core edits::tests::revert_and_restore_single_file_only --lib`

Expected: PASS。

### Task 3: Tauri command 与前端 API

**Files:**
- Modify: `apps/desktop/src/lib.rs`
- Modify: `apps/desktop/frontend/src/desktop/bridge/tauri.ts`
- Modify: `apps/desktop/frontend/src/desktop/ui/store/useStore.ts`

- [ ] **Step 1: 新增 commands**

在 `lib.rs` 新增：

```rust
#[tauri::command]
async fn revert_edit_file(app: AppHandle, session_id: String, run_id: String, file_path: String) -> AppResult<RevertResult>;

#[tauri::command]
async fn restore_edit_file(app: AppHandle, session_id: String, run_id: String, file_path: String) -> AppResult<RevertResult>;
```

并注册到 `invoke_handler`。

- [ ] **Step 2: 前端桥接**

`tauri.ts` 增加：

```ts
revertEditFile: (sessionId: string, runId: string, filePath: string) =>
  invoke<RevertResult>("revert_edit_file", { sessionId, runId, filePath }),
restoreEditFile: (sessionId: string, runId: string, filePath: string) =>
  invoke<RevertResult>("restore_edit_file", { sessionId, runId, filePath }),
```

- [ ] **Step 3: store 状态更新**

`useStore.ts` 增加两个 action，成功后更新对应 `TurnFileChange.reverted` 和 run 汇总 `reverted`。

### Task 4: Sidebar UI 与滚动保持

**Files:**
- Modify: `apps/desktop/frontend/src/desktop/ui/components/EditTreePanel.tsx`
- Modify: `apps/desktop/frontend/src/desktop/ui/components/DiffPanel.tsx`
- Modify: `apps/desktop/frontend/src/desktop/ui/components/ChatView.tsx`
- Create: `apps/desktop/frontend/src/desktop/ui/lib/chatScrollPosition.ts`
- Test: `apps/desktop/frontend/src/desktop/ui/lib/chatScrollPosition.test.mjs`

- [ ] **Step 1: 文件卡按钮**

每个文件卡片右侧显示两个圆形按钮：
- `Undo2`：文件未回退时可点，调用 `revertEditFile`
- `Redo2`：文件已回退时可点，调用 `restoreEditFile`

按钮文案：`回退这个文件` / `恢复这个文件`。

- [ ] **Step 2: diff 头分层**

`DiffViewer` 增加 `hideHeaderMeta?: boolean`；sidebar 调用传 `hideHeaderMeta`，避免 `文件名 / 修改 / 本次净变化 / +N / -N / 行内` 和文件标题挤在一起。

- [ ] **Step 3: chat 滚动保持**

`ChatView` 对消息滚动容器挂 `ResizeObserver`：如果 `stickToBottomRef.current` 为 true，容器尺寸变化后 `scrollTop = scrollHeight - clientHeight`；否则不动。

- [ ] **Step 4: 跑前端验证**

Run: `node frontend/src/desktop/ui/lib/chatScrollPosition.test.mjs && pnpm exec tsc --noEmit`（在 `apps/desktop` 目录）

Expected: PASS。

### Task 5: changelog 与完整验证

**Files:**
- Modify: `docs/changelog.md`

- [ ] **Step 1: 追加 changelog**

记录 Why、改动列表、影响范围（protocol + agent-core + desktop frontend）、兼容性（旧 metadata 字段 default 兼容）、留尾巴。

- [ ] **Step 2: 完整验证**

Run:

```bash
cargo check --workspace
cargo check -p agent-core --tests
cargo test -p agent-core --lib
cd apps/desktop && pnpm exec tsc --noEmit
```

Expected: 全部通过。
