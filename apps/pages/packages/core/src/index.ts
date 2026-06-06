// 精简版 core — 只包含 dashboard 实际需要的导出
// 原版包含 tree-sitter、fingerprint、analyzer 等分析管道模块，dashboard 不需要
export * from "./types.js";
export {
  KnowledgeGraphSchema,
  validateGraph,
  sanitizeGraph,
  autoFixGraph,
  COMPLEXITY_ALIASES,
  DIRECTION_ALIASES,
  type ValidationResult,
  type GraphIssue,
} from "./schema.js";
export { SearchEngine, type SearchResult, type SearchOptions } from "./search.js";
