// 页面注释：选中元素快照类型 + 注释消息组装（架构 §6 / §5.4）。
// 这些类型与 inspector.js 采集逻辑共用同一份字段定义（snapshot.ts 那侧无法 import TS，
// 靠本文件做权威声明，inspector 产出需对齐）。

import type { MessageAttachment } from "@/desktop/ui/types";

export interface StyleDiffEntry {
  prop: string;
  before: string;
  after: string;
}

export interface ReactElementInfo {
  componentChain: string[];
  props: Record<string, string>;
  sourceHint?: string;
  /** dev 模式 _debugSource 精确源码位置 */
  source?: { file: string; line: number | null };
}

export interface ParentInfo {
  tagName: string;
  classList: string[];
  id?: string;
  /** 父容器布局（决定子元素怎么排——改对齐 / 间距 / 排列时用） */
  layout?: {
    display: string;
    flexDirection: string;
    justifyContent: string;
    alignItems: string;
    gap: string;
    gridTemplateColumns: string;
  };
  childCount?: number;
}

export interface HebElementSnapshot {
  url: string;
  viewport: { width: number; height: number };
  capturedAt: string;
  tagName: string;
  id?: string;
  classList: string[];
  selectorPath: string;
  xpath: string;
  attributes: Record<string, string>;
  ownText?: string;
  innerText?: string;
  react?: ReactElementInfo | null;
  boundingClientRect: { x: number; y: number; width: number; height: number };
  computedStyles: Record<string, string>;
  parent?: ParentInfo;
  /** 当前元素在父中的下标 + 同级元素摘要（改与其他元素关系用） */
  indexInParent?: number;
  siblings?: string[];
  childrenSummary?: string[];
}

/** 选中元素的简短人类标签：button.btn-primary ⟨SaveBtn⟩ */
export function elementBadge(snap: HebElementSnapshot): string {
  let base = snap.tagName;
  if (snap.id) base += `#${snap.id}`;
  else if (snap.classList.length) base += `.${snap.classList[0]}`;
  const comp = snap.react?.componentChain?.[0];
  return comp ? `${base} ⟨${comp}⟩` : base;
}

// 属性值：& " < > 全转义
function escapeAttr(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

// 元素文本：只转 & 和 <（XML 文本里 > 合法，保留它让组件链/选择器的箭头可读）
function escapeText(value: string): string {
  return value.replace(/&/g, "&amp;").replace(/</g, "&lt;");
}

/**
 * 注释 → 主对话 user message（content + attachments）。
 * 导语写成自然第一人称（架构 §5.4），不说"从内置浏览器注释"机器腔。
 * content 自描述，不依赖 system prompt 教学（保 prompt cache）。
 */
export function buildAnnotationMessage(input: {
  snapshot: HebElementSnapshot;
  comment: string;
  styleDiff: StyleDiffEntry[];
}): { content: string; attachments: MessageAttachment[] } {
  const { snapshot, comment, styleDiff } = input;
  const compChain = snapshot.react?.componentChain ?? [];
  const target = compChain.length
    ? `${snapshot.selectorPath}（React: ${[...compChain].reverse().join(" > ")}）`
    : snapshot.selectorPath;

  const lines: string[] = [];
  lines.push("我在页面预览里圈了个地方，想这样改：");
  lines.push("");
  lines.push(
    `<web_annotation url="${escapeAttr(snapshot.url)}" viewport="${snapshot.viewport.width}x${snapshot.viewport.height}">`
  );
  if (comment.trim()) lines.push(`  <comment>${escapeText(comment.trim())}</comment>`);
  lines.push(`  <target>${escapeText(target)}</target>`);
  if (styleDiff.length) {
    lines.push("  <style_changes>");
    for (const d of styleDiff) {
      lines.push(`    ${d.prop}: ${d.before} → ${d.after}`);
    }
    lines.push("  </style_changes>");
  }
  lines.push("</web_annotation>");
  lines.push("");
  if (styleDiff.length) {
    lines.push(
      "元素完整快照在附件 element.json。style_changes 里是我在预览上实时调过、确认了效果的精确值，改源码时请原样采用。"
    );
  } else {
    lines.push("元素完整快照在附件 element.json，请据此定位源码修改。");
  }

  const attachments: MessageAttachment[] = [
    {
      kind: "text_file",
      name: "element.json",
      media_type: "application/json",
      content: JSON.stringify(snapshot, null, 2),
    },
  ];

  return { content: lines.join("\n"), attachments };
}
