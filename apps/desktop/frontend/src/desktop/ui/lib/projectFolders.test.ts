import {
  projectInputWithoutAllowedPath,
  projectInputWithAllowedPaths,
} from "./projectFolders";
import type { WorkspaceProject } from "@/desktop/ui/types";

function makeProject(): WorkspaceProject {
  return {
    id: "-tmp-demo",
    name: "demo",
    folders: [
      { path: "/tmp/demo" }, // workdir
      { path: "/tmp/demo-lib" },
      { path: "/tmp/demo-docs" },
    ],
    source: "manual",
    created_at: 1,
    updated_at: 2,
  };
}

function assertEqual<T>(actual: T, expected: T, label: string) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${label}: expected ${e}, got ${a}`);
  }
}

// 删中间一条 allowed_path：workdir 保留、剩余 allowed_paths 正确，id/name/source 透传。
assertEqual(
  projectInputWithoutAllowedPath(makeProject(), "/tmp/demo-lib"),
  {
    id: "-tmp-demo",
    name: "demo",
    workdir: "/tmp/demo",
    allowed_paths: ["/tmp/demo-docs"],
    source: "manual",
  },
  "removes a middle allowed path and keeps workdir",
);

// 删等于 workdir 的路径：folders[0] 不在删除范围内，workdir 不被清掉。
assertEqual(
  projectInputWithoutAllowedPath(makeProject(), "/tmp/demo"),
  {
    id: "-tmp-demo",
    name: "demo",
    workdir: "/tmp/demo",
    allowed_paths: ["/tmp/demo-lib", "/tmp/demo-docs"],
    source: "manual",
  },
  "never drops the workdir even if path equals it",
);

// 删不存在的路径：allowed_paths 原样保留。
assertEqual(
  projectInputWithoutAllowedPath(makeProject(), "/tmp/not-there").allowed_paths,
  ["/tmp/demo-lib", "/tmp/demo-docs"],
  "leaves allowed paths intact when path is absent",
);

// source 缺省时落为 null（saveProject 入参约定）。
assertEqual(
  projectInputWithoutAllowedPath(
    { ...makeProject(), source: undefined },
    "/tmp/demo-lib",
  ).source,
  null,
  "defaults missing source to null",
);

// ── projectInputWithAllowedPaths ─────────────────────────────────────

// 追加新路径到项目配置
assertEqual(
  projectInputWithAllowedPaths(makeProject(), ["/tmp/demo-new"]),
  {
    id: "-tmp-demo",
    name: "demo",
    workdir: "/tmp/demo",
    allowed_paths: ["/tmp/demo-lib", "/tmp/demo-docs", "/tmp/demo-new"],
    source: "manual",
  },
  "appends new paths to existing allowed_paths",
);

// 去重：新路径与已有路径重复时不重复添加
assertEqual(
  projectInputWithAllowedPaths(makeProject(), ["/tmp/demo-lib", "/tmp/demo-new"]),
  {
    id: "-tmp-demo",
    name: "demo",
    workdir: "/tmp/demo",
    allowed_paths: ["/tmp/demo-lib", "/tmp/demo-docs", "/tmp/demo-new"],
    source: "manual",
  },
  "deduplicates paths that already exist in project",
);

// 空新路径列表：allowed_paths 原样保留
assertEqual(
  projectInputWithAllowedPaths(makeProject(), []),
  {
    id: "-tmp-demo",
    name: "demo",
    workdir: "/tmp/demo",
    allowed_paths: ["/tmp/demo-lib", "/tmp/demo-docs"],
    source: "manual",
  },
  "empty newPaths leaves allowed_paths intact",
);

// source 缺省时落为 null
assertEqual(
  projectInputWithAllowedPaths(
    { ...makeProject(), source: undefined },
    ["/tmp/extra"],
  ).source,
  null,
  "defaults missing source to null",
);