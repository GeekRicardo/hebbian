import { cn } from "@/desktop/ui/lib/utils";

export interface Suggestion {
  label: string;
  value: string;
}

interface Props {
  suggestions: Suggestion[];
  onSelect: (value: string) => void;
  className?: string;
}

/**
 * 输入框上方的建议 chip 行。suggestions 非空时渲染，空时不占位。
 * 点击 chip 相当于用户自己填入并发送这段文本。
 */
export function InputSuggestions({ suggestions, onSelect, className }: Props) {
  if (suggestions.length === 0) return null;

  return (
    <div
      className={cn(
        "flex flex-wrap gap-1.5 px-3 py-2 border-t border-border",
        className
      )}
    >
      {suggestions.map((s) => (
        <button
          key={s.value}
          type="button"
          onClick={() => onSelect(s.value)}
          className={cn(
            "inline-flex items-center gap-1 rounded-full border border-border",
            "bg-card px-3 py-1 text-xs text-foreground",
            "hover:bg-accent hover:border-accent-foreground/20 transition-colors",
            "active:scale-95"
          )}
        >
          <span className="text-muted-foreground select-none">↩</span>
          {s.label}
        </button>
      ))}
    </div>
  );
}
