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

console.log("inspector.test.cjs: all assertions passed");
