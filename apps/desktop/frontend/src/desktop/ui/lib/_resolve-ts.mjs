// Node ESM resolve hook：给无扩展名的相对 import 补 `.ts`。
// 项目源码按 bundler 惯例写无扩展 import（vite/tsc 解析），而 node ESM 要求显式扩展。
// 用法：node --experimental-strip-types --import ./_register-ts.mjs <name>.test.ts
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
export async function resolve(specifier, context, next) {
  const isRel = specifier.startsWith("./") || specifier.startsWith("../");
  if (isRel && !/\.[cm]?[jt]s$/.test(specifier)) {
    const url = new URL(specifier, context.parentURL);
    if (existsSync(fileURLToPath(url) + ".ts")) return next(specifier + ".ts", context);
  }
  return next(specifier, context);
}
