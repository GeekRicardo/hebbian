import {
  applyToolStart,
  applyToolDone,
  applyToolCallDelta,
  applyTextDelta,
  applyTextDone,
} from "./streamingParts.ts";
import type { StreamingAssistantPart } from "../types.ts";

function check(name: string, got: unknown, exp: unknown) {
  const a = JSON.stringify(got), e = JSON.stringify(exp);
  if (a !== e) throw new Error(`${name}: expected ${e}, got ${a}`);
}
const tools = (p: StreamingAssistantPart[]) => p.filter((x) => x.type === "tool_call");

// 回归：非流式 provider（anthropic 带工具）多轮 run，tool_start/tool_done 的 index
// 全是 undefined、id 各不同。修前 toolPartIndex 回退按 `part.index === undefined`
// 匹配，命中上一轮工具 part，N 个工具全塌成 1 个互相覆盖。修后：index 缺失不回退，
// 各工具按 id 独立成卡片。
{
  let parts: StreamingAssistantPart[] = [];
  for (const id of ["toolu_A", "toolu_B", "toolu_C"]) {
    parts = applyToolStart(parts, { type: "tool_start", index: undefined, id, name: "Bash", input: {} } as never);
    parts = applyToolDone(parts, { type: "tool_done", index: undefined, id, result: "ok-" + id, duration_ms: 5 } as never);
  }
  check("非流式多轮 index=undefined 工具各自独立不覆盖", tools(parts).length, 3);
}

// 流式 delta：同一工具首个 chunk id 未到、只有有效 index，后续 chunk 带 id——
// 必须按 index 认领之前建的 part，不能分裂。
{
  let p: StreamingAssistantPart[] = [];
  p = applyToolCallDelta(p, { type: "tool_call_delta", index: 0, id: null, name: "Bash", arguments_delta: '{"a' } as never);
  p = applyToolCallDelta(p, { type: "tool_call_delta", index: 0, id: "toolu_X", name: "Bash", arguments_delta: '":1}' } as never);
  check("流式 delta id 后到不分裂", tools(p).length, 1);
  const call = tools(p)[0];
  check("流式 delta 参数拼接完整", call.type === "tool_call" ? call.arguments : "", '{"a":1}');
}

// 单 ModelStep 多并行工具（index 0/1/2 有效）：各自独立。
{
  let p: StreamingAssistantPart[] = [];
  for (let i = 0; i < 3; i++) p = applyToolCallDelta(p, { type: "tool_call_delta", index: i, id: null, name: "Bash", arguments_delta: "x" } as never);
  check("并行工具 index 0/1/2 各自独立", tools(p).length, 3);
}

// index 和 id 都缺：无法定位，一律新建（不互相覆盖）。
{
  let p: StreamingAssistantPart[] = [];
  for (let i = 0; i < 2; i++) p = applyToolStart(p, { type: "tool_start", index: undefined, id: null, name: "Bash", input: {} } as never);
  check("index/id 都缺各自独立", tools(p).length, 2);
}

// 文本累积基础：多段 text_delta 顺序拼接成一段。
{
  let p: StreamingAssistantPart[] = [];
  p = applyTextDelta(p, "abc");
  p = applyTextDelta(p, "def");
  const texts = p.filter((x) => x.type === "text");
  check("文本段合并", texts.length === 1 && texts[0].type === "text" ? texts[0].text : "", "abcdef");
}

// 回归：多轮 run 的 text_done 必须追加本轮、绝不覆盖累积。
// 非流式 end_turn 路径只 emit TextDone 没发过 TextDelta，本轮文本只在 fullText。
// 修前 `fullText.endsWith(streamingText)` 在多轮恒 false → streamingText 被单轮覆盖，
// 前面输出全消失（run 完 reload 才恢复）。
{
  // 多轮非流式：前几轮 text_delta 累积，末轮 end_turn 只 text_done。
  let st = "";
  let parts: StreamingAssistantPart[] = [];
  for (const seg of ["第1轮说明", "第2轮说明", "第3轮说明"]) {
    st += seg;
    parts = applyTextDelta(parts, seg);
  }
  const done = applyTextDone(st, parts, "总结：全部完成");
  check("text_done 多轮追加不覆盖", done.streamingText, "第1轮说明第2轮说明第3轮说明总结：全部完成");
  const txt = done.streamingParts.filter((x) => x.type === "text");
  check("text_done 追加进 parts", txt.length === 1 && txt[0].type === "text" ? txt[0].text : "", "第1轮说明第2轮说明第3轮说明总结：全部完成");
}
{
  // 流式：本轮文本已 delta 累积，text_done 同内容确认 → 不重复。
  let st = "";
  let parts: StreamingAssistantPart[] = [];
  st += "你好世界";
  parts = applyTextDelta(parts, "你好世界");
  const done = applyTextDone(st, parts, "你好世界");
  check("text_done 流式不重复", done.streamingText, "你好世界");
}
{
  // 非流式首轮空累积：streamingText 空，text_done 补全。
  const done = applyTextDone("", [], "直接回答");
  check("text_done 空累积补全", done.streamingText, "直接回答");
}

console.log("streamingParts.test.ts: all passed");
