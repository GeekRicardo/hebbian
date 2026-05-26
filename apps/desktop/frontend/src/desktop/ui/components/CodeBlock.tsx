import { useState, type ReactNode } from "react";
import { Check, Copy } from "lucide-react";
import { toast } from "sonner";

/**
 * Markdown `<pre>` 渲染：右上角浮一个复制按钮，hover 出现。
 *
 * 抽自原 MessageBubble.tsx，被 [`MarkdownRenderer`] 用做 ReactMarkdown 的
 * `components.pre`。所有 assistant 文本 / plan markdown / plan 评论里的代码块都共用。
 */
export function CodeBlock({
  children,
  ...rest
}: React.HTMLAttributes<HTMLPreElement>) {
  const [copied, setCopied] = useState(false);
  const code = extractText(children).replace(/\n$/, "");

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败");
    }
  }

  return (
    <div className="group/code relative">
      <pre {...rest}>{children}</pre>
      <button
        type="button"
        onClick={handleCopy}
        title="复制代码"
        className="absolute right-2 top-2 inline-flex items-center gap-1 rounded border border-border bg-background/80 px-1.5 py-1 text-xs text-muted-foreground opacity-0 transition-opacity hover:bg-background group-hover/code:opacity-100"
      >
        {copied ? (
          <Check className="h-3.5 w-3.5 text-emerald-500" />
        ) : (
          <Copy className="h-3.5 w-3.5" />
        )}
      </button>
    </div>
  );
}

/** 把 React 节点树里的纯文本拼出来——给"复制"按钮用。 */
export function extractText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (typeof node === "object" && "props" in node) {
    return extractText(
      (node as React.ReactElement<{ children?: ReactNode }>).props.children
    );
  }
  return "";
}
