import { useCallback, useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { FileText, X } from "lucide-react";
import type { MessageAttachment } from "@/desktop/ui/types";
import { cn } from "@/desktop/ui/lib/utils";
import { nextPreviewZoom } from "@/desktop/ui/lib/imagePreviewZoom";

type Variant = "composer" | "compact" | "gallery";

interface Props {
  attachments?: MessageAttachment[];
  variant?: Variant;
  onRemove?: (index: number) => void;
  className?: string;
}

interface PreviewImage {
  src: string;
  name: string;
}

export function AttachmentPreviewStrip({
  attachments = [],
  variant = "compact",
  onRemove,
  className,
}: Props) {
  const [preview, setPreview] = useState<PreviewImage | null>(null);

  const content = useMemo(
    () =>
      attachments.map((attachment, index) => {
        if (attachment.kind === "image") {
          const src = imageAttachmentSrc(attachment);
          return (
            <ImageThumb
              key={`${attachment.kind}-${attachment.name}-${index}`}
              src={src}
              name={attachment.name}
              onPreview={() => setPreview({ src, name: attachment.name })}
              onRemove={onRemove ? () => onRemove(index) : undefined}
            />
          );
        }
        return (
          <AttachmentPill
            key={`${attachment.kind}-${attachment.name}-${index}`}
            name={attachment.name}
            onRemove={onRemove ? () => onRemove(index) : undefined}
            variant={variant}
          />
        );
      }),
    [attachments, onRemove, variant]
  );

  if (attachments.length === 0) return null;

  return (
    <>
      <div className={cn("flex flex-wrap gap-1.5", className)}>{content}</div>
      {preview &&
        createPortal(
          <ImagePreviewOverlay
            image={preview}
            onClose={() => setPreview(null)}
          />,
          document.body
        )}
    </>
  );
}

/**
 * 方形图片缩略图：所有场景下统一外观（输入框 / user / assistant 消息）。
 * 文件名不直接显示，鼠标 hover 通过 title 提示；右上角的 X 按钮在 hover 时浮现。
 */
function ImageThumb({
  src,
  name,
  onPreview,
  onRemove,
}: {
  src: string;
  name: string;
  onPreview?: () => void;
  onRemove?: () => void;
}) {
  return (
    <div className="relative shrink-0 group/thumb" title={name}>
      <button
        type="button"
        onClick={onPreview}
        className="block h-14 w-14 overflow-hidden rounded-md border border-border bg-muted transition hover:border-primary/60 focus:outline-none focus:ring-2 focus:ring-ring"
        aria-label={`预览图片 ${name}`}
      >
        <img
          src={src}
          alt={name}
          className="h-full w-full object-cover"
          draggable={false}
        />
      </button>
      {onRemove && (
        <button
          type="button"
          onClick={onRemove}
          className="absolute -right-1 -top-1 inline-flex h-4 w-4 items-center justify-center rounded-full border border-border bg-background text-muted-foreground opacity-0 group-hover/thumb:opacity-100 transition hover:text-destructive shadow"
          aria-label="移除附件"
        >
          <X className="h-2.5 w-2.5" />
        </button>
      )}
    </div>
  );
}

interface AttachmentPillProps {
  name: string;
  onRemove?: () => void;
  variant: Variant;
}

function AttachmentPill({ name, onRemove, variant }: AttachmentPillProps) {
  return (
    <div
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md border border-border bg-muted/60 px-2 py-1 text-xs text-muted-foreground",
        variant === "composer" ? "max-w-[220px]" : "max-w-[240px]"
      )}
    >
      <FileText className="h-3.5 w-3.5 shrink-0" />
      <span className="min-w-0 truncate">{name}</span>
      {onRemove && (
        <button
          type="button"
          onClick={onRemove}
          className="rounded p-0.5 text-muted-foreground hover:bg-background hover:text-foreground"
          title="移除附件"
        >
          <X className="h-3 w-3" />
        </button>
      )}
    </div>
  );
}

function ImagePreviewOverlay({
  image,
  onClose,
}: {
  image: PreviewImage;
  onClose: () => void;
}) {
  const [zoom, setZoom] = useState(1);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  useEffect(() => {
    setZoom(1);
  }, [image.src]);

  const handleWheel = useCallback((event: React.WheelEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
    setZoom((current) => nextPreviewZoom(current, event.deltaY));
  }, []);

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center overflow-hidden bg-black/80 p-4"
      role="dialog"
      aria-modal="true"
      aria-label={image.name}
      onClick={onClose}
    >
      <button
        type="button"
        onClick={onClose}
        className="absolute right-4 top-4 rounded-md bg-background/90 p-2 text-foreground shadow hover:bg-background"
        title="关闭"
      >
        <X className="h-5 w-5" />
      </button>
      <figure
        className="max-h-full max-w-full"
        onClick={(event) => event.stopPropagation()}
        onWheel={handleWheel}
        onDoubleClick={() => setZoom(1)}
      >
        <img
          src={image.src}
          alt={image.name}
          className="max-h-[calc(100vh-6rem)] max-w-[calc(100vw-2rem)] rounded-md object-contain shadow-2xl transition-transform duration-100"
          draggable={false}
          style={{ transform: `scale(${zoom})` }}
        />
        <figcaption className="mt-2 truncate text-center text-sm text-white/80">
          {image.name}
        </figcaption>
      </figure>
    </div>
  );
}

function imageAttachmentSrc(attachment: Extract<MessageAttachment, { kind: "image" }>) {
  return `data:${attachment.media_type};base64,${attachment.data}`;
}
