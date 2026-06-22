import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { toast } from "sonner";
import { CodeBlock } from "./CodeBlock";
import { openLink } from "@/desktop/ui/lib/openLink";

/**
 * 公共 Markdown 渲染器。
 *
 * 抽自 MessageBubble.tsx 里历史的 `<ReactMarkdown remarkPlugins={[remarkGfm]}
 * components={markdownComponents}>` 包装，让 PlanTab / PlanApprovalDialog /
 * MessageBubble 共用一份配置。
 *
 * 用 `<div className="markdown-body">` 包裹给宿主样式接管（assistant 气泡 /
 * plan 面板等可以分别 scope CSS）。当前不内置任何样式，由调用方传 className 控制。
 */
export function MarkdownRenderer({
  markdown,
  className,
}: {
  markdown: string;
  className?: string;
}) {
  return (
    <div className={className}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
        {markdown}
      </ReactMarkdown>
    </div>
  );
}

/**
 * Markdown 里的链接统一拦截（架构 §8.5）：裸 <a> 在 Tauri webview 里点击会把整个
 * app 导航走，必须 preventDefault 改走 openLink，按设置去系统/内置浏览器。只拦
 * http(s)/file 等真实超链接；锚点（#xxx）等无 href 的交给浏览器默认行为。
 */
function MarkdownAnchor({ href, children, ...rest }: React.ComponentProps<"a">) {
  const open = (event: React.MouseEvent<HTMLAnchorElement>) => {
    if (!href) return;
    event.preventDefault();
    event.stopPropagation();
    void openLink(href).catch(() => toast.error("打开链接失败"));
  };
  return (
    <a {...rest} href={href} onClick={open} target="_blank" rel="noreferrer">
      {children}
    </a>
  );
}

export const markdownComponents = {
  pre: CodeBlock,
  a: MarkdownAnchor,
} satisfies React.ComponentProps<typeof ReactMarkdown>["components"];
