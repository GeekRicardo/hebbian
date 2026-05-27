import { useEffect, useState, type ReactNode } from "react";
import { ChevronUp } from "lucide-react";
import { cn } from "@/desktop/ui/lib/utils";

/**
 * 挂在白色输入框最底部的"展开更多设置"触发条。
 *
 * 一条 14px chevron 条，无底色——仅靠 chevron 朝向 + 颜色明暗暗示状态；
 * 展开/折叠状态由调用方持有，故意不持久化。
 */
export function DrawerToggle({
  open,
  onToggle,
  disabled,
}: {
  open: boolean;
  onToggle: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      disabled={disabled}
      className={cn(
        // 5px 高的窄触发条——overflow-hidden 把 chevron 上下裁掉，只露中间一段细线
        "group/drawer flex h-[5px] w-full items-center justify-center overflow-hidden",
        "transition-colors",
        "disabled:opacity-40 disabled:pointer-events-none"
      )}
      aria-expanded={open}
      aria-label={open ? "收起更多设置" : "展开更多设置"}
      title={open ? "收起" : "更多设置"}
    >
      <ChevronUp
        className={cn(
          "h-3 w-3 text-muted-foreground/50 transition-[transform,color] duration-150",
          "group-hover/drawer:text-foreground",
          open ? "rotate-0" : "rotate-180"
        )}
      />
    </button>
  );
}

interface InputDrawerProps {
  open: boolean;
  left?: ReactNode;
  right?: ReactNode;
}

/**
 * 输入框下方的二级抽屉——独立 rounded-3xl 卡片，向上重叠 32px (-mt-8) 钻入白色卡片下沿，
 * 配合白色卡片 z-10 盖住抽屉上半圆角，视觉上从白色卡片下端"伸出"而不是漂浮分离。
 *
 * 入退场动画：
 * - open: true → mount + animate-slide-up（150ms）
 * - open: false → 标记 closing → animate-slide-down-out（160ms）→ onAnimationEnd 后真正 unmount
 * - 不用 grid-rows/max-height（那需要 overflow-hidden，会裁切 Reasoning / RunMode 的上拉 popup）
 *
 * 反色：容器加 `dark` class，里面的 design token 自动切到 dark 主题。
 */
export function InputDrawer({ open, left, right }: InputDrawerProps) {
  // mounted：组件在 DOM 中（即使 open=false 但退场动画未跑完时也保留）
  const [mounted, setMounted] = useState(open);

  useEffect(() => {
    if (open) setMounted(true);
    // open=false 时不立刻 unmount——等 onAnimationEnd 触发后才卸载（让退场动画跑完）
  }, [open]);

  function handleAnimationEnd() {
    if (!open) setMounted(false);
  }

  if (!mounted) return null;

  return (
    <div
      className={cn(
        // 与白色卡片左右对齐；向上重叠 24px (-mt-6 = rounded-3xl 半径)，让黑色矩形顶部对齐
        // 白色下圆角顶端 y=H-24。白色 z-10 在上覆盖 [H-24, H] 范围内**白色圆角内**的部分，
        // 圆角**外侧凹陷三角**白色没填充——黑色透过显示，从而"填实"白色下圆角的凹陷。
        "dark -mt-6 relative",
        open ? "animate-slide-up" : "animate-slide-down-out"
      )}
      onAnimationEnd={handleAnimationEnd}
      aria-hidden={!open}
    >
      <div
        className={cn(
          "flex flex-wrap items-center justify-between gap-2",
          // 上沿直角（rounded-t-none），下沿圆角——配合 -mt-6 让黑色填实白色下圆角凹陷
          "rounded-t-none rounded-b-3xl bg-background text-foreground",
          // pt-8 补回 -mt-6 钻入区域 (24px) + 8px 视觉间距
          "px-3 pt-8 pb-2"
        )}
      >
        <div className="flex flex-wrap items-center gap-1.5">{left}</div>
        <div className="flex items-center gap-1.5">{right}</div>
      </div>
    </div>
  );
}
