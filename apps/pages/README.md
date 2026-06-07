# Hebbian Pages — 把代码库知识图谱搬上 GitHub Pages

一个**纯静态**站点：打开网页就能浏览整个项目的交互式知识图谱、看任意文件源码、点代码里的符号跳转定义/查找引用，还能配上自己的 LLM Key 边聊天边理解项目。零后端，整站就是一堆静态文件，丢 GitHub Pages 上即可。

线上示例：<https://geekricardo.github.io/hebbian/>

---

## 一、来龙去脉（这东西怎么来的）

### 1. 起点：Understand Anything

[Understand Anything](https://github.com/Lum1104/Understand-Anything)（下称 **UA**）是一个 Claude Code 插件：用 LLM + 静态分析把一个代码库扫描成一份**知识图谱** `.understand-anything/knowledge-graph.json`，再用一个 React Dashboard 把图谱可视化出来。

它原本的 Dashboard 跑法是**本地起一个 Vite dev server**：

- server 端用一次性 token 做门控，前端 `fetch('/knowledge-graph.json?token=...')` 拿数据；
- 看源码时前端 `fetch('/file-content.json?token=...&path=...')`，由 server 实时读磁盘返回文件内容。

也就是说，**它强依赖那个本地 server**——关掉就没了，更没法分享给别人看。

### 2. 需求：变成谁都能打开的静态页面

我们想要的是：

1. 把已经跑好的图谱**长期挂在网上**，任何人点链接就能看；
2. 保留 UA Dashboard 的**全部**能力（力导向图、分层、领域视图、导览 Tour、文件树、过滤、主题、代码高亮……）；
3. 额外加一个 **LLM 对话**面板，像 UA chat 那样结合图谱回答问题，Key 存在用户浏览器本地；
4. 全程**零服务端**，能直接部署到 GitHub Pages。

### 3. 走过的弯路

- **第一版**：从零手写了一个精简 dashboard。结果丢掉了导览、分层、领域视图、ELK 布局等一大半功能，是个「阉割版」，废弃。
- **第二版（当前）**：直接把 **UA 完整 Dashboard 源码**搬进来，只做三件「外科手术」式改动：
  1. 去掉 server 依赖 —— 新增**静态模式**，数据和源码改成构建时打包进静态 JSON；
  2. 加 **ChatPanel** —— LLM 对话 + 多会话 + 图谱/源码上下文 + 可点击引用；
  3. 加 **符号跳转** —— 代码里点函数/类名，弹「跳转定义 / 查找引用」。

### 4. 部署踩的坑

Action 构建、`gh-pages` 分支推送都成功了，但访问 Pages 一直 404。排查发现**和仓库是否私有无关**——根因是这个仓库**从来没启用过 Pages**（GitHub 不会因为存在 `gh-pages` 分支就自动开始服务）。补一次启用即可：

```bash
gh api repos/<owner>/<repo>/pages -X POST \
  -f "source[branch]=gh-pages" -f "source[path]=/"
```

> 注意：**私有仓库**的 GitHub Pages 需要 Pro/Team/Enterprise 套餐；公开仓库免费可用。

---

## 二、架构

这是一个 pnpm 工作区（独立于 hebbian 主仓的 Rust workspace）：

```
apps/pages/
├── package.json              # 工作区根：dev / build / build:ghpages 等编排脚本
├── pnpm-workspace.yaml
└── packages/
    ├── core/                 # @understand-anything/core（精简版）
    │   └── src/              # 只保留 dashboard 需要的 types / search / schema
    └── dashboard/            # @understand-anything/dashboard（UA 完整 dashboard + 我们的改动）
        ├── scripts/
        │   └── bundle-graph.mjs       # 构建前：图谱 + 源码 → public/*.json
        ├── vite.config.ts             # UA 原版（带 token server，本仓不用）
        ├── vite.config.static.ts      # 本地静态开发：STATIC_MODE, base "/"
        ├── vite.config.ghpages.ts     # 线上构建：STATIC_MODE, base "./", 输出 dist-ghpages/
        └── src/
            ├── App.tsx                # 静态模式分支：跳过 token gate，从 public/ 加载
            ├── store.ts               # 加 sourceContent（内嵌源码）
            └── components/
                ├── CodeViewer.tsx     # 静态模式从内嵌源码读 + 符号点击弹窗
                └── ChatPanel.tsx      # 新增：LLM 对话面板
```

### 数据从哪来

构建时 `bundle-graph.mjs` 读项目根的 `.understand-anything/knowledge-graph.json`，连同图谱里每个节点引用到的**源码文件**，一起打包成三个静态文件放进 `public/`：

| 文件 | 内容 |
|------|------|
| `knowledge-graph.json` | 图谱本体（节点 / 边 / 分层 / 导览）|
| `source-content.json`  | `文件路径 → { 源码, 语言, 行数 }` 映射，**源码内嵌**，看代码不再需要 server |
| `meta.json`            | 分析时间、commit hash、版本 |

Vite 构建时 `public/` 原样拷进产物，所以最终站点**完全自包含**，运行期不依赖任何后端、也不依赖 GitHub Raw。

### 三种数据模式（同一份代码三套行为）

UA 原本就有 `DEMO_MODE`，我们补了 `STATIC_MODE`：

- **本地 server 模式**（UA 原版）：token gate + server 实时读文件；
- **STATIC_MODE**（本仓）：跳过 token，`fetch('./knowledge-graph.json')` 等相对路径加载，源码从内嵌的 `source-content.json` 取 —— 这是 Pages 用的模式。

### 三个新功能

1. **LLM 对话（ChatPanel）**
   - 默认 DeepSeek（`deepseek-chat`），兼容任意 OpenAI 格式接口；Key/模型/地址存浏览器 `localStorage`，不上传任何服务器；
   - 每次提问用图谱的模糊搜索（Fuse.js）挑出最相关的节点，把**项目概览 + 分层 + 相关节点 + 相关源码**塞进 system prompt，所以它既能答架构层面的问题，也能答具体代码实现；
   - 支持**多会话**（新建/切换/删除），整段历史持久化在 `localStorage`；
   - LLM 回答里的 `[@节点ID]` 渲染成可点击按钮，点了在图谱定位并打开源码；行内写的文件路径（可带 `:行号`）也可点击跳转。

2. **代码符号跳转（CodeViewer）**
   把图谱的 edges 当轻量 LSP 用：
   - 源码里函数/类/模块名渲染成可点击（虚线下划线）；
   - 点一下弹小菜单：**跳转定义**（同名节点）/ **查找引用**（指向它的入边，如 `calls`/`imports`）；
   - 同名符号按当前文件**扩展名过滤**，避免 TS 的 `Message` 混进 Rust 的 `Message`；排除 `contains`/`exports` 这类结构边，只留真正的使用引用。

3. **移动端适配**
   底部导航多一个 Chat tab；对话列表用浮层不挤压主区；点击区放大到 ≥40px + `active` 反馈。

---

## 三、使用

### 本地预览

```bash
cd apps/pages
pnpm install
pnpm dev          # 自动 build core → 打包图谱 → 起 dev server
# 打开 http://localhost:5173
```

`pnpm dev` 会从 `apps/pages/../../.understand-anything/`（即 hebbian 仓库根）找图谱。前提是该项目已经跑过 UA 的 `/understand` 生成了图谱。

### 构建静态产物

```bash
pnpm build:ghpages
# 产物在 packages/dashboard/dist-ghpages/，可直接静态托管
```

### 自动部署（GitHub Action）

`.github/workflows/deploy-pages.yml`：push 到 `main`（且改了图谱或 `apps/pages/**`）时自动跑——装依赖 → build core → 打包图谱 → vite 构建 → 用 `peaceiris/actions-gh-pages` 推到 `gh-pages` 分支。

**首次**需要手动启用一次 Pages（见上文「踩的坑」），之后全自动。

---

## 四、能不能做成支持任意仓库的独立工具？

**能，而且代价不大。** 整套东西对外的唯一契约就是那份 `knowledge-graph.json`（外加可选的内嵌源码），跟 hebbian 本身没有任何耦合。把它从「hebbian 的一个子目录」抽成「谁都能用的工具」，有三条由轻到重的路线：

### 路线 A：模板仓库 / 脚手架（最快）

把 `apps/pages/` 整理成一个模板仓库，或做成 `npx create-ua-pages`：

```bash
npx create-ua-pages   # 在目标仓库生成 pages/ + .github/workflows/deploy-pages.yml
```

用户在自己仓库跑过 UA 生成图谱后，push 即自动发布。
- 优点：实现成本最低，用户能改源码定制。
- 缺点：每个仓库各拷一份 dashboard 源码，UA 升级要逐个同步。

### 路线 B：预构建的 Dashboard 包 + 一条 Action（推荐）

把 dashboard 构建产物发成一个 npm 包（或 release 资产），用户仓库里只留薄薄一层：

```yaml
# .github/workflows/pages.yml
- uses: <org>/ua-pages-action@v1
  with:
    graph-dir: .understand-anything   # 图谱位置
    source: embed                     # embed=内嵌源码 / raw=运行时从 GitHub Raw 拉
```

这个复合 Action 内部做：定位图谱 → 打包 → 注入预构建前端 → 部署。
- 优点：用户仓库零前端代码；升级只需 bump action 版本。
- 缺点：定制 UI 要 fork。

### 路线 C：UA 官方内置 `--static` 导出（最彻底）

直接给 UA 的 dashboard 命令加一个 `understand dashboard --static --out ./site` 子命令，产出一份可直接托管的静态站点。本仓的 `STATIC_MODE` + `bundle-graph` 本质就是这个能力的原型，把它回流进 UA 上游最一劳永逸。

### 需要参数化的点（任意仓库通用要解决的）

| 维度 | 现状（hebbian 写死） | 通用化做法 |
|------|---------------------|-----------|
| 图谱位置 | `../../.understand-anything/` | CLI/Action 入参 `graph-dir` |
| 部署子路径 base | `./`（已经是相对路径，天然通用）| 无需改 |
| 源码来源 | 全量内嵌 `source-content.json` | 二选一：**embed**（自包含、体积大）或 **raw**（按 commit hash 从 `raw.githubusercontent.com` 拉，体积小、但私有仓需 token）|
| 默认 LLM | DeepSeek | 配置项，UI 已支持任意 OpenAI 兼容接口 |
| 仓库名/owner | 无硬依赖 | 仅 raw 模式需要，从 git remote 推断即可 |

### 体积权衡（唯一要认真对待的工程问题）

内嵌源码让站点完全自包含，但 `source-content.json` 会随项目变大（hebbian 1500+ 文件约几 MB）。两个方向：

- **embed**：适合中小项目、要离线/自包含；
- **raw**：只打包图谱（通常几百 KB），源码运行时按 `gitCommitHash` 从 GitHub Raw 拉——版本精确对齐、站点极小，代价是依赖仓库公开可读。

> 结论：本仓已经把「UA 图谱 → 自包含静态站点」这条路打通并验证。要支持任意仓库，**不需要重写**，只需把写死的图谱路径和源码来源参数化，按路线 B 封装成一个 Action + 预构建包即可。

---

## 五、技术栈

React 19 · Vite 6 · TailwindCSS 4 · React Flow（图）· Fuse.js（搜索）· prism-react-renderer（高亮）· react-markdown（对话渲染）· Zustand（状态）。
