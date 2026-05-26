import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { CodeBlock } from "./CodeBlock";

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

const markdownComponents = { pre: CodeBlock } satisfies React.ComponentProps<
  typeof ReactMarkdown
>["components"];
