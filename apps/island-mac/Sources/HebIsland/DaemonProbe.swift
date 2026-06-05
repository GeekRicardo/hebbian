import Foundation

/// 探测 socket 上是否已有一个活着的 hebisland daemon（能 connect 成功即视为活）。
/// 用同步 POSIX connect，便于在 NSApplication 启动前 / notify 短进程里直接判断。
func isDaemonAlive(sockPath: String) -> Bool {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    if fd < 0 { return false }
    defer { close(fd) }

    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    let cap = MemoryLayout.size(ofValue: addr.sun_path)
    sockPath.withCString { cstr in
        withUnsafeMutablePointer(to: &addr.sun_path) { rawPtr in
            rawPtr.withMemoryRebound(to: CChar.self, capacity: cap) { dst in
                _ = strlcpy(dst, cstr, cap)
            }
        }
    }

    let len = socklen_t(MemoryLayout<sockaddr_un>.size)
    let result = withUnsafePointer(to: &addr) { ptr -> Int32 in
        ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
            connect(fd, sa, len)
        }
    }
    return result == 0
}

/// 确保 daemon 在跑：已有就直接返回（复用）；没有就 spawn 一个 detached daemon，
/// 再轮询等 socket 变得可连（最多约 5s）。任何调用方推送前调用即可。
func ensureDaemonRunning(sockPath: String) {
    if isDaemonAlive(sockPath: sockPath) { return }

    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: currentExecutablePath())
    proc.arguments = ["daemon"]
    // 切断 stdio，让子 daemon 在本 notify 短进程退出后继续存活。
    proc.standardOutput = FileHandle.nullDevice
    proc.standardError = FileHandle.nullDevice
    proc.standardInput = FileHandle.nullDevice
    do {
        try proc.run()
    } catch {
        return
    }

    for _ in 0..<50 {
        if isDaemonAlive(sockPath: sockPath) { return }
        usleep(100_000) // 100ms
    }
}

/// 当前可执行文件路径，用于 spawn 自身 daemon。arg0 为相对路径时按 CWD 补全。
private func currentExecutablePath() -> String {
    let arg0 = CommandLine.arguments.first ?? "hebisland"
    if arg0.hasPrefix("/") { return arg0 }
    if arg0.contains("/") {
        return FileManager.default.currentDirectoryPath + "/" + arg0
    }
    return arg0
}
