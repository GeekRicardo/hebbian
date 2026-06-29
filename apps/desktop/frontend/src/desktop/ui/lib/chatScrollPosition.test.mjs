// Chat 滚动位置保持策略测试（inline node sanity check，项目惯例：前端无 vitest）。
// 跑法：node frontend/src/desktop/ui/lib/chatScrollPosition.test.mjs

import { stickyBottomScrollTop, anchorScrollTop } from "./chatScrollPosition.ts";

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

// ── anchorScrollTop：sidebar 重排时把锚点钉回原视觉位置 ──
// 锚点原本距容器顶 100px；重排后元素 offsetTop 从 500 漂到 800 → scrollTop 应为 800-100=700。
expectEqual(
  "锚点钉回原位（元素下移）",
  anchorScrollTop(
    { messageId: "m1", offsetFromTop: 100 },
    800,
    { scrollHeight: 5000, clientHeight: 600 },
  ),
  700,
);
// 锚点略滚出上方（offsetFromTop 负）：新 offsetTop=300 → 300-(-50)=350。
expectEqual(
  "锚点略滚出上方仍正确",
  anchorScrollTop(
    { messageId: "m2", offsetFromTop: -50 },
    300,
    { scrollHeight: 5000, clientHeight: 600 },
  ),
  350,
);
// clamp 上界：目标超过 maxScrollTop（4400）时夹到 4400。
expectEqual(
  "越界夹到底部",
  anchorScrollTop(
    { messageId: "m3", offsetFromTop: 0 },
    9999,
    { scrollHeight: 5000, clientHeight: 600 },
  ),
  4400,
);
// clamp 下界：目标为负时夹到 0。
expectEqual(
  "越界夹到顶部",
  anchorScrollTop(
    { messageId: "m4", offsetFromTop: 500 },
    100,
    { scrollHeight: 5000, clientHeight: 600 },
  ),
  0,
);

console.log("chatScrollPosition.test.mjs: all assertions passed");
