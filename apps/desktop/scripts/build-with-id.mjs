// 生成一个共享 HEBBIAN_BUILD_ID，喂给 Desktop 编译。
//
//   pnpm app:build  → release 出包（tauri build）
//   pnpm app:dev    → 开发（tauri dev）
import { execSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import path from 'node:path'
import { randomBytes } from 'node:crypto'

const here = path.dirname(fileURLToPath(import.meta.url))
const desktopDir = path.resolve(here, '..') // apps/desktop

const mode = process.argv[2] === 'dev' ? 'dev' : 'build'

const d = new Date()
const p = (n) => String(n).padStart(2, '0')
const rand = randomBytes(2).toString('hex')
const id = `${p(d.getFullYear() % 100)}${p(d.getMonth() + 1)}${p(d.getDate())}t${p(d.getHours())}${p(d.getMinutes())}-${rand}`
const env = { ...process.env, HEBBIAN_BUILD_ID: id }
console.log(`[build-with-id] HEBBIAN_BUILD_ID=${id} mode=${mode}`)

const run = (cmd, cwd) => execSync(cmd, { stdio: 'inherit', env, cwd })

if (mode === 'build') {
  run('pnpm tauri build', desktopDir)
} else {
  run('pnpm tauri dev', desktopDir)
}
