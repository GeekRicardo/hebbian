import { cn } from "@/desktop/ui/lib/utils";

interface LoopingWebmProps {
  src: string;
  className?: string;
  imageClassName?: string;
}

export function LoopingWebm({
  src,
  className,
  imageClassName,
}: LoopingWebmProps) {
  return (
    <span
      className={cn(
        "relative inline-flex items-center justify-center overflow-hidden bg-transparent",
        className,
      )}
      aria-hidden="true"
    >
      <img
        src={src}
        className={cn(
          "h-full w-full object-contain",
          imageClassName,
        )}
        alt=""
      />
    </span>
  );
}
