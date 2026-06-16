import {
  isSessionCompacting,
  shouldApplyCompactionResult,
} from "./compactingState.ts";

function check(name: string, actual: unknown, expected: unknown) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${name}: expected ${e}, got ${a}`);
  }
}

// ── isSessionCompacting ──

// 回归核心：压缩 A 会话时切到 B，B 的输入框不该被判定为 compacting
// （旧实现是全局单标志 / 残留 sending，B 会被错误禁用）。
check("压缩A切到B不禁用B", isSessionCompacting("A", "B"), false);

// 发起压缩的会话本身：compacting。
check("发起会话自身compacting", isSessionCompacting("A", "A"), true);

// 没有会话在压缩：任何会话都不 compacting。
check("无压缩任何会话false", isSessionCompacting(null, "A"), false);

// 没有打开任何会话。
check("无当前会话false", isSessionCompacting("A", null), false);

// ── shouldApplyCompactionResult ──

// 压缩完成时仍停留在发起会话：回填。
check("停留发起会话回填", shouldApplyCompactionResult("A", "A"), true);

// 压缩耗时里切到了别的会话：不回填（否则把 A 的数据覆盖到 B）。
check("切走后不回填", shouldApplyCompactionResult("A", "B"), false);

// 切到无会话状态：不回填。
check("切到空不回填", shouldApplyCompactionResult("A", null), false);

console.log("all passed");
