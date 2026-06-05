import Foundation
import Network

/// Unix domain socket server: listens on ~/.hebbian/island.sock, handles line-delimited JSON protocol.
final class SocketServer {
    private let sockPath: String
    private var listener: NWListener?
    private let queue = DispatchQueue(label: "com.hebbian.island.socket")

    /// Called on main thread when a "show" message arrives.
    var onShow: ((NotificationCard, NWConnection) -> Void)?
    /// Called on main thread when a "dismiss" message arrives.
    var onDismiss: ((String) -> Void)?

    /// Map msgId -> connection for action write-back
    private var connByMsgId: [String: NWConnection] = [:]
    private let mapLock = NSLock()

    init(sockPath: String) {
        self.sockPath = sockPath
    }

    func start() {
        preconditionSocketClean()

        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.unix(path: sockPath)

        do {
            listener = try NWListener(using: params)
        } catch {
            fputs("[SocketServer] Failed to create listener: \(error)\n", stderr)
            return
        }

        listener?.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                self?.fixSocketPermissions()
                print("[SocketServer] Listening on \(self?.sockPath ?? "")")
            case .failed(let error):
                fputs("[SocketServer] Listener failed: \(error)\n", stderr)
                self?.restart()
            case .cancelled:
                print("[SocketServer] Listener cancelled")
            default:
                break
            }
        }

        listener?.newConnectionHandler = { [weak self] connection in
            self?.handleNewConnection(connection)
        }

        listener?.start(queue: queue)
    }

    /// Write an action message back to the connection that originated a notification.
    func writeAction(msgId: String, result: ActionResult) {
        mapLock.lock()
        let conn = connByMsgId[msgId]
        mapLock.unlock()

        guard let conn = conn else {
            print("[SocketServer] No connection for msgId=\(msgId), dropping action=\(result.action)")
            return
        }
        let msg = ActionMessage(msgId: msgId, result: result)
        guard let line = msg.toJSONLine(), let data = line.data(using: .utf8) else { return }
        conn.send(content: data, completion: .contentProcessed({ _ in }))
    }

    /// Dismiss a notification (no action write-back, just remove tracking)
    func untrack(msgId: String) {
        mapLock.lock()
        connByMsgId.removeValue(forKey: msgId)
        mapLock.unlock()
    }

    // MARK: - Private

    private func preconditionSocketClean() {
        // Remove stale socket file
        unlink(sockPath)

        // Set umask so socket gets created with 0700
        let savedUmask = umask(0o077)
        // Defer: umask will be restored after bind (in .ready handler)
    }

    private func fixSocketPermissions() {
        // Ensure socket file is 0700 (only current user)
        chmod(sockPath, 0o700)
    }

    private func restart() {
        listener?.cancel()
        DispatchQueue.global().asyncAfter(deadline: .now() + 1) { [weak self] in
            self?.start()
        }
    }

    private func handleNewConnection(_ connection: NWConnection) {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready:
                self?.readLines(from: connection)
            case .failed, .cancelled:
                self?.cleanupConnection(connection)
            default:
                break
            }
        }
        connection.start(queue: queue)
    }

    private func readLines(from connection: NWConnection, buffer: String = "") {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            if let error = error, !isComplete {
                print("[SocketServer] Read error: \(error)")
                self?.cleanupConnection(connection)
                return
            }
            var buf = buffer
            if let data = data, let chunk = String(data: data, encoding: .utf8) {
                buf.append(chunk)
            }
            let parts = buf.components(separatedBy: "\n")
            let completeLines = parts.dropLast()
            let remainder = parts.last ?? ""

            for line in completeLines where !line.isEmpty {
                self?.processLine(line, from: connection)
            }

            if isComplete {
                if !remainder.isEmpty {
                    self?.processLine(remainder, from: connection)
                }
                self?.cleanupConnection(connection)
            } else {
                self?.readLines(from: connection, buffer: remainder)
            }
        }
    }

    private func processLine(_ line: String, from connection: NWConnection) {
        guard let data = line.data(using: .utf8) else { return }
        do {
            let msg = try JSONDecoder().decode(IncomingMessage.self, from: data)
            switch msg.type {
            case "show":
                guard let card = msg.card else { return }
                mapLock.lock()
                connByMsgId[card.id] = connection
                mapLock.unlock()
                DispatchQueue.main.async { [weak self] in
                    self?.onShow?(card, connection)
                }
            case "dismiss":
                guard let id = msg.id else { return }
                DispatchQueue.main.async { [weak self] in
                    self?.onDismiss?(id)
                }
            default:
                break
            }
        } catch {
            print("[SocketServer] Invalid JSON: \(line.prefix(80)) — \(error)")
        }
    }

    private func cleanupConnection(_ connection: NWConnection) {
        mapLock.lock()
        // Remove all entries for this connection
        connByMsgId = connByMsgId.filter { $0.value !== connection }
        mapLock.unlock()
        connection.cancel()
    }
}
