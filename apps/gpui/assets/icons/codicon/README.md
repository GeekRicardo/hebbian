这些是 VS Code 的 codicon（MIT / CC BY 4.0），直接从 `@vscode/codicons` 拷过来的。

为什么单独放一份而不是统一用 lucide：**右侧工作台那一竖条图标，原前端用的就是 codicon**
（`<Codicon name="files" />` 等），不是 lucide。想「一模一样」就得用同一套字形——
换成形近的 lucide 图标，一眼就能看出不是同一个东西。

对应关系（原前端 RightSidebar.tsx 的 tab 定义）：
files / server-process / diff-modified / source-control / checklist /
list-tree / comment-discussion / globe / terminal
