import { projectInputWithoutAllowedPath } from "./projectFolders";
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