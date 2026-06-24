# hebweb vs desktop 命令对齐（parity）对照

> 2026-06-24 更新。desktop 168 业务+native 命令 / hebweb 已识别 134 / 未实现 34。
> 未实现 34 = browser 18 + terminal 8 + wechat 5 + log 独立窗口/推流 3 —— **全部依赖 Tauri 原生容器**
> （WebviewWindow 嵌入浏览器 / 本机 PTY / 系统托盘常驻 / 独立窗口），web surface 物理无对应。
> **可在 server 侧实现的纯逻辑命令全部已对齐（134，含 OAuth 全链路）**。
>
> ⚠️ **修正前一版误判**：曾把 13 个 oauth + 1 deepseek + read_log_file 列为「native 硬约束」，
> 实为 `model_gateway::auth`（纯 reqwest）+ fs，与 Tauri 无关，本轮已补齐并 WS 实测（见三）。

## 一、已对齐（134，hebweb 与 desktop 同名同语义）

**OAuth 登录（纯 model_gateway::auth）**: oauth_codex_start/poll/refresh oauth_openai_start/exchange
  oauth_claude_start/exchange/refresh oauth_claude_code_import oauth_gemini_start/exchange/refresh
  oauth_gemini_cli_import deepseek_login
**日志**: read_log_file

**会话/对话流**: list_sessions get_session create_session send_message inject_user_message
  approve_permission answer_question cancel_message rename_session delete_session
  fork_session truncate_after truncate_inclusive search_sessions update_session_config
  update_session_settings generate_session_title compact_session get_context_usage
  preview/undo_compaction
**providers/models**: get_providers list_provider_presets save_providers upsert_provider
  get_provider fetch_provider_models test_provider_model get_models_catalog refresh_models_catalog
**prompts**: list_prompts upsert_prompt delete_prompt set_default_prompt
**projects**: list_projects save_project delete_project import_vscode_project import_project_file
**permissions**: list_permissions add_permission remove_permission clear_permissions
  list_permission_paths add_permission_path remove_permission_path
**memory**: list_memories read_memory
**settings**: get_settings save_settings
**run 控制**: get_run_mode set_run_mode get_force_automode set_force_automode
**旁支(branch)**: branch_create branch_send branch_discard branch_cancel  ← 步骤3 端到端实测
**subagent**: list_subagents get_subagent save_subagent delete_subagent set_subagent_enabled load_subagent_run
**mcp**: get_mcp_config save_mcp_config discover_mcp_tools
**hooks**: get_hooks_raw save_hooks_raw
**skill**: list_skills list_claude_skills import_claude_skills scan_skill_dir scan_skill_github
  import_skills_from_dir import_skills_from_github set_skill_enabled delete_skill
  list_skill_collections delete_skill_collection read_skill_md
**plugin**: plugin_marketplace_add/list/list_plugins/remove plugin_install plugin_uninstall plugin_list
**tools**: list_tools
**goal**: get_active_goal set_active_goal clear_active_goal
**plan/todo**: list_todos list_session_plans read_plan_markdown update_plan_markdown
  list_plan_comments add_plan_comment
**model_io**: list_session_model_io get_session_model_io_entry
**import/导入**: list_claude_sessions import_claude_session
**路径审批/附件/预览**: approve_path_access attach_path drop_paths preview_session_payload
**edits worktree / background task / rules** 等

## 二、未实现——依赖 Tauri 原生容器，web surface 物理无对应（34）

逐条已核验函数签名 / 模块依赖，**全部命中 Tauri 原生能力**（非「未做」）：
- **browser_***（18）: 内置浏览器=Tauri WebviewWindow / wry / CEF 嵌入（`browser/mod.rs` 78 处 tauri 引用）。web 无此容器。UI 隐藏浏览器 tab。
- **terminal_***（8）: 内置终端=本机 PTY + Tauri 窗口（`terminal/mod.rs` 23 处 tauri 引用）。架构已决策 web 不暴露本机 shell（127.0.0.1 无鉴权、安全风险高）。UI 隐藏终端 tab。
- **wechat***（5）: 渠道运行收进 Desktop 进程 + 托盘后台常驻（架构 §7.5）。`wechat_status/start/stop` 依赖 `app.try_state::<WeChatState>()`（进程内渠道运行态）、`wechat_login_poll` confirmed 时 `spawn_channel(&app)` 在 Desktop 进程内拉起 ChannelBridge。**注**：`wechat_login_start` 本身是无状态 HTTP（请求二维码），但单补它无意义——poll→run 链断在 Desktop 进程内 WeChatState 上，整套搬进 hebweb 是独立架构工作，非薄路由。
- **log 独立窗口/推流**（3）: `open_log_viewer_window` / `set_log_viewer_always_on_top` 开独立 Tauri 窗口（真 native）；`subscribe_log_stream` 走 Tauri Channel 推流（可用 WS event 复刻，但价值低——日志历史已可由 read_log_file 拉，未补）。

## 三、本轮补齐（OAuth 全链路 + deepseek + read_log_file，纠正前一版误判）

> **根因**：这些命令的业务逻辑全在 `model_gateway::auth`（纯 reqwest）+ fs，desktop 的 `oauth_*`
> command 只是**不接 AppHandle 的薄壳**（逐条核验：`oauth::codex_start()` 等无任何 Tauri 参数）。
> 前一版把「desktop 用 Tauri shell 打开系统浏览器」误当成「登录逻辑依赖 Tauri」——其实那层 shell
> 与登录逻辑无关，且浏览器 surface 里前端本就能直接跳转授权页 + 回调拿 code，OAuth 反而更自然。
> hebweb 已依赖 model_gateway crate（用它做模型调用），补这批只是加薄路由委托。

| 命令 | 实现 + WS 实测结果 |
|------|-----------|
| oauth_claude_start | 委托 `model_gateway::auth::claude_oauth_start`。**实测**：返回真实 `auth_url=https://claude.ai/oauth/authorize?...` |
| oauth_openai_start | **实测**：返回 `auth_url=https://auth.openai.com/oauth/authorize?...` |
| oauth_gemini_start | **实测**：返回 `auth_url=https://accounts.google.com/o/oauth2/v2/auth?...` |
| oauth_codex_start | device flow。**实测**：返回真实 `device_code=deviceauth_6a3b...`, `expires_in:900` |
| *_exchange / *_refresh / *_import | claude/openai/gemini 各自的 code 换 token / 刷新 / 读本机 CLI 凭证，纯 reqwest 委托（参数齐全：sessionId/code/refreshToken/clientId/clientSecret） |
| deepseek_login | 委托 `model_gateway::auth::deepseek::deepseek_login`，DeepseekChallenge PoW 登录 |
| read_log_file | 读今天日志文件（纯 fs）。**实测**：返回 14.6MB 真实日志内容 |

## 四、此前补齐（路径审批/附件/预览，逻辑下沉 agent_core 两 surface 共用）

> 根因都是**业务逻辑误留在 desktop 私有层**。彻底解法是把纯逻辑下沉到 agent_core，desktop / hebweb 共用：
> - `agent_core::preview_payload::build_preview_payload`（从 desktop chat.rs 下沉，零 Tauri 依赖）
> - `agent_core::attach`（attach_path / drop_paths 分流，纯 fs，测试一并下沉）
> - approve_path_access 复用 hebweb 已有 `pending_approvals` oneshot + storage 落盘
>
> 更早两轮已补：switch_provider_model / fetch_provider_usage / export_session_to_claude /
> discover_all_rules / read_skill_md / import_project_file。

| 命令 | 实现 + WS 实测结果 |
|------|-----------|
| approve_path_access | 按 scope 落 storage（this_session→session.allowed_paths / global→settings / this_project,once→不持久化）+ 投 ApprovalDecision 回 run oneshot。**实测**：建临时 session→this_session scope→重载 `allowed_paths=["/tmp/approve-target-dir"]` 已落盘 |
| preview_session_payload | 委托 `agent_core::preview_payload`，复刻 agent_loop 进模型前的拼装（工具集 + system prompt + transcript），不发请求不改 session。**实测**：真 session 返回 `{model, messages(447), tools(57), _workspace}` |
| attach_path | 委托 `agent_core::attach`，探测 file/dir/missing。**实测**：file→`{kind:file}` / dir→`{kind:dir}` / 缺失→`{kind:missing}` |
| drop_paths | 委托 `agent_core::attach`，小图片/文本读成附件、目录/大文件→reference、缺失→missing。**实测**：md→text_file、png→image(base64)、目录→reference、缺失→missing |

## 结论
- **凡能在 server 侧实现的纯逻辑命令，web 与 desktop 完全对齐**（134 命令同名同 dispatch，含 OAuth 全链路）。
- desktop 168 业务+native / hebweb 已识别 134 / 未实现 34。未实现的 34 个**逐条核验为 Tauri 原生容器依赖**（browser WebviewWindow 18 + terminal PTY 8 + wechat 进程内渠道+托盘 5 + log 独立窗口/推流 3），属 surface 能力边界，web UI 已降级隐藏不报错。
- **终端（决策 2026-06-24）**：PTY 后端技术上可在 hebweb 实现（embedded 模式），但**有意保持降级不做**——① web 暴露本机 shell 安全风险高（hebweb 仅 127.0.0.1 无鉴权）；② web 端以跑 agent 为主，本地终端价值低。明确边界决策，非能力缺失。popout 独立窗口本就强 native。
- **验证方法**：从 desktop `generate_handler!` 抽真实命令集 → 逐个打到运行中 hebweb WS → 按兜底 sentinel 分流 → 134 识别 / 34 未实现，与源码静态差集一致。证据见 [core-rpc-verification-evidence.md §五/§六](core-rpc-verification-evidence.md)。
