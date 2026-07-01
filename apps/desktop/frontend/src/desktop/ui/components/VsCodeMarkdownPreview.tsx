import MarkdownIt from "markdown-it";
import { memo, useMemo } from "react";
import { toast } from "sonner";
import { cn } from "@/desktop/ui/lib/utils";
import { openLink } from "@/desktop/ui/lib/openLink";

const markdownIt = new MarkdownIt({
  html: false,
  linkify: true,
  typographer: false,
  breaks: false,
});

export const VsCodeMarkdownPreview = memo(function VsCodeMarkdownPreview({
  markdown,
  className,
}: {
  markdown: string;
  className?: string;
}) {
  const html = useMemo(() => markdownIt.render(markdown), [markdown]);

  return (
    <div
      className={cn("vscode-markdown-preview", className)}
      onClick={(event) => {
        const link = (event.target as HTMLElement).closest("a");
        const href = link?.getAttribute("href");
        if (!href) return;
        event.preventDefault();
        event.stopPropagation();
        void openLink(href).catch(() => toast.error("打开链接失败"));
      }}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
});
