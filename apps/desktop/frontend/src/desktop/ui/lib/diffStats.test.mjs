import { calculateDiffStats } from "./diffStats.ts";

function expectEqual(name, actual, expected) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${name}: expected ${e}, got ${a}`);
}

expectEqual(
  "统计新增和删除行数",
  calculateDiffStats("a\nb\nc", "a\nb2\nc\nd"),
  { addCount: 2, removeCount: 1 },
);
expectEqual(
  "纯新增文件按行统计",
  calculateDiffStats("", "x\ny"),
  { addCount: 2, removeCount: 1 },
);

console.log("diffStats.test.mjs: all assertions passed");
