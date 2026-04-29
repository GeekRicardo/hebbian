const BASE64_IMAGE_PREFIXES: Array<[RegExp, string]> = [
  [/^iVBORw0KGgo/i, "image/png"],
  [/^\/9j\//i, "image/jpeg"],
  [/^R0lGOD/i, "image/gif"],
  [/^UklGR/i, "image/webp"],
];

function compactBase64(value: string) {
  return value.replace(/\s/g, "");
}

export function avatarImageSrc(value?: string | null): string | null {
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

export function isImageAvatar(value?: string | null) {
  return avatarImageSrc(value) !== null;
}
