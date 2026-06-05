import Foundation
import Network

/// Handles the `hebisland notify` subcommand: connect to daemon socket, send JSON, optionally wait for action.
enum NotifyClient {
    static func send(json: String, wait: Bool, timeout: TimeInterval) {
        let sockPath = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".hebbian/island.sock").path

        // Validate JSON is parseable
        guard let data = json.data(using: .utf8),
              let _ = try? JSONDecoder().decode(IncomingMessage.self, from: data) else {
            fputs("Error: invalid message JSON\n", stderr)
            exit(1)
        }

        // 没有 daemon 就自动拉起一个，有就复用。
        ensureDaemonRunning(sockPath: sockPath)

        let endpoint = NWEndpoint.unix(path: sockPath)
        let connection = NWConnection(to: endpoint, using: .tcp)

        let semaphore = DispatchSemaphore(value: 0)
        var responseLine: String?
        var connectError: String?
        let readLock = NSLock()

        connection.stateUpdateHandler = { state in
            switch state {
            case .ready:
                // Write the message line
                var line = json
                if !line.hasSuffix("\n") { line.append("\n") }
                connection.send(content: line.data(using: .utf8), completion: .contentProcessed({ _ in
                    if !wait {
                        // Fire-and-forget: close after write
                        connection.cancel()
                        semaphore.signal()
                    }
                }))
                if wait {
                    // Start receiving
                    receiveLine(connection: connection, lock: readLock) { line in
                        responseLine = line
                        connection.cancel()
                        semaphore.signal()
                    }
                }
            case .failed(let error):
                connectError = error.localizedDescription
                connection.cancel()
                semaphore.signal()
            case .cancelled:
                semaphore.signal()
            default:
                break
            }
        }

        connection.start(queue: .global())

        let deadline: DispatchTime
        if wait {
            deadline = .now() + timeout
            _ = semaphore.wait(timeout: deadline)
        } else {
            _ = semaphore.wait(timeout: .now() + 5) // short timeout for fire-and-forget
        }

        if let err = connectError {
            fputs("Error: 请先运行 hebisland daemon (\(err))\n", stderr)
            exit(1)
        }

        if wait {
            if let line = responseLine {
                print(line.trimmingCharacters(in: .newlines))
                exit(0)
            } else {
                fputs("Error: timeout waiting for response\n", stderr)
                connection.cancel()
                exit(1)
            }
        } else {
            exit(0)
        }
    }

    private static func receiveLine(connection: NWConnection, lock: NSLock, buffer: String = "", onLine: @escaping (String) -> Void) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { data, _, isComplete, error in
            if let error = error, !isComplete {
                fputs("Error: socket read failed: \(error)\n", stderr)
                return
            }
            var buf = buffer
            if let data = data, let chunk = String(data: data, encoding: .utf8) {
                buf.append(chunk)
            }
            lock.lock()
            let parts = buf.components(separatedBy: "\n")
            lock.unlock()
            if parts.count > 1, let first = parts.first, !first.isEmpty {
                onLine(first)
            } else if isComplete {
                onLine(buf)
            } else {
                let remaining = parts.count > 1 ? parts.dropFirst().joined(separator: "\n") : buf
                receiveLine(connection: connection, lock: lock, buffer: remaining, onLine: onLine)
            }
        }
    }
}
