import type { MessageAttachment } from "@/desktop/ui/types";

/** 输入框附件上限：图片 12MB、文本 1MB。主对话与旁支/注释通用。 */
export const MAX_TEXT_FILE_BYTES = 1024 * 1024;
export const MAX_IMAGE_BYTES = 12 * 1024 * 1024;

/** 是否当作文本文件处理（按 MIME 前缀或扩展名白名单）。 */
export function isTextFile(file: File) {
  if (file.type.startsWith("text/")) return true;
  return /\.(txt|md|markdown|json|jsonl|csv|ts|tsx|js|jsx|rs|py|go|java|c|cpp|h|hpp|css|html|xml|yaml|yml|toml|sql)$/i.test(
    file.name
  );
}

/** 由文件名推断文本类 media type（拿不到 file.type 时兜底）。 */
export function mediaTypeFromName(name: string) {
  const lower = name.toLowerCase();
  if (lower.endsWith(".json")) return "application/json";
  if (lower.endsWith(".xml")) return "application/xml";
  if (lower.endsWith(".html")) return "text/html";
  if (lower.endsWith(".css")) return "text/css";
  if (lower.endsWith(".csv")) return "text/csv";
  if (lower.endsWith(".md") || lower.endsWith(".markdown")) return "text/markdown";
  return "text/plain";
}

/** 把图片 File 读成 base64 图片附件。 */
export async function imageAttachmentFromFile(file: File): Promise<MessageAttachment> {
  const dataUrl = await readFileAsDataUrl(file);
  const comma = dataUrl.indexOf(",");
  const data = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
  return {
    kind: "image",
    name: file.name || "pasted-image.png",
    media_type: file.type || mediaTypeFromDataUrl(dataUrl) || "image/png",
    data,
  };
}

function readFileAsDataUrl(file: File) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () => reject(new Error(`${file.name} 读取失败`));
    reader.readAsDataURL(file);
  });
}

function mediaTypeFromDataUrl(dataUrl: string) {
  const match = /^data:([^;,]+)[;,]/.exec(dataUrl);
  return match?.[1] ?? null;
}
