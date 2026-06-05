import AppKit
import SwiftUI

/// NSHostingView 子类：首次点击（窗口未激活时）直接命中 SwiftUI 内容，
/// 不被 nonactivating panel 吞掉用于激活——这样多个通知窗口之间无需先点一下激活再点。
final class FirstMouseHostingView<Content: View>: NSHostingView<Content> {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

/// A floating NSPanel that hosts a CardView. One instance per notification.
final class PanelController: NSObject {
    let msgId: String
    let theme: CardTheme
    private(set) var panel: HebIslandPanel?
    private var hostingView: FirstMouseHostingView<AnyView>?

    var onResult: ((ActionResult) -> Void)?
    var onDismiss: (() -> Void)?
    /// Called (synchronously, on main) after fold/expand toggles so NotificationManager
    /// can re-stack everyone in one pass — fold/expand never positions itself.
    var onRelayout: (() -> Void)?

    private var dismissTimer: Timer?
    private var remainingMs: Int = 0
    private var hoverPaused = false
    private var isClosing = false

    // Fold state
    var isFolded = false
    private var currentCard: NotificationCard
    /// Content height of the expanded card; relayout uses it to size the panel.
    private(set) var expandedHeight: CGFloat = 0

    // Drag & snap — homeOrigin set by NotificationManager on each relayout
    var homeOrigin: NSPoint = .zero

    // Layout constants — match design.html
    private let foldedSize: CGFloat = 48
    private let cardWidth: CGFloat = 420
    private let snapDistance: CGFloat = 48

    /// The size this panel should have right now, given its fold state.
    var targetSize: NSSize {
        isFolded ? NSSize(width: foldedSize, height: foldedSize)
                 : NSSize(width: cardWidth, height: expandedHeight)
    }

    init(msgId: String, card: NotificationCard, onResult: @escaping (ActionResult) -> Void, onDismiss: @escaping () -> Void) {
        self.msgId = msgId
        self.currentCard = card
        self.theme = CardTheme(cardType: card.cardType)
        self.onResult = onResult
        self.onDismiss = onDismiss
        super.init()

        let hosting = FirstMouseHostingView(rootView: AnyView(makeCardView(card)))
        hosting.setFrameSize(hosting.fittingSize)
        hosting.wantsLayer = true
        hosting.layer?.masksToBounds = true
        self.hostingView = hosting
        self.expandedHeight = hosting.fittingSize.height

        let rect = NSRect(origin: .zero, size: hosting.fittingSize)
        let panel = HebIslandPanel(
            contentRect: rect,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.mainMenuWindow)) + 2)
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = false
        panel.isMovableByWindowBackground = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        panel.contentView = hosting
        panel.delegate = self
        self.panel = panel

        panel.onDragEnd = { [weak self] currentOrigin in
            guard let self = self else { return }
            let dx = abs(currentOrigin.x - self.homeOrigin.x)
            let dy = abs(currentOrigin.y - self.homeOrigin.y)
            if dx < self.snapDistance && dy < self.snapDistance {
                self.animateSnap(to: self.homeOrigin)
            }
        }
    }

    deinit {
        dismissTimer?.invalidate()
        panel?.close()
    }

    // MARK: - View builders

    private func makeCardView(_ card: NotificationCard) -> CardView {
        CardView(
            card: card,
            onResult: { [weak self] result in self?.handleResult(result) },
            onClose: { [weak self] in self?.handleClose() },
            onFold: { [weak self] in self?.toggleFold() },
            onHoverEnter: { [weak self] in self?.hoverEnter() },
            onHoverExit: { [weak self] in self?.hoverExit() }
        )
    }

    private func makeFoldedView() -> FoldedCardView {
        FoldedCardView(
            theme: theme,
            onTap: { [weak self] in self?.toggleFold() },
            onHoverEnter: { [weak self] in self?.hoverEnter() },
            onHoverExit: { [weak self] in self?.hoverExit() }
        )
    }

    // MARK: - Show / Dismiss / Frame

    func show(at frame: NSRect) {
        guard let panel = panel else { return }
        panel.setFrame(frame, display: true)
        panel.orderFront(nil)
        panel.alphaValue = 0
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.2
            panel.animator().alphaValue = 1
        }
    }

    /// Re-position + re-size to the stack slot. Skips while the user is dragging.
    func applyFrame(_ frame: NSRect) {
        guard let panel = panel, !panel.isDragging else { return }
        if abs(panel.frame.origin.x - frame.origin.x) < 0.5,
           abs(panel.frame.origin.y - frame.origin.y) < 0.5,
           abs(panel.frame.width - frame.width) < 0.5,
           abs(panel.frame.height - frame.height) < 0.5 {
            return
        }
        panel.setFrame(frame, display: true)
    }

    var isVisible: Bool { panel?.isVisible ?? false }

    func dismiss(animated: Bool = true) {
        guard let panel = panel, !isClosing else { return }
        isClosing = true
        dismissTimer?.invalidate()
        dismissTimer = nil

        if animated {
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0.4
                ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
                panel.animator().alphaValue = 0
                var frame = panel.frame
                frame.origin.x += 80
                panel.animator().setFrame(frame, display: true)
            } completionHandler: { [weak self] in
                self?.closePanel()
            }
        } else {
            closePanel()
        }
    }

    private func closePanel() {
        onDismiss?()
        panel?.close()
        hostingView = nil
        panel = nil
    }

    // MARK: - Update / Auto-dismiss

    func startAutoDismiss(afterMs: Int) {
        remainingMs = afterMs
        scheduleTimer()
    }

    func updateCard(_ card: NotificationCard) {
        currentCard = card
        guard let hosting = hostingView, !isFolded else { return }
        hosting.rootView = AnyView(makeCardView(card))
        hosting.layoutSubtreeIfNeeded()
        expandedHeight = hosting.fittingSize.height
        onRelayout?()
    }

    private func scheduleTimer() {
        dismissTimer?.invalidate()
        guard remainingMs > 0 else { return }
        dismissTimer = Timer.scheduledTimer(withTimeInterval: TimeInterval(remainingMs) / 1000.0, repeats: false) { [weak self] _ in
            guard let self = self, !self.hoverPaused else { return }
            DispatchQueue.main.async { self.handleResult(ActionResult(action: "dismiss")) }
        }
    }

    private func hoverEnter() {
        guard let timer = dismissTimer, timer.isValid else { return }
        hoverPaused = true
        let elapsed = timer.fireDate.timeIntervalSince(Date())
        remainingMs = max(500, Int(elapsed * -1000))
        timer.invalidate()
    }

    private func hoverExit() {
        hoverPaused = false
        scheduleTimer()
    }

    // MARK: - Actions

    /// Returns true if the current click should be suppressed (was a drag).
    private func suppressDragClick() -> Bool {
        guard let panel = panel, panel.wasDragged else { return false }
        panel.wasDragged = false
        return true
    }

    private func handleResult(_ result: ActionResult) {
        guard !isClosing else { return }
        if suppressDragClick() { return }
        dismissTimer?.invalidate()
        dismissTimer = nil
        onResult?(result)
        dismiss(animated: true)
    }

    private func handleClose() {
        guard !isClosing else { return }
        if suppressDragClick() { return }
        handleResult(ActionResult(action: "dismiss"))
    }

    // MARK: - Fold / Expand

    func toggleFold() {
        guard let hosting = hostingView, !isClosing else { return }
        if suppressDragClick() { return }   // a drag must not trigger fold/expand

        isFolded.toggle()
        if isFolded {
            hosting.rootView = AnyView(makeFoldedView())
        } else {
            hosting.rootView = AnyView(makeCardView(currentCard))
            hosting.layoutSubtreeIfNeeded()
            expandedHeight = hosting.fittingSize.height
        }
        onRelayout?()

        hosting.alphaValue = 0
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.15
            hosting.animator().alphaValue = 1
        }
    }

    /// Snap-animate the panel back to its home position.
    /// 必须用 animator().setFrame（NSWindow 的 animator 代理不支持 setFrameOrigin → 无动画）。
    private func animateSnap(to origin: NSPoint) {
        guard let panel = panel else { return }
        var frame = panel.frame
        frame.origin = origin
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.3
            ctx.timingFunction = CAMediaTimingFunction(controlPoints: 0.34, 1.56, 0.64, 1)
            panel.animator().setFrame(frame, display: true)
        }
    }
}

// MARK: - NSWindowDelegate

extension PanelController: NSWindowDelegate {
    func windowWillClose(_ notification: Notification) {
        dismissTimer?.invalidate()
        onDismiss?()
    }
}

// MARK: - Keyable NSPanel with drag support

/// NSPanel subclass that handles drag-to-move, snap detection, and click suppression.
/// Matches design.html drag/snap behavior (DRAG_THRESHOLD=5, SNAP_DISTANCE=48).
final class HebIslandPanel: NSPanel {
    override var canBecomeKey: Bool { true }

    private var dragStartMouse: NSPoint = .zero
    private var dragStartOrigin: NSPoint = .zero
    var isDragging: Bool = false
    var wasDragged: Bool = false

    var onDragEnd: ((NSPoint) -> Void)?

    override func sendEvent(_ event: NSEvent) {
        switch event.type {
        case .leftMouseDown:
            dragStartMouse = NSEvent.mouseLocation
            dragStartOrigin = self.frame.origin
            isDragging = false
            wasDragged = false
            super.sendEvent(event)

        case .leftMouseDragged:
            let current = NSEvent.mouseLocation
            let dx = current.x - dragStartMouse.x
            let dy = current.y - dragStartMouse.y
            if abs(dx) > 5 || abs(dy) > 5 {
                isDragging = true
                wasDragged = true
            }
            if isDragging {
                var newOrigin = dragStartOrigin
                newOrigin.x += dx
                newOrigin.y += dy
                self.setFrameOrigin(newOrigin)
            } else {
                super.sendEvent(event)
            }

        case .leftMouseUp:
            if wasDragged {
                onDragEnd?(self.frame.origin)
            }
            super.sendEvent(event)
            isDragging = false

        default:
            super.sendEvent(event)
        }
    }
}
