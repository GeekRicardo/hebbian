// inspector.js 纯函数核心测试（node 直接跑：node apps/desktop/src/browser/inspector.test.cjs）。
// DOM 薄壳（picker/overlay/styler 的事件接线）不在此测——由 TDD §3.3 手动验收清单覆盖。
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert");

// inspector.js 所在包是 type:module，require(".js") 解析有歧义；用 new Function 在
// 当前 realm 执行源码取纯函数核心（DOM 全局缺失 → 走 module.exports 早返回分支）。
// 当前 realm 执行保证返回的数组/对象与本测试同 prototype，deepStrictEqual 才成立。
function loadCore() {
  const src = fs.readFileSync(path.join(__dirname, "inspector.js"), "utf8");
  const mod = { exports: {} };
  new Function("module", "exports", "window", src)(mod, mod.exports, undefined);
  return mod.exports;
}
const core = loadCore();

// ── truncate ──
assert.strictEqual(core.truncate("abc", 5), "abc");
assert.strictEqual(core.truncate("abcdef", 3), "abc…[截断]");

// ── buildSelectorPath：遇 id 锚点截断 ──
assert.strictEqual(
  core.buildSelectorPath([
    { tag: "BUTTON", id: "", classes: ["btn", "btn-primary", "x", "y"], nthChild: 2 },
    { tag: "DIV", id: "root", classes: [], nthChild: 1 },
    { tag: "BODY", id: "", classes: [], nthChild: 1 },
  ]),
  "div#root > button.btn.btn-primary.x:nth-child(2)"
);
// 无 id：全链拼接（类最多 3 个）
assert.strictEqual(
  core.buildSelectorPath([
    { tag: "SPAN", id: "", classes: [], nthChild: 3 },
    { tag: "MAIN", id: "", classes: ["page"], nthChild: 2 },
  ]),
  "main.page:nth-child(2) > span:nth-child(3)"
);

// ── buildXPath ──
assert.strictEqual(
  core.buildXPath([
    { tag: "BUTTON", nthOfType: 1 },
    { tag: "DIV", nthOfType: 2 },
  ]),
  "/div[2]/button[1]"
);

// ── summarizeProps：上限 20 项、每值 100 字符、children 跳过、不可序列化标记 ──
const bigProps = {};
for (let i = 0; i < 30; i++) bigProps[`k${i}`] = i;
const summarized = core.summarizeProps(bigProps);
assert.ok(Object.keys(summarized).length <= 20);
const circular = {};
circular.self = circular;
const withBad = core.summarizeProps({ ok: 1, bad: circular, children: "x" });
assert.strictEqual(withBad.ok, "1");
assert.strictEqual(withBad.bad, "[NOT SERIALIZABLE]");
assert.ok(!("children" in withBad));
const longVal = core.summarizeProps({ v: "x".repeat(300) });
assert.ok(longVal.v.length <= 100 + "…[截断]".length);

// ── componentChainFromFiber：函数/memo 包装/类，host 元素跳过，≤8 层 ──
function SaveBtn() {}
function Card() {}
const wrapped = { type: { render: function Inner() {} }, return: null };
const fiber = {
  type: "button", // host 元素跳过
  return: { type: SaveBtn, return: { type: Card, return: wrapped } },
};
assert.deepStrictEqual(core.componentChainFromFiber(fiber), ["SaveBtn", "Card", "Inner"]);
// 深链截断到 8
let deep = null;
for (let i = 0; i < 20; i++) {
  const fn = Object.defineProperty(function () {}, "name", { value: `C${i}` });
  deep = { type: fn, return: deep };
}
assert.strictEqual(core.componentChainFromFiber(deep).length, 8);

// ── nearestComponentProps ──
const propsFiber = {
  type: "div",
  return: { type: SaveBtn, memoizedProps: { label: "保存" }, return: null },
};
assert.deepStrictEqual(core.nearestComponentProps(propsFiber), { label: "保存" });

// ── capSnapshot：超限按序丢字段，必留字段始终在 ──
const fat = {
  url: "http://localhost:3000/",
  tagName: "div",
  selectorPath: "div#root",
  innerText: "x".repeat(2000),
  attributes: { long: "y".repeat(4000) },
  computedStyles: { color: "z".repeat(4000) },
  childrenSummary: new Array(10).fill("div"),
  react: { componentChain: ["A"], props: { p: "q".repeat(3000) } },
};
const capped = core.capSnapshot(fat);
assert.ok(JSON.stringify(capped).length <= core.MAX_SNAPSHOT_BYTES + 64);
assert.ok(capped.url && capped.tagName && capped.selectorPath);

// ── parseInMsg：信封校验 ──
assert.strictEqual(core.parseInMsg("not json"), null);
assert.strictEqual(core.parseInMsg({ type: "heb:picker:start" }), null); // 缺 source
assert.strictEqual(core.parseInMsg({ source: "evil", type: "x" }), null);
assert.deepStrictEqual(
  core.parseInMsg(JSON.stringify({ source: "hebbian-host", type: "heb:picker:start" })),
  { source: "hebbian-host", type: "heb:picker:start" }
);
assert.deepStrictEqual(
  core.parseInMsg({ source: "hebbian-host", type: "heb:style:apply", payload: { prop: "color", value: "red" } })
    .payload,
  { prop: "color", value: "red" }
);


// ── refToIndex：@N 引用解析（多元素注释框） ──
assert.strictEqual(core.refToIndex("@1"), 0);
assert.strictEqual(core.refToIndex("@3"), 2);
assert.strictEqual(core.refToIndex(" @2 "), 1);
assert.strictEqual(core.refToIndex("@0"), -1);
assert.strictEqual(core.refToIndex("x"), -1);
assert.strictEqual(core.refToIndex(""), -1);
assert.strictEqual(core.refToIndex(null), -1);

// ── composeAsideText：chip 还原成元素定位 ──
assert.strictEqual(
  core.composeAsideText([
    { type: "text", value: "让 " },
    { type: "ref", ref: "@1", locator: "button.btn" },
    { type: "text", value: " 和 " },
    { type: "ref", ref: "@2", locator: "div.card" },
    { type: "text", value: " 对齐" },
  ]),
  "让 「元素1: button.btn」 和 「元素2: div.card」 对齐"
);
assert.strictEqual(
  core.composeAsideText([{ type: "ref", ref: "@2", locator: "" }]),
  "「元素2」"
);
assert.strictEqual(core.composeAsideText([]), "");

// ── draftChatKey：对话恒定锚在 1 号元素，切激活元素不漂移 ──
// 回归：曾经 chat 区用激活元素 key、syncDraftToList 用 elements[0] key，两套不一致，
// 切到 2 号聊天后历史读不回。现在统一走 draftChatKey。
const multiDraft = {
  elements: [{ key: "el-1" }, { key: "el-2" }, { key: "el-3" }],
  activeIndex: 2, // 激活在 3 号
};
assert.strictEqual(core.draftChatKey(multiDraft), "el-1"); // 仍取 1 号，不随 activeIndex 变
assert.strictEqual(core.draftChatKey({ elements: [{ key: "only" }], activeIndex: 0 }), "only");
assert.strictEqual(core.draftChatKey({ elements: [] }), null);
assert.strictEqual(core.draftChatKey(null), null);

// ── findDraftElementIndex：去重比 DOM 引用 + selectorPath（React 重渲染兜底）──
const nodeA = { tag: "a" }, nodeB = { tag: "b" };
const draftEls = {
  elements: [
    { el: nodeA, snapshot: { selectorPath: "div#root > a:nth-child(1)" } },
    { el: nodeB, snapshot: { selectorPath: "div#root > b:nth-child(2)" } },
  ],
};
// 同一 DOM 引用 → 命中
assert.strictEqual(core.findDraftElementIndex(draftEls, nodeB, { selectorPath: "whatever" }), 1);
// DOM 引用变了（React 换节点）但 selectorPath 相同 → 仍命中，不重复加入
const nodeAReplaced = { tag: "a2" };
assert.strictEqual(
  core.findDraftElementIndex(draftEls, nodeAReplaced, { selectorPath: "div#root > a:nth-child(1)" }),
  0
);
// 全新元素 → -1
assert.strictEqual(
  core.findDraftElementIndex(draftEls, { tag: "c" }, { selectorPath: "div#root > c:nth-child(3)" }),
  -1
);
// 无 snapshot 也不崩
assert.strictEqual(core.findDraftElementIndex(draftEls, { tag: "x" }, null), -1);
assert.strictEqual(core.findDraftElementIndex(null, nodeA, null), -1);

// ── styleChangeLocate：对话流样式块的可持久化定位 ──
// 回归：样式改动块（PreviewStyle 渲染的绿卡片）曾经只是一次性 DOM，没存进对话消息
// 序列，切元素 / 重建卡片后整组消失、还原入口丢失。现在存 locate，重建时据此找回元素。
// selector 来源（target 非 @N）：直接用 target + allMatches
assert.deepStrictEqual(
  core.styleChangeLocate(".btn", true, null),
  { selector: ".btn", allMatches: true }
);
assert.deepStrictEqual(
  core.styleChangeLocate("div#root > a", false, null),
  { selector: "div#root > a", allMatches: false }
);
// @N 来源：用激活元素的 selectorPath 当 selector，allMatches 恒 false
assert.deepStrictEqual(
  core.styleChangeLocate("@1", false, { selectorPath: "div#root > button:nth-child(2)" }),
  { selector: "div#root > button:nth-child(2)", allMatches: false }
);
// @N 但拿不到 selectorPath → null（重建时无从定位，回填降级不渲染还原按钮）
assert.strictEqual(core.styleChangeLocate("@2", false, null), null);
assert.strictEqual(core.styleChangeLocate("@1", false, { selectorPath: "" }), null);

// ── resolveStyleEls：重建卡片时按 locate 重新查 DOM 找回元素 ──
// 用假 doc 注入（node 无 document）。allMatches → querySelectorAll；否则 querySelector。
const elX = { tag: "x" }, elY = { tag: "y" }, elZ = { tag: "z" };
const fakeDoc = {
  querySelector: function (sel) { return sel === ".one" ? elX : null; },
  querySelectorAll: function (sel) { return sel === ".many" ? [elX, elY, elZ] : []; },
};
assert.deepStrictEqual(core.resolveStyleEls({ selector: ".one", allMatches: false }, fakeDoc), [elX]);
assert.deepStrictEqual(core.resolveStyleEls({ selector: ".many", allMatches: true }, fakeDoc), [elX, elY, elZ]);
// 无匹配 → []
assert.deepStrictEqual(core.resolveStyleEls({ selector: ".nope", allMatches: false }, fakeDoc), []);
// locate 为 null / doc 缺失 / querySelector 抛错 → [] 不崩
assert.deepStrictEqual(core.resolveStyleEls(null, fakeDoc), []);
assert.deepStrictEqual(core.resolveStyleEls({ selector: ".one", allMatches: false }, null), []);
const throwDoc = { querySelector: function () { throw new Error("bad selector"); } };
assert.deepStrictEqual(core.resolveStyleEls({ selector: ":::", allMatches: false }, throwDoc), []);

console.log("inspector.test.cjs: all assertions passed");
