// Chat 滚动位置保持策略测试（inline node sanity check，项目惯例：前端无 vitest）。
// 跑法：node frontend/src/desktop/ui/lib/chatScrollPosition.test.mjs

import { stickyBottomScrollTop } from "./chatScrollPosition.ts";

function expectEqual(name, actual, expected) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${name}: expected ${e}, got ${a}`);
  }
}

expectEqual(
  "重排后贴底",
  stickyBottomScrollTop({ scrollHeight: 1800, clientHeight: 600 }),
  1200,
);
expectEqual(
  "内容不足一屏时归零",
  stickyBottomScrollTop({ scrollHeight: 420, clientHeight: 600 }),
  0,
);

console.log("chatScrollPosition.test.mjs: all assertions passed");
