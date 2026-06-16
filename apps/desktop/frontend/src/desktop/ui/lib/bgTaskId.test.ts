// 后台 task_id 提取测试（node --experimental-strip-types bgTaskId.test.ts）。
//
// 回归锚点：旧 MessageBubble 正则 `\]\s+(?:background|后台)` 对当前文案
// `[bash_007] 已在后台启动` 全部 NO MATCH（"]" 后是"已在"不是"后台"），
// 导致 chat 卡片提不到 task_id、永远显示"无输出"。这里固化新 helper 必须命中。

import { extractBgTaskId } from "./bgTaskId.ts";

let failed = 0;
function eq(actual: unknown, expected: unknown, label: string) {
  const ok = actual === expected;
  if (!ok) failed++;
  console.log(`${ok ? "✓" : "✗"} ${label} => ${JSON.stringify(actual)}`);
}

// 当前文案
eq(extractBgTaskId("[bash_007] 已在后台启动"), "bash_007", "已在后台启动");
eq(
  extractBgTaskId("[bash_008] 60s 内未结束，已转后台\n--- 已产出 ---\ntick"),
  "bash_008",
  "超时转后台"
);
// 旧格式兼容
eq(extractBgTaskId("task_id=bash_001 cmd=ls"), "bash_001", "旧 task_id= 格式");
// 完成态聚合文本
eq(extractBgTaskId("[bash_003 exit 0]\noutput"), "bash_003", "完成态头");
// 不该误伤
eq(extractBgTaskId("[ScheduleWakeup] 已设置 60s 后唤醒"), null, "ScheduleWakeup 不匹配");
eq(extractBgTaskId(null), null, "null");
eq(extractBgTaskId(""), null, "空串");

if (failed > 0) {
  throw new Error(`${failed} 条断言失败`);
}
console.log("\n全部通过");
