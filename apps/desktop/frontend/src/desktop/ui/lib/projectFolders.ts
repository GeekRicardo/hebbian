import type { WorkspaceProject, WorkspaceProjectInput } from "@/desktop/ui/types";

/**
 * 从项目配置里移除某个允许路径，返回 saveProject 所需的入参。
 *
 * 项目的 folders 约定：folders[0] 是 workdir，其余才是 allowed_paths。
 * 删除只作用于 allowed_paths（slice(1)）——即使传入的 path 恰好等于 workdir，
 * 也不会把 workdir 删掉，避免把项目主目录意外清空。
 */
export function projectInputWithoutAllowedPath(
  project: WorkspaceProject,
  path: string,
): WorkspaceProjectInput {
  const folders = project.folders;
  const workdir = folders[0]?.path ?? "";
  const allowed_paths = folders
    .slice(1)
    .map((folder) => folder.path)
    .filter((p) => p !== path);
  return {
    id: project.id,
    name: project.name,
    workdir,
    allowed_paths,
    source: project.source ?? null,
  };
}