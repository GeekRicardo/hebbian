import type {
  BackgroundTaskInfo,
  Message,
  SessionBackgroundReport,
} from "@/desktop/ui/types";
import { extractBgTaskId, extractSubagentTaskId } from "./bgTaskId";

/**
 * 右侧 sidebar「后台任务」列表的一条派生项。
 * 数据源单一化：完成的 Bash task / 已唤醒的 ScheduleWakeup 都从 session.messages 派生，
 * 永久保留；实时状态（运行中的输出 / 倒计时）再用注册表 report join。
 */
export interface TaskItem {
  /** 任务类型：Bash 后台 shell / ScheduleWakeup 定时唤醒 / 后台 subagent */
  kind: "bash" | "cron" | "subagent";
  /** 注册表 task_id；某些异常 case 模型 result 还没 parse 出来时为 null。cron 恒 null */
  task_id: string | null;
  /** 对应 tool_call.id，用于在 chat 区滚动定位 */
  tool_call_id: string;
  /** 对应 message.id，用于 `[data-message-id="..."]` 锚点跳转 */
  message_id: string;
  command: string;
  status: "running" | "exited" | "killed" | "failed" | "unknown";
  /** 注册表里的实时元信息（如果还在）。仅 bash */
  shell?: BackgroundTaskInfo;
  /** 最终 tool result 文本 */
  result?: string | null;
  duration_ms?: number | null;
  /** cron 专属：唤醒原因 / 触发时刻(ms) / 是否还在等 */
  cron?: {
    reason: string;
    fireAtMs: number;
    pending: boolean;
  };
}

/**
 * 从 session.messages 派生历史 + 用注册表 join 实时状态。
 * messages 是 source of truth：完成的 task 永远在 messages 里，不依赖注册表保留。
 * ScheduleWakeup 同理从 messages 派生——cron 到点后会被 scheduler 从 pending 表移除，
 * 但 tool_call 永久留在 messages，据此保留卡片（实时倒计时按 reason join pending_crons）。
 */
export function deriveBackgroundTasks(
  messages: Message[],
  report: SessionBackgroundReport | null
): TaskItem[] {
  const shellsByTaskId = new Map<string, BackgroundTaskInfo>();
  for (const s of report?.shells ?? []) {
    // 只展示真后台任务（is_background=true）。前台运行中的 Bash 由 Bash 工具卡片的
    // kill 按钮处理，不在此面板显示。
    if (!s.is_background) continue;
    shellsByTaskId.set(s.task_id, s);
  }
  // ScheduleWakeup 实时倒计时：按 reason join 当前还在等的 cron。
  // 前端 Message 不带 run_id，而 cron 串行（一个 run 同时只有一条 AwaitingCron），
  // pending 表通常 0/1 条，按 reason 匹配足够区分"还在等 vs 已唤醒"。
  const pendingCronByReason = new Map<
    string,
    NonNullable<SessionBackgroundReport["pending_crons"]>[number]
  >();
  for (const c of report?.pending_crons ?? []) {
    pendingCronByReason.set(c.reason, c);
  }
  const consumed = new Set<string>();
  const finishedSubagents = new Set<string>();
  for (const m of messages) {
    if (m.role === "user" && m.meta?.type === "system_notification" && m.meta.kind === "bg_task_finished" && m.meta.task_id) {
      finishedSubagents.add(m.meta.task_id);
    }
  }
  const items: TaskItem[] = [];

  // 1. 从 messages 找历史 Bash bg task（含前台超时转后台的）+ ScheduleWakeup
  for (const m of messages) {
    for (const tc of m.tool_calls ?? []) {
      if (tc.name === "ScheduleWakeup") {
        const input = (tc.input as Record<string, any> | undefined) ?? {};
        const reason =
          typeof input.reason === "string" ? input.reason : "(无说明)";
        const delaySecs =
          typeof input.delay_secs === "number" ? input.delay_secs : 0;
        const pending = pendingCronByReason.get(reason);
        // 唤醒时刻：还在等用 scheduler 的 fire_at_ms（精确）；已触发用
        // tool_call 产出时刻 + delay 推算（messages 永久留存，无需后端）。
        const fireAtMs = pending
          ? pending.fire_at_ms
          : m.created_at + delaySecs * 1000;
        items.push({
          kind: "cron",
          task_id: null,
          tool_call_id: tc.id,
          message_id: m.id,
          command: reason,
          status: pending ? "running" : "exited",
          result: tc.result,
          duration_ms: tc.duration_ms,
          cron: { reason, fireAtMs, pending: !!pending },
        });
        continue;
      }
      if (tc.name === "Task") {
        const input = (tc.input as Record<string, any> | undefined) ?? {};
        const result = tc.result ?? "";
        const taskId = extractSubagentTaskId(result);
        if (!taskId) continue;
        items.push({
          kind: "subagent",
          task_id: taskId,
          tool_call_id: tc.id,
          message_id: m.id,
          command: typeof input.subagent_type === "string" ? input.subagent_type : "subagent",
          status: finishedSubagents.has(taskId) ? "exited" : "running",
          result: tc.result,
          duration_ms: tc.duration_ms,
        });
        continue;
      }
      if (tc.name !== "Bash") continue;
      const input = (tc.input as Record<string, any> | undefined) ?? {};
      const explicit = input.run_in_background === true;
      const result = tc.result ?? "";
      const taskId = extractBgTaskId(result);
      // 仅前台正常结束的 Bash 不该出现（没 task_id 且 explicit=false）
      if (!explicit && !taskId) continue;
      const shell = taskId ? shellsByTaskId.get(taskId) : undefined;
      if (taskId) consumed.add(taskId);
      const status: TaskItem["status"] = shell
        ? (shell.state as TaskItem["status"])
        : tc.result
          ? "exited"
          : "running";
      items.push({
        kind: "bash",
        task_id: taskId,
        tool_call_id: tc.id,
        message_id: m.id,
        command: typeof input.command === "string" ? input.command : "(无命令)",
        status,
        shell,
        result: tc.result,
        duration_ms: tc.duration_ms,
      });
    }
  }
  // 2. 注册表有但 messages 还没记到的（task 刚启动 / tool_result 还没回来 / 上次会话残留）
  for (const s of report?.shells ?? []) {
    if (!s.is_background) continue;
    if (consumed.has(s.task_id)) continue;
    items.push({
      kind: "bash",
      task_id: s.task_id,
      tool_call_id: `pending-${s.task_id}`,
      message_id: "",
      command: s.command,
      status: s.state as TaskItem["status"],
      shell: s,
    });
  }
  // 3. 排序：running 优先（按 elapsed_secs 升序新的在前）；其他保持 messages 时序
  const runningItems = items.filter((it) => it.status === "running");
  const otherItems = items.filter((it) => it.status !== "running");
  runningItems.sort((a, b) => {
    const ae = a.shell?.elapsed_secs ?? 0;
    const be = b.shell?.elapsed_secs ?? 0;
    return ae - be;
  });
  return [...runningItems, ...otherItems];
}
