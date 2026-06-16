// 后台任务列表派生测试
// 跑法：node --experimental-strip-types --import ./_register-ts.mjs backgroundTasks.test.ts
//
// 回归锚点（问题 2）：ScheduleWakeup 触发后会被 scheduler 从 pending_crons 移除，
// 旧实现 sidebar 只读 pending_crons → cron 卡片消失。修复后从 messages 派生，
// 已唤醒的 cron 仍必须出现在列表里（status=exited），据此固化"完成后保留"。

import { deriveBackgroundTasks } from "./backgroundTasks.ts";

let failed = 0;
function assert(cond: boolean, label: string) {
  if (!cond) failed++;
  console.log(`${cond ? "✓" : "✗"} ${label}`);
}

const t0 = 1_000_000_000_000;

// 场景：一条已唤醒的 ScheduleWakeup（pending_crons 已空）+ 一条运行中的后台 Bash
const messages: any[] = [
  {
    id: "m1",
    role: "assistant",
    created_at: t0,
    tool_calls: [
      {
        id: "tc-cron",
        name: "ScheduleWakeup",
        input: { delay_secs: 40, reason: "看后台进度" },
        result: "[ScheduleWakeup] 已设置 40s 后唤醒",
      },
      {
        id: "tc-bash",
        name: "Bash",
        input: { command: "sleep 300", run_in_background: true },
        result: "[bash_007] 已在后台启动",
      },
    ],
  },
];
const report: any = {
  shells: [
    {
      task_id: "bash_007",
      state: "running",
      command: "sleep 300",
      cwd: "/",
      elapsed_secs: 5,
      log_path: null,
      is_background: true,
    },
  ],
  pending_crons: [], // cron 已触发，scheduler 已移除——旧实现卡片会消失
  has_suspended_checkpoint: false,
};

const items = deriveBackgroundTasks(messages, report);
const cron = items.find((i) => i.kind === "cron");
const bash = items.find((i) => i.kind === "bash");

assert(!!cron, "cron 卡片在 pending_crons 为空时仍保留");
assert(cron?.status === "exited", "已唤醒 cron 状态为 exited");
assert(cron?.cron?.pending === false, "已唤醒 cron pending=false");
assert(cron?.cron?.reason === "看后台进度", "cron reason 透传");
assert(cron?.cron?.fireAtMs === t0 + 40 * 1000, "已唤醒 cron 用 created_at+delay 推算唤醒时刻");
assert(!!bash && bash.task_id === "bash_007", "后台 Bash 用统一 helper 提取到 task_id");
assert(bash?.status === "running", "运行中后台 Bash join 注册表状态为 running");
// 运行中的 Bash 排在已唤醒 cron 前面（running 优先）
assert(items[0].kind === "bash", "running 项排在已完成项之前");

// 场景 2：cron 还在等（pending_crons 有），用 scheduler 的精确 fire_at_ms
const items2 = deriveBackgroundTasks(messages, {
  ...report,
  pending_crons: [{ run_id: "r1", fire_at_ms: t0 + 99_000, seconds_remaining: 30, reason: "看后台进度" }],
});
const cron2 = items2.find((i) => i.kind === "cron");
assert(cron2?.cron?.pending === true, "等待中 cron pending=true");
assert(cron2?.cron?.fireAtMs === t0 + 99_000, "等待中 cron 用 scheduler fire_at_ms");

if (failed > 0) throw new Error(`${failed} 条断言失败`);
console.log("\n全部通过");
