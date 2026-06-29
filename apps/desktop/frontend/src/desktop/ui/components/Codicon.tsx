import { cn } from "@/desktop/ui/lib/utils";

export function Codicon({ name, className }: { name: string; className?: string }) {
  return <span aria-hidden="true" className={cn("codicon", `codicon-${name}`, className)} />;
}
