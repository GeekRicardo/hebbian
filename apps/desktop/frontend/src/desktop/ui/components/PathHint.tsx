import { type ReactNode } from "react";
import { HoverHint } from "@/desktop/ui/components/HoverHint";
import { pathLeaf } from "@/desktop/ui/lib/utils";

type Props = {
  path: string;
  className?: string;
  side?: "top" | "bottom";
  align?: "start" | "center" | "end";
  children?: ReactNode;
};

export function PathHint({
  path,
  className,
  side = "top",
  align = "start",
  children,
}: Props) {
  return (
    <HoverHint hint={path} className={className} side={side} align={align}>
      {children ?? <span title={path}>{pathLeaf(path)}</span>}
    </HoverHint>
  );
}
