import Foundation
import AppKit

/// CLI dispatch: `hebisland daemon` | `hebisland notify --msg '<json>' [--wait] [--timeout N]`
func main() {
    let args = CommandLine.arguments.dropFirst()
    guard let subcommand = args.first else {
        fputs("Usage: hebisland daemon | notify --msg '<json>' [--wait] [--timeout N]\n", stderr)
        exit(1)
    }
    switch subcommand {
    case "daemon":
        runDaemon()
    case "notify":
        runNotify(args: Array(args.dropFirst()))
    default:
        fputs("Unknown subcommand: \(subcommand)\n", stderr)
        exit(1)
    }
}

func runDaemon() {
    // Ensure ~/.hebbian directory exists
    let hebbianDir = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".hebbian")
    try? FileManager.default.createDirectory(at: hebbianDir, withIntermediateDirectories: true)

    // 单例：已有活 daemon 就直接退出复用，避免 unlink 抢占在跑的 socket。
    let sockPath = hebbianDir.appendingPathComponent("island.sock").path
    if isDaemonAlive(sockPath: sockPath) {
        print("hebisland daemon already running, reusing")
        exit(0)
    }

    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)

    let delegate = AppDelegate()
    app.delegate = delegate

    _ = NSApplicationMain(CommandLine.argc, CommandLine.unsafeArgv)
}

func runNotify(args: [String]) {
    var msgJson: String?
    var shouldWait = false
    var timeoutSec = 60.0

    var i = 0
    while i < args.count {
        switch args[i] {
        case "--msg":
            i += 1
            guard i < args.count else {
                fputs("Error: --msg requires a value\n", stderr)
                exit(1)
            }
            msgJson = args[i]
        case "--wait":
            shouldWait = true
        case "--timeout":
            i += 1
            guard i < args.count, let t = Double(args[i]) else {
                fputs("Error: --timeout requires a numeric value\n", stderr)
                exit(1)
            }
            timeoutSec = t
        default:
            break
        }
        i += 1
    }

    guard let msgJson = msgJson else {
        fputs("Error: --msg is required\n", stderr)
        exit(1)
    }

    NotifyClient.send(json: msgJson, wait: shouldWait, timeout: timeoutSec)
}

main()
