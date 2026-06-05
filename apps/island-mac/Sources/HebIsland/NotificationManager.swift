import AppKit
import Network

/// Manages notification lifecycle: create/dismiss/stack/update panels.
/// All UI work must happen on main thread.
final class NotificationManager {
    private var controllers: [PanelController] = []
    private let socketServer: SocketServer
    private let screenResolver: ScreenResolver

    /// Max info-type notifications visible at once; extras get collapsed.
    private let maxInfoPanels = 5

    // Layout constants — matches design.html script
    private let margin: CGFloat = 20
    private let gap: CGFloat = 10
    private let foldedSize: CGFloat = 48
    private let cardWidth: CGFloat = 420

    init(socketServer: SocketServer, screenResolver: ScreenResolver) {
        self.socketServer = socketServer
        self.screenResolver = screenResolver
    }

    /// Show a notification card. Called on main thread from SocketServer.
    func show(card: NotificationCard, connection: NWConnection) {
        DispatchQueue.main.async { [weak self] in
            self?._showOnMain(card: card, connection: connection)
        }
    }

    /// Dismiss a notification by ID. Called on main thread from SocketServer.
    func dismiss(id: String) {
        DispatchQueue.main.async { [weak self] in
            self?._dismissOnMain(id: id, sendAction: false)
        }
    }

    /// Relayout all panels after a screen change or fold/expand.
    func relayout() {
        DispatchQueue.main.async { [weak self] in
            self?._relayoutOnMain()
        }
    }

    // MARK: - Main-thread implementation

    private func _showOnMain(card: NotificationCard, connection: NWConnection) {
        // Check for duplicate ID -> update existing
        if let existing = controllers.first(where: { $0.msgId == card.id }) {
            existing.updateCard(card)
            if let dur = card.effectiveDurationMs {
                existing.startAutoDismiss(afterMs: dur)
            }
            return
        }

        // Check info panel limit
        let isInfo = card.cardType == "info"
        if isInfo {
            let infoCount = controllers.filter { $0.theme == .info }.count
            if infoCount >= maxInfoPanels {
                if let oldest = controllers.first(where: { $0.theme == .info }) {
                    oldest.dismiss(animated: true)
                    controllers.removeAll { $0 === oldest }
                }
            }
        }

        let controller = PanelController(
            msgId: card.id,
            card: card,
            onResult: { [weak self] result in
                self?.socketServer.writeAction(msgId: card.id, result: result)
                self?.socketServer.untrack(msgId: card.id)
                self?._dismissOnMain(id: card.id, sendAction: false)
            },
            onDismiss: { [weak self] in
                self?.socketServer.untrack(msgId: card.id)
                self?._removeController(id: card.id)
            }
        )

        // Fold/expand restacks everyone synchronously so the toggled panel is sized
        // and positioned in one pass (no expand-then-slide jump).
        controller.onRelayout = { [weak self] in
            self?._relayoutOnMain()
        }

        controllers.append(controller)
        _relayoutOnMain()

        if let dur = card.effectiveDurationMs {
            controller.startAutoDismiss(afterMs: dur)
        }
    }

    private func _dismissOnMain(id: String, sendAction: Bool) {
        guard let controller = controllers.first(where: { $0.msgId == id }) else { return }
        if sendAction {
            socketServer.writeAction(msgId: id, result: ActionResult(action: "dismiss"))
        }
        controller.dismiss(animated: true)
        controllers.removeAll { $0 === controller }
        // Delay relayout to allow dismiss animation
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
            self?._relayoutOnMain()
        }
    }

    private func _removeController(id: String) {
        controllers.removeAll { $0.msgId == id }
        _relayoutOnMain()
    }

    private func _relayoutOnMain() {
        let screen = screenResolver.focusScreen
        let frame = screen.frame

        var yOffset: CGFloat = margin
        for controller in controllers {
            guard controller.panel != nil else { continue }

            // Size is owned by the controller's fold state (folded 48×48, else 420×content).
            let size = controller.targetSize

            // Top-right stacking: x aligns to right edge, y descends from top.
            let x = frame.maxX - margin - size.width
            let y = frame.maxY - margin - yOffset - size.height
            let targetFrame = NSRect(x: x, y: y, width: size.width, height: size.height)

            // Record home anchor for drag snap.
            controller.homeOrigin = NSPoint(x: x, y: y)

            if !controller.isVisible {
                controller.show(at: targetFrame)
            } else {
                controller.applyFrame(targetFrame)
            }
            yOffset += size.height + gap
        }
    }
}
