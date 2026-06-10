import { useEffect, useMemo, useState } from "react";
import { Send, X } from "lucide-react";
import { useStore } from "@/desktop/ui/store/useStore";
import { getBrowserHost } from "@/desktop/ui/lib/browserHost";
import {
  buildAnnotationMessage,
  elementBadge,
  type HebElementSnapshot,
  type StyleDiffEntry,
} from "@/desktop/ui/lib/annotation";

// 样式参数编辑器分组（架构 §5.3）。每项一行控件，改动经 host.applyStyle 实时写回页面。
const NUMERIC_PX = new Set([
  "font-size",
  "border-radius",
  "border-width",
  "letter-spacing",
  "margin-top",
  "padding-top",
  "gap",
]);
const FONT_WEIGHTS = ["300", "400", "500", "600", "700", "800"];
const TEXT_ALIGNS = ["left", "center", "right", "justify"];

interface StyleField {
  prop: string;
  label: string;
  kind: "number" | "color" | "select";
  options?: string[];
}

const STYLE_GROUPS: { title: string; fields: StyleField[] }[] = [
  {
    title: "文字",
    fields: [
      { prop: "font-size", label: "字号", kind: "number" },
      { prop: "font-weight", label: "字重", kind: "select", options: FONT_WEIGHTS },
      { prop: "color", label: "文字颜色", kind: "color" },
      { prop: "text-align", label: "对齐", kind: "select", options: TEXT_ALIGNS },
      { prop: "letter-spacing", label: "字距", kind: "number" },
    ],
  },
  {
    title: "边框背景",
    fields: [
      { prop: "border-radius", label: "圆角", kind: "number" },
      { prop: "border-width", label: "边框宽度", kind: "number" },
      { prop: "border-color", label: "边框颜色", kind: "color" },
      { prop: "background-color", label: "背景色", kind: "color" },
    ],
  },
  {
    title: "盒模型",
    fields: [
      { prop: "margin-top", label: "上外边距", kind: "number" },
      { prop: "padding-top", label: "上内边距", kind: "number" },
      { prop: "gap", label: "间距", kind: "number" },
    ],
  },
];

/** "16px" → "16"；"rgb(0,0,0)" 原样。数字控件取数值部分。 */
function numericValue(raw: string): string {
  const m = raw.match(/^(-?\d+(?:\.\d+)?)/);
  return m ? m[1] : "";
}

/** 颜色控件需要 #rrggbb；computedStyle 多为 rgb()，转一下兜底白。 */
function toHexColor(raw: string): string {
  const m = raw.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  if (!m) return /^#[0-9a-f]{6}$/i.test(raw.trim()) ? raw.trim() : "#000000";
  const hex = (n: string) => Number(n).toString(16).padStart(2, "0");
  return `#${hex(m[1])}${hex(m[2])}${hex(m[3])}`;
}

export function AnnotationCard({
  snapshot,
  anchorRect,
  onClose,
}: {
  snapshot: HebElementSnapshot;
  /** 选中元素在主窗口中的屏幕矩形（占位 div offset + snapshot.rect 换算后） */
  anchorRect: { left: number; top: number; width: number; height: number };
  onClose: () => void;
}) {
  const host = getBrowserHost();
  const sendUserMessage = useStore((s) => s.sendUserMessage);
  const hasSession = useStore((s) => !!s.currentSession);
  const [comment, setComment] = useState("");
  const [sending, setSending] = useState(false);
  // 本地维护用户调过的值（受控控件显示用）；真实 diff 由 inspector 侧累积，提交时取回。
  const [edited, setEdited] = useState<Record<string, string>>({});

  // 卡片定位：锚点右下方，避免遮住元素本身；超出视口则翻到左/上侧。
  const style = useMemo(() => {
    const margin = 8;
    const cardW = 320;
    let left = anchorRect.left;
    let top = anchorRect.top + anchorRect.height + margin;
    if (left + cardW > window.innerWidth - 12) left = Math.max(12, window.innerWidth - cardW - 12);
    if (top + 420 > window.innerHeight - 12) top = Math.max(12, anchorRect.top - 420 - margin);
    return { left, top, width: cardW } as const;
  }, [anchorRect]);

  useEffect(() => {
    // 切换选中元素时清空本地编辑态
    setEdited({});
    setComment("");
  }, [snapshot.selectorPath, snapshot.capturedAt]);

  const applyStyle = (prop: string, value: string) => {
    setEdited((prev) => ({ ...prev, [prop]: value }));
    void host.applyStyle(prop, value);
  };

  const close = (revert: boolean) => {
    if (revert) void host.revertStyles();
    void host.clearSelection();
    onClose();
  };

  const submit = async () => {
    if (sending || !hasSession) return;
    setSending(true);
    try {
      // 从 inspector 取回权威 styleDiff（含 before 原值），再组装消息。
      const diff = await collectStyleDiff(host);
      const { content, attachments } = buildAnnotationMessage({ snapshot, comment, styleDiff: diff });
      await sendUserMessage(content, attachments);
      void host.clearSelection();
      onClose();
    } finally {
      setSending(false);
    }
  };

  const currentValue = (field: StyleField): string =>
    edited[field.prop] ?? snapshot.computedStyles[field.prop] ?? "";

  return (
    <div
      className="fixed z-[60] flex max-h-[420px] flex-col overflow-hidden rounded-xl border border-border bg-popover text-popover-foreground shadow-xl"
      style={style}
    >
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-foreground" title={snapshot.selectorPath}>
          {elementBadge(snapshot)}
        </span>
        <button
          type="button"
          onClick={() => close(true)}
          className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
          title="取消"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>

      <textarea
        value={comment}
        onChange={(e) => setComment(e.target.value)}
        placeholder="描述这些更改…"
        rows={2}
        className="resize-none border-b border-border bg-transparent px-3 py-2 text-[13px] outline-none placeholder:text-muted-foreground"
      />

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2">
        {STYLE_GROUPS.map((group) => (
          <div key={group.title} className="mb-3">
            <div className="mb-1 text-[11px] font-semibold text-muted-foreground">{group.title}</div>
            <div className="flex flex-col gap-1.5">
              {group.fields.map((field) => (
                <StyleRow
                  key={field.prop}
                  field={field}
                  raw={currentValue(field)}
                  onChange={(v) => applyStyle(field.prop, v)}
                />
              ))}
            </div>
          </div>
        ))}
      </div>

      <div className="flex items-center justify-end gap-2 border-t border-border px-3 py-2">
        <button
          type="button"
          onClick={() => close(true)}
          className="rounded-md px-2.5 py-1 text-[12px] text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          取消
        </button>
        <button
          type="button"
          onClick={submit}
          disabled={sending || !hasSession}
          className="inline-flex items-center gap-1 rounded-md bg-primary px-2.5 py-1 text-[12px] font-medium text-primary-foreground disabled:opacity-50"
          title={hasSession ? "发送到对话" : "先打开一个对话"}
        >
          <Send className="h-3 w-3" />
          发送到对话
        </button>
      </div>
    </div>
  );
}

function StyleRow({
  field,
  raw,
  onChange,
}: {
  field: StyleField;
  raw: string;
  onChange: (value: string) => void;
}) {
  return (
    <label className="flex items-center gap-2 text-[12px]">
      <span className="w-16 shrink-0 text-muted-foreground">{field.label}</span>
      {field.kind === "number" && (
        <span className="flex flex-1 items-center gap-1">
          <input
            type="number"
            value={numericValue(raw)}
            onChange={(e) => onChange(NUMERIC_PX.has(field.prop) ? `${e.target.value}px` : e.target.value)}
            className="w-full rounded border border-border bg-background px-2 py-0.5 outline-none focus:border-primary"
          />
          <span className="text-[10px] text-muted-foreground">px</span>
        </span>
      )}
      {field.kind === "color" && (
        <input
          type="color"
          value={toHexColor(raw)}
          onChange={(e) => onChange(e.target.value)}
          className="h-6 w-full rounded border border-border bg-background"
        />
      )}
      {field.kind === "select" && (
        <select
          value={raw.trim()}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded border border-border bg-background px-2 py-0.5 outline-none focus:border-primary"
        >
          <option value="">—</option>
          {field.options!.map((opt) => (
            <option key={opt} value={opt}>
              {opt}
            </option>
          ))}
        </select>
      )}
    </label>
  );
}

/** 请求 inspector 回吐 styleDiff，等一次事件回来（带超时兜底）。 */
function collectStyleDiff(host: ReturnType<typeof getBrowserHost>): Promise<StyleDiffEntry[]> {
  return new Promise((resolve) => {
    let settled = false;
    let unlisten: (() => void) | null = null;
    const done = (diff: StyleDiffEntry[]) => {
      if (settled) return;
      settled = true;
      if (unlisten) unlisten();
      resolve(diff);
    };
    void host.onStyleDiff((diff) => done(diff)).then((fn) => {
      unlisten = fn;
      void host.requestStyleDiff();
    });
    setTimeout(() => done([]), 800);
  });
}
