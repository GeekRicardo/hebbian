import { type ReactNode } from "react";

interface Props {
  hint: ReactNode;
  side?: "top" | "bottom";
  align?: "start" | "center" | "end";
  className?: string;
  keepOpenDelayMs?: number;
  children: ReactNode;
}

export function HoverHint({ children }: Props) {
  return <>{children}</>;
}
