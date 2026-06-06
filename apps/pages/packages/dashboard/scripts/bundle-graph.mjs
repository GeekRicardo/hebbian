/**
 * bundle-graph — 将 UA 知识图谱 + 源码打包成静态 JSON 文件
 *
 * 用法: node scripts/bundle-graph.mjs [project-root]
 *   不传参数时自动向上查找 .understand-anything/
 *
 * 输出到 public/ 目录，Vite 构建时自动复制到 dist/
 *   - public/knowledge-graph.json   (图谱数据)
 *   - public/source-content.json    (源码内容映射)
 */

import { readFileSync, writeFileSync, statSync, existsSync, mkdirSync } from "fs";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

// ---- 项目根目录 ----
const projectRoot = (() => {
  const arg = process.argv[2];
  if (arg) return path.resolve(arg);
  // 默认: 从 scripts/ 上两级到 packages/dashboard/, 再上四级到项目根
  return path.resolve(__dirname, "..", "..", "..", "..", "..");
})();

const graphDir = path.join(projectRoot, ".understand-anything");
const graphPath = path.join(graphDir, "knowledge-graph.json");
const outDir = path.resolve(__dirname, "..", "public");

if (!existsSync(graphPath)) {
  console.error(`❌ knowledge-graph.json 不存在: ${graphPath}`);
  console.error("   请先在项目根运行 /understand 生成知识图谱");
  process.exit(1);
}

// ---- 确保输出目录存在 ----
mkdirSync(outDir, { recursive: true });

// ---- 1. 复制 knowledge-graph.json ----
const graph = JSON.parse(readFileSync(graphPath, "utf-8"));
const graphOutputPath = path.join(outDir, "knowledge-graph.json");
writeFileSync(graphOutputPath, JSON.stringify(graph));
console.log(`✓ knowledge-graph.json → public/  (${graph.nodes.length} 节点, ${graph.edges?.length ?? 0} 边)`);

// ---- 2. 构建 source-content.json ----
const extToLanguage = {
  rs: "rust",     ts: "typescript",  tsx: "tsx",
  js: "javascript", jsx: "jsx",      mjs: "javascript",
  py: "python",   css: "css",        html: "markup",
  json: "json",   md: "markdown",    yaml: "yaml",
  yml: "yaml",    toml: "toml",      sh: "bash",
  bash: "bash",   zsh: "bash",       sql: "sql",
  go: "go",       rb: "ruby",        xml: "markup",
  svg: "markup",  dockerfile: "dockerfile",
  graphql: "graphql", proto: "protobuf",
};

function detectLanguage(filePath) {
  const base = path.basename(filePath).toLowerCase();
  if (base === "dockerfile") return "dockerfile";
  if (base === "makefile") return "makefile";
  if (base === ".env.example" || base.startsWith(".env")) return "bash";
  const ext = filePath.split(".").pop()?.toLowerCase();
  return extToLanguage[ext] ?? "text";
}

const sourceContent = {};
let embedded = 0;
let skipped = 0;
const MAX_FILE_BYTES = 1024 * 1024; // 1MB

for (const node of graph.nodes) {
  if (!node.filePath) continue;
  const fullPath = path.join(projectRoot, node.filePath);

  if (!existsSync(fullPath)) {
    skipped++;
    continue;
  }

  let stat;
  try { stat = statSync(fullPath); } catch { skipped++; continue; }
  if (stat.size > MAX_FILE_BYTES) {
    console.warn(`  ⚠ 跳过大文件 (${(stat.size / 1024 / 1024).toFixed(1)}MB): ${node.filePath}`);
    skipped++;
    continue;
  }

  let content;
  try { content = readFileSync(fullPath, "utf-8"); } catch { skipped++; continue; }

  // 拒绝二进制文件
  if (content.includes("\0")) { skipped++; continue; }

  sourceContent[node.filePath] = {
    language: detectLanguage(node.filePath),
    content,
    lineCount: content.split("\n").length,
    sizeBytes: stat.size,
  };
  embedded++;
}

const scOutputPath = path.join(outDir, "source-content.json");
writeFileSync(scOutputPath, JSON.stringify(sourceContent));
console.log(`✓ source-content.json → public/  (${embedded} 文件, ${skipped} 跳过)`);

// ---- 3. 生成 meta.json (最小必需数据) ----
const meta = {
  lastAnalyzedAt: graph.project?.analyzedAt ?? new Date().toISOString(),
  gitCommitHash: graph.project?.gitCommitHash ?? "unknown",
  version: graph.version ?? "1.0.0",
  analyzedFiles: graph.nodes.length,
};
writeFileSync(path.join(outDir, "meta.json"), JSON.stringify(meta));
console.log(`✓ meta.json → public/`);

console.log(`\n📦 打包完成。输出: ${outDir}/`);
console.log(`   运行 pnpm build:ghpages 来构建静态站点`);
