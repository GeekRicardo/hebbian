// 生成一个共享 HEBBIAN_BUILD_ID（§7.8.7 版本协商），同时喂给 hebcore 与 desktop 的编译，
// 使同一次构建的两个 binary 注入相同版本号。
//
//   pnpm app:build  → release 出包（cargo build --release -p hebcore + tauri build）
//   pnpm app:dev    → 开发（cargo build -p hebcore + tauri dev）
//
// 为什么要 wrapper：desktop 和 hebcore 是两个独立 binary、各自 build.rs，没有共享的"这次
// 构建"标识；外层注入同一个 HEBBIAN_BUILD_ID 环境变量是让它们版本号一致的唯一干净办法。
import { execSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const here = path.dirname(fileURLToPath(import.meta.url))
const desktopDir = path.resolve(here, '..') // apps/desktop
const repoRoot = path.resolve(desktopDir, '../..') // workspace root

const mode = process.argv[2] === 'dev' ? 'dev' : 'build'

// 紧凑时间戳，如 260627t1430（年后两位 月 日 t 时 分）。每次构建唯一。
const d = new Date()
const p = (n) => String(n).padStart(2, '0')
const id = `${p(d.getFullYear() % 100)}${p(d.getMonth() + 1)}${p(d.getDate())}t${p(d.getHours())}${p(d.getMinutes())}`
const env = { ...process.env, HEBBIAN_BUILD_ID: id }
console.log(`[build-with-id] HEBBIAN_BUILD_ID=${id} mode=${mode}`)

const run = (cmd, cwd) => execSync(cmd, { stdio: 'inherit', env, cwd })

if (mode === 'build') {
  run('cargo build --release -p hebcore', repoRoot)
  run('pnpm tauri build', desktopDir)
} else {
  run('cargo build -p hebcore', repoRoot)
  run('pnpm tauri dev', desktopDir)
}
