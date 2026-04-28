import { useCallback, useEffect, useMemo, useState } from "react";
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
  const hasGalleryImages =
    variant === "gallery" &&
    attachments.some((attachment) => attachment.kind === "image");

  const content = useMemo(
    () =>
      attachments.map((attachment, index) => {
        if (attachment.kind === "image") {
          const src = imageAttachmentSrc(attachment);
          if (variant === "gallery") {
            return (
              <button
                key={`${attachment.kind}-${attachment.name}-${index}`}
                type="button"
                onClick={() => setPreview({ src, name: attachment.name })}
                className="group/image min-w-0 overflow-hidden rounded-md border border-border bg-background text-left transition hover:border-primary/60"
                title="预览图片"
              >
                <img
                  src={src}
                  alt={attachment.name}
                  className="h-44 w-full bg-muted object-contain"
                  draggable={false}
                />
                <div className="truncate border-t border-border px-2 py-1.5 text-xs text-muted-foreground group-hover/image:text-foreground">
                  {attachment.name}
                </div>
              </button>
            );
          }

          return (
            <AttachmentPill
              key={`${attachment.kind}-${attachment.name}-${index}`}
              name={attachment.name}
              onRemove={onRemove ? () => onRemove(index) : undefined}
              imageSrc={src}
              onPreview={() => setPreview({ src, name: attachment.name })}
              variant={variant}
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
      <div
        className={cn(
          hasGalleryImages
            ? "grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-2"
            : "flex flex-wrap gap-1.5",
          className
        )}
      >
        {content}
      </div>
      {preview && (
        <ImagePreviewOverlay image={preview} onClose={() => setPreview(null)} />
      )}
    </>
  );
}

interface AttachmentPillProps {
  name: string;
  imageSrc?: string;
  onPreview?: () => void;
  onRemove?: () => void;
  variant: Variant;
}

function AttachmentPill({
  name,
  imageSrc,
  onPreview,
  onRemove,
  variant,
}: AttachmentPillProps) {
  const imageSize = variant === "composer" ? "h-5 w-5" : "h-8 w-8";

  return (
    <div
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md border border-border bg-muted/60 px-2 py-1 text-xs text-muted-foreground",
        variant === "composer" ? "max-w-[220px]" : "max-w-[240px]"
      )}
    >
      {imageSrc ? (
        <button
          type="button"
          onClick={onPreview}
          className="shrink-0 rounded focus:outline-none focus:ring-2 focus:ring-ring"
          title="预览图片"
        >
          <img
            src={imageSrc}
            alt={name}
            className={cn(imageSize, "rounded object-cover")}
            draggable={false}
          />
        </button>
      ) : (
        <FileText className="h-3.5 w-3.5 shrink-0" />
      )}
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
