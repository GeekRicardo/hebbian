// 注册 _resolve-ts.mjs（搭配 --experimental-strip-types 跑带相对 import 的 *.test.ts）。
import { register } from "node:module";
register("./_resolve-ts.mjs", import.meta.url);
