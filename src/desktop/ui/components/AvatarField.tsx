import { useRef, useState, type ReactNode } from "react";
import { ImagePlus, X } from "lucide-react";
import { Button } from "@/desktop/ui/components/ui/button";
import { Label, Textarea } from "@/desktop/ui/components/ui/input";
import { cn } from "@/desktop/ui/lib/utils";
import { avatarImageSrc } from "@/desktop/ui/lib/avatar";

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
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  suggestions?: string[];
  placeholder?: string;
  previewFallback: ReactNode;
}) {
  const inputRef = useRef<HTMLInputElement>(null);

  function handleFile(file?: File) {
    if (!file || !file.type.startsWith("image/")) return;
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === "string") onChange(reader.result);
    };
    reader.readAsDataURL(file);
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
