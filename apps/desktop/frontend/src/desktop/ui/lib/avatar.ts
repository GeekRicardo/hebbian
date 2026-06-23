const BASE64_IMAGE_PREFIXES: Array<[RegExp, string]> = [
  [/^iVBORw0KGgo/i, "image/png"],
  [/^\/9j\//i, "image/jpeg"],
  [/^R0lGOD/i, "image/gif"],
  [/^UklGR/i, "image/webp"],
];

function compactBase64(value: string) {
  return value.replace(/\s/g, "");
}

function computeAvatarImageSrc(value?: string | null): string | null {
  const raw = value?.trim();
  if (!raw) return null;

  if (/^data:image\/(?:png|jpe?g|gif|webp);base64,/i.test(raw)) {
    return raw;
  }

  try {
    const url = new URL(raw);
    if (url.protocol === "http:" || url.protocol === "https:") {
      return raw;
    }
  } catch {
    // Not a URL; it may still be a raw base64 image.
  }

  const encoded = compactBase64(raw);
  if (encoded.length < 80 || !/^[A-Za-z0-9+/]+={0,2}$/.test(encoded)) {
    return null;
  }

  const detected = BASE64_IMAGE_PREFIXES.find(([pattern]) =>
    pattern.test(encoded)
  );
  if (!detected) return null;

  return `data:${detected[1]};base64,${encoded}`;
}

// 解析一次头像源要跑多趟全量字符串扫描（trim / replace / 正则）；内嵌的 base64
// 头像可达数百 KB，单次就不便宜。消息列表每条气泡都渲染头像、且传入的是同一个
// 头像值，逐条重算会把主线程拖死。按入参缓存：纯值入参、同输入同输出，缓存不改
// 语义；有界、满则整清，避免无限增长。
const srcCache = new Map<string | null | undefined, string | null>();
const SRC_CACHE_LIMIT = 32;

export function avatarImageSrc(value?: string | null): string | null {
  if (srcCache.has(value)) return srcCache.get(value) ?? null;
  const result = computeAvatarImageSrc(value);
  if (srcCache.size >= SRC_CACHE_LIMIT) srcCache.clear();
  srcCache.set(value, result);
  return result;
}

export function isImageAvatar(value?: string | null) {
  return avatarImageSrc(value) !== null;
}
