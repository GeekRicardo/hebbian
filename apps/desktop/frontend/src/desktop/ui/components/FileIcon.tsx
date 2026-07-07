import { Codicon } from "./Codicon";

/**
 * VS Code 风格的文件图标：按扩展名映射到 Codicon 子类型图标。
 *
 * VS Code 实际使用 file icon theme（如 Seti / Material），这里用 Codicon 自带的
 * 子类型图标（file-code / file-binary / file-media / file-text / file-pdf / file-zip）
 * 做近似映射，效果接近 VS Code 默认行为。
 */

type ExtensionMap = Record<string, string>;

const SOURCE_EXT: ExtensionMap = {
  rs: "file-code",
  ts: "file-code",
  tsx: "file-code",
  js: "file-code",
  jsx: "file-code",
  mjs: "file-code",
  cjs: "file-code",
  py: "file-code",
  go: "file-code",
  java: "file-code",
  rb: "file-code",
  php: "file-code",
  c: "file-code",
  h: "file-code",
  cpp: "file-code",
  hpp: "file-code",
  cs: "file-code",
  swift: "file-code",
  kt: "file-code",
  scala: "file-code",
  dart: "file-code",
  toml: "file-code",
  yaml: "file-code",
  yml: "file-code",
  json: "file-code",
  jsonc: "file-code",
  xml: "file-code",
  sql: "file-code",
  sh: "file-code",
  bash: "file-code",
  zsh: "file-code",
  fish: "file-code",
  ps1: "file-code",
  dockerfile: "file-code",
  makefile: "file-code",
  cmake: "file-code",
  r: "file-code",
  lua: "file-code",
  hs: "file-code",
  ex: "file-code",
  exs: "file-code",
  zig: "file-code",
};

const HTML_CSS_EXT: ExtensionMap = {
  html: "file-code",
  htm: "file-code",
  css: "file-code",
  scss: "file-code",
  sass: "file-code",
  less: "file-code",
  vue: "file-code",
  svelte: "file-code",
  astro: "file-code",
};

const TEXT_EXT: ExtensionMap = {
  md: "file-text",
  markdown: "file-text",
  mdx: "file-text",
  txt: "file-text",
  log: "file-text",
  csv: "file-text",
  tsv: "file-text",
  ini: "file-text",
  cfg: "file-text",
  conf: "file-text",
  env: "file-text",
  gitignore: "file-text",
  gitattributes: "file-text",
  editorconfig: "file-text",
  license: "file-text",
  lock: "file-text",
  nix: "file-text",
};

const MEDIA_EXT: ExtensionMap = {
  png: "file-media",
  jpg: "file-media",
  jpeg: "file-media",
  gif: "file-media",
  svg: "file-media",
  webp: "file-media",
  ico: "file-media",
  bmp: "file-media",
  tiff: "file-media",
  mp4: "file-media",
  webm: "file-media",
  mp3: "file-media",
  wav: "file-media",
  ogg: "file-media",
  woff: "file-media",
  woff2: "file-media",
  ttf: "file-media",
  otf: "file-media",
};

const ARCHIVE_EXT: ExtensionMap = {
  zip: "file-zip",
  tar: "file-zip",
  gz: "file-zip",
  tgz: "file-zip",
  bz2: "file-zip",
  xz: "file-zip",
  zst: "file-zip",
  "7z": "file-zip",
  rar: "file-zip",
};

const BINARY_EXT: ExtensionMap = {
  exe: "file-binary",
  dll: "file-binary",
  so: "file-binary",
  dylib: "file-binary",
  wasm: "file-binary",
  class: "file-binary",
  pyc: "file-binary",
  deb: "file-binary",
  rpm: "file-binary",
  dmg: "file-binary",
  iso: "file-binary",
  pdf: "file-pdf",
};

const EXT_TO_ICON: Map<string, string> = new Map(
  Object.entries({ ...SOURCE_EXT, ...HTML_CSS_EXT, ...TEXT_EXT, ...MEDIA_EXT, ...ARCHIVE_EXT, ...BINARY_EXT }),
);

function ext(path: string): string {
  const dot = path.lastIndexOf(".");
  if (dot <= 0) return "";
  return path.slice(dot + 1).toLowerCase();
}

function basename(path: string): string {
  const parts = path.replace(/\\/g, "/").split("/");
  return parts[parts.length - 1] || path;
}

function baseNameExt(path: string): string {
  return basename(path).toLowerCase();
}

export function fileIconName(path: string): string {
  const e = ext(path);
  if (e && EXT_TO_ICON.has(e)) return EXT_TO_ICON.get(e)!;

  // 无扩展名文件特殊匹配
  const base = baseNameExt(path);
  if (["license", "makefile", "dockerfile", "procfile"].includes(base)) return "file-code";
  if (["gitignore", "gitattributes", "editorconfig", "env", "gemfile", "brewfile"].includes(base)) return "file-text";

  return "file";
}

/**
 * 渲染文件图标组件。
 */
export function FileIcon({ path, className }: { path: string; className?: string }) {
  return <Codicon name={fileIconName(path)} className={className} />;
}