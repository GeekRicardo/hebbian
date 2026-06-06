import { useEffect, type ReactNode } from "react";
import { X } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";

interface DialogProps {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  title?: string;
  description?: string;
  children: ReactNode;
  footer?: ReactNode;
  size?: "sm" | "md" | "lg" | "xl" | "2xl";
}

const sizeCls = {
  sm: "max-w-sm",
  md: "max-w-md",
  lg: "max-w-[820px]",
  xl: "max-w-[1120px]",
  "2xl": "max-w-[1040px]",
};

export function Dialog({
  open,
  onOpenChange,
  title,
  description,
  children,
  footer,
  size = "md",
}: DialogProps) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onOpenChange(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onOpenChange]);

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center p-4 bg-black/40 backdrop-blur-sm animate-fade-in"
      onClick={() => onOpenChange(false)}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        className={cn(
          "relative w-full bg-card text-card-foreground border border-border rounded-xl shadow-xl max-h-[90vh] flex flex-col animate-slide-up",
          sizeCls[size]
        )}
      >
        {(title || description) && (
          <div className="px-6 pt-5 pb-3 border-b border-border">
            {title && <h2 className="text-base font-semibold">{title}</h2>}
            {description && (
              <p className="text-xs text-muted-foreground mt-1">{description}</p>
            )}
          </div>
        )}
        <button
          aria-label="close"
          onClick={() => onOpenChange(false)}
          className="absolute top-3 right-3 h-7 w-7 inline-flex items-center justify-center rounded-md hover:bg-accent text-muted-foreground"
        >
          <X className="w-4 h-4" />
        </button>
        <div className="flex-1 overflow-y-auto px-6 py-4">{children}</div>
        {footer && (
          <div className="px-6 py-3 border-t border-border flex items-center justify-end gap-2">
            {footer}
          </div>
        )}
      </div>
    </div>
  );
}
