// 注释消息组装测试（node --experimental-strip-types annotation.test.ts）。
import { buildAnnotationMessage, elementBadge, type HebElementSnapshot } from "./annotation.ts";

function assert(name: string, cond: boolean) {
  if (!cond) throw new Error(`FAIL: ${name}`);
}

const snap: HebElementSnapshot = {
  url: "http://localhost:3000/settings",
  viewport: { width: 1280, height: 800 },
  capturedAt: "2026-06-10T00:00:00.000Z",
  tagName: "button",
  classList: ["btn", "btn-primary"],
  selectorPath: "div#root > button.btn.btn-primary",
  xpath: "/div[1]/button[1]",
  attributes: { type: "submit" },
  innerText: "保存",
  react: { componentChain: ["SaveBtn", "Card", "SettingsPage"], props: { label: '"保存"' } },
  boundingClientRect: { x: 10, y: 20, width: 80, height: 32 },
  computedStyles: { "border-radius": "6px", "font-weight": "400" },
  childrenSummary: [],
};

// badge
assert("badge 含组件名", elementBadge(snap) === "button.btn ⟨SaveBtn⟩");
assert(
  "badge 无 react 退化",
  elementBadge({ ...snap, react: null }) === "button.btn"
);

// 带 styleDiff
const withStyle = buildAnnotationMessage({
  snapshot: snap,
  comment: "改成右对齐，hover 加阴影",
  styleDiff: [
    { prop: "border-radius", before: "6px", after: "12px" },
    { prop: "font-weight", before: "400", after: "600" },
  ],
});
assert("导语第一人称", withStyle.content.startsWith("我在页面预览里圈了个地方"));
assert("含 web_annotation", withStyle.content.includes("<web_annotation"));
assert("target 含组件链(由外到内)", withStyle.content.includes("SettingsPage > Card > SaveBtn"));
assert("含 style_changes diff", withStyle.content.includes("border-radius: 6px → 12px"));
assert("提示原样采用", withStyle.content.includes("原样采用"));
assert("附件是 text_file", withStyle.attachments[0].kind === "text_file");
assert("附件名 element.json", withStyle.attachments[0].name === "element.json");
assert(
  "附件可解析回 snapshot",
  JSON.parse((withStyle.attachments[0] as { content: string }).content).selectorPath ===
    "div#root > button.btn.btn-primary"
);

// 无 styleDiff：导语改走"定位源码修改"
const noStyle = buildAnnotationMessage({ snapshot: snap, comment: "这块文案不对", styleDiff: [] });
assert("无 style 不含 style_changes", !noStyle.content.includes("<style_changes>"));
assert("无 style 提示定位源码", noStyle.content.includes("据此定位源码"));

// 空 comment 不产出空 comment 标签
const empty = buildAnnotationMessage({ snapshot: snap, comment: "   ", styleDiff: [] });
assert("空 comment 跳过标签", !empty.content.includes("<comment>"));

// XSS/转义：comment 里的尖括号被转义
const danger = buildAnnotationMessage({
  snapshot: { ...snap, url: 'http://x/"<a>' },
  comment: "<script>",
  styleDiff: [],
});
assert("comment 文本转义(<)", danger.content.includes("&lt;script>"));
assert("url 属性转义(含 > )", danger.content.includes("&quot;&lt;a&gt;"));

console.log("annotation.test.ts: all assertions passed");
