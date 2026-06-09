import { useRef, useState, type ReactNode } from "react";
import { ImagePlus, X } from "lucide-react";
import { Button } from "@/desktop/ui/components/ui/button";
import { Label, Textarea } from "@/desktop/ui/components/ui/input";
import { cn } from "@/desktop/ui/lib/utils";
import { avatarImageSrc } from "@/desktop/ui/lib/avatar";

type CropDraft = {
  src: string;
  naturalWidth: number;
  naturalHeight: number;
  x: number;
  y: number;
  size: number;
};

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

function clampCrop(crop: CropDraft, patch: Partial<Pick<CropDraft, "x" | "y" | "size">>): CropDraft {
  const minSize = Math.min(64, crop.naturalWidth, crop.naturalHeight);
  const maxSize = Math.min(crop.naturalWidth, crop.naturalHeight);
  const size = clamp(Math.round(patch.size ?? crop.size), minSize, maxSize);
  const maxX = Math.max(0, crop.naturalWidth - size);
  const maxY = Math.max(0, crop.naturalHeight - size);
  return {
    ...crop,
    size,
    x: clamp(Math.round(patch.x ?? crop.x), 0, maxX),
    y: clamp(Math.round(patch.y ?? crop.y), 0, maxY),
  };
}

function createCropDraft(src: string): Promise<CropDraft> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => {
      const naturalWidth = image.naturalWidth;
      const naturalHeight = image.naturalHeight;
      const size = Math.min(naturalWidth, naturalHeight);
      resolve({
        src,
        naturalWidth,
        naturalHeight,
        x: Math.round((naturalWidth - size) / 2),
        y: Math.round((naturalHeight - size) / 2),
        size,
      });
    };
    image.onerror = () => reject(new Error("图片读取失败"));
    image.src = src;
  });
}

function renderCroppedAvatar(crop: CropDraft): Promise<string> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => {
      const canvas = document.createElement("canvas");
      canvas.width = 256;
      canvas.height = 256;
      const ctx = canvas.getContext("2d");
      if (!ctx) {
        reject(new Error("头像生成失败"));
        return;
      }
      ctx.drawImage(image, crop.x, crop.y, crop.size, crop.size, 0, 0, 256, 256);
      resolve(canvas.toDataURL("image/png"));
    };
    image.onerror = () => reject(new Error("图片读取失败"));
    image.src = crop.src;
  });
}

export function AvatarPreview({
  value,
  fallback,
  title,
  className,
  imageClassName,
}: {
  value?: string | null;
  fallback: ReactNode;
  title?: string;
  className?: string;
  imageClassName?: string;
}) {
  const src = avatarImageSrc(value);
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const showImage = src && src !== failedSrc;
  const label = value?.trim();

  return (
    <div
      className={cn(
        "flex items-center justify-center overflow-hidden rounded-md bg-muted text-sm",
        className
      )}
      title={title}
    >
      {showImage ? (
        <img
          src={src}
          alt=""
          className={cn("h-full w-full object-cover", imageClassName)}
          onError={() => setFailedSrc(src)}
        />
      ) : label ? (
        <span className="truncate leading-none">{label}</span>
      ) : (
        fallback
      )}
    </div>
  );
}

export function AvatarField({
  label,
  value,
  onChange,
  suggestions = [],
  placeholder = "Emoji、图片 URL 或 base64",
  previewFallback,
  cropImage = false,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  suggestions?: string[];
  placeholder?: string;
  previewFallback: ReactNode;
  cropImage?: boolean;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [crop, setCrop] = useState<CropDraft | null>(null);
  const [cropError, setCropError] = useState<string | null>(null);

  function handleFile(file?: File) {
    if (!file || !file.type.startsWith("image/")) return;
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result !== "string") return;
      if (!cropImage) {
        onChange(reader.result);
        return;
      }
      createCropDraft(reader.result)
        .then((draft) => {
          setCrop(draft);
          setCropError(null);
        })
        .catch((error: Error) => setCropError(error.message));
    };
    reader.readAsDataURL(file);
  }

  async function applyCrop() {
    if (!crop) return;
    try {
      onChange(await renderCroppedAvatar(crop));
      setCrop(null);
      setCropError(null);
    } catch (error: any) {
      setCropError(error?.message ?? String(error));
    }
  }

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <Label>{label}</Label>
        <div className="flex items-center gap-1">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => inputRef.current?.click()}
          >
            <ImagePlus className="h-3.5 w-3.5" />
            上传
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => onChange("")}
            disabled={!value}
          >
            <X className="h-3.5 w-3.5" />
            清除
          </Button>
          <input
            ref={inputRef}
            type="file"
            accept="image/*"
            className="hidden"
            onChange={(event) => {
              handleFile(event.target.files?.[0]);
              event.currentTarget.value = "";
            }}
          />
        </div>
      </div>

      <div className="grid grid-cols-[48px_1fr] gap-3">
        <AvatarPreview
          value={value}
          fallback={previewFallback}
          className="h-12 w-12 text-xl"
        />
        <Textarea
          rows={3}
          value={value}
          spellCheck={false}
          autoCorrect="off"
          onChange={(event) => onChange(event.target.value)}
          placeholder={placeholder}
          className="min-h-[72px] resize-none text-xs"
        />
      </div>

      {crop && (
        <div className="space-y-3 rounded-xl border border-border bg-background p-3">
          <div className="flex items-start justify-between gap-3">
            <div>
              <div className="text-xs font-medium text-foreground">选择显示区域</div>
              <p className="mt-1 text-xs text-muted-foreground">拖动滑块调整方形区域，保存后会用于聊天头像。</p>
            </div>
            <div className="flex items-center gap-2">
              <Button type="button" variant="outline" size="sm" onClick={() => setCrop(null)}>
                取消
              </Button>
              <Button type="button" size="sm" onClick={applyCrop}>
                使用
              </Button>
            </div>
          </div>
          <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_160px]">
            <div className="relative overflow-hidden rounded-lg border border-border bg-slate-950/5">
              <img src={crop.src} alt="" className="block max-h-[360px] w-full object-contain" />
              <div
                className="pointer-events-none absolute border-2 border-white shadow-[0_0_0_9999px_rgba(15,23,42,0.45)]"
                style={{
                  left: `${(crop.x / crop.naturalWidth) * 100}%`,
                  top: `${(crop.y / crop.naturalHeight) * 100}%`,
                  width: `${(crop.size / crop.naturalWidth) * 100}%`,
                  height: `${(crop.size / crop.naturalHeight) * 100}%`,
                }}
              />
            </div>
            <div className="space-y-2 text-xs">
              <label className="block space-y-1">
                <span className="text-muted-foreground">左右</span>
                <input
                  type="range"
                  min={0}
                  max={Math.max(0, crop.naturalWidth - crop.size)}
                  value={crop.x}
                  onChange={(event) => setCrop(clampCrop(crop, { x: Number(event.target.value) }))}
                  className="w-full"
                />
              </label>
              <label className="block space-y-1">
                <span className="text-muted-foreground">上下</span>
                <input
                  type="range"
                  min={0}
                  max={Math.max(0, crop.naturalHeight - crop.size)}
                  value={crop.y}
                  onChange={(event) => setCrop(clampCrop(crop, { y: Number(event.target.value) }))}
                  className="w-full"
                />
              </label>
              <label className="block space-y-1">
                <span className="text-muted-foreground">大小</span>
                <input
                  type="range"
                  min={Math.min(64, crop.naturalWidth, crop.naturalHeight)}
                  max={Math.min(crop.naturalWidth, crop.naturalHeight)}
                  value={crop.size}
                  onChange={(event) => setCrop(clampCrop(crop, { size: Number(event.target.value) }))}
                  className="w-full"
                />
              </label>
            </div>
          </div>
          {cropError && <div className="text-xs text-destructive">{cropError}</div>}
        </div>
      )}

      {suggestions.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {suggestions.map((avatar) => (
            <button
              key={avatar}
              type="button"
              onClick={() => onChange(avatar)}
              className={cn(
                "h-8 w-8 rounded-md border text-lg flex items-center justify-center transition-colors",
                value === avatar
                  ? "border-primary bg-primary/10"
                  : "border-border hover:bg-accent"
              )}
            >
              {avatar}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
