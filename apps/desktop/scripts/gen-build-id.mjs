// 生成一个唯一 build id 写到 workspace 根的 .hebbian-build-id（§7.8.7 版本协商）。
// 由 `pnpm tauri build` 的 beforeBuildCommand 调用——每次构建写一个新值，desktop 与 hebcore
// 的 build.rs 都读这个文件（rerun-if-changed 它），从而：① 每次 tauri build 版本号都不同
// （末尾带随机后缀，同分钟多次构建也不撞）；② desktop 与同次打包的 hebcore 版本号一致
// （都读同一文件），避免版本协商把它俩判成不同版本、启动时反复弹窗。
import { writeFileSync } from 'node:fs'
import { randomBytes } from 'node:crypto'
import { fileURLToPath } from 'node:url'
import path from 'node:path'

const here = path.dirname(fileURLToPath(import.meta.url))
const repoRoot = path.resolve(here, '../../..') // apps/desktop/scripts → workspace 根

const d = new Date()
const p = (n) => String(n).padStart(2, '0')
// 时间戳（可读）+ 4 位随机 hex（末尾随机，保证唯一），如 260628t1530-a3f9。
const id = `${p(d.getFullYear() % 100)}${p(d.getMonth() + 1)}${p(d.getDate())}t${p(d.getHours())}${p(d.getMinutes())}-${randomBytes(2).toString('hex')}`

writeFileSync(path.join(repoRoot, '.hebbian-build-id'), id)
console.log(`[gen-build-id] ${id}`)
