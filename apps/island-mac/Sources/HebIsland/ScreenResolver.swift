import AppKit
import CoreGraphics

/// Determines which screen the frontmost application window is on,
/// and fires a callback when the focus screen changes.
final class ScreenResolver {
    /// Current focus screen (the one the frontmost window occupies)
    private(set) var focusScreen: NSScreen = NSScreen.main ?? NSScreen.screens.first!

    var onScreenChange: ((NSScreen) -> Void)?

    private var previousScreenDigest: Int = 0
    private var pollTimer: Timer?
    private var observing = false

    func start() {
        guard !observing else { return }
        observing = true

        // Immediate first detection
        detectAndNotify()

        // NSWorkspace notifications
        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(frontAppChanged),
            name: NSWorkspace.didActivateApplicationNotification,
            object: nil
        )

        // Screen parameter changes (monitor connect/disconnect)
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(screenParamsChanged),
            name: NSApplication.didChangeScreenParametersNotification,
            object: nil
        )

        // Polling fallback (500ms)
        pollTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            self?.detectAndNotify()
        }
    }

    func stop() {
        observing = false
        pollTimer?.invalidate()
        pollTimer = nil
        NSWorkspace.shared.notificationCenter.removeObserver(self)
        NotificationCenter.default.removeObserver(self)
    }

    /// Force re-detect and fire callback if screen changed.
    func detectNow() {
        detectAndNotify()
    }

    // MARK: - Private

    @objc private func frontAppChanged() { detectAndNotify() }
    @objc private func screenParamsChanged() { detectAndNotify() }

    private func detectAndNotify() {
        let screen = detectFocusScreen()
        let digest = screen.hashValue
        if digest != previousScreenDigest {
            previousScreenDigest = digest
            focusScreen = screen
            DispatchQueue.main.async { [weak self] in
                guard let self = self else { return }
                self.onScreenChange?(screen)
            }
        }
    }

    /// Detect which screen the frontmost application window occupies.
    private func detectFocusScreen() -> NSScreen {
        let frontApp = NSWorkspace.shared.frontmostApplication
        // Exclude self
        guard let pid = frontApp?.processIdentifier,
              pid != ProcessInfo.processInfo.processIdentifier else {
            return NSScreen.main ?? NSScreen.screens.first!
        }

        let windowList = CGWindowListCopyWindowInfo(
            [.optionOnScreenOnly, .excludeDesktopElements],
            kCGNullWindowID
        ) as? [[String: Any]] ?? []

        for window in windowList {
            guard let windowLayer = window[kCGWindowLayer as String] as? Int,
                  windowLayer == 0,
                  let ownerPID = window[kCGWindowOwnerPID as String] as? pid_t,
                  ownerPID == pid,
                  let boundsDict = window[kCGWindowBounds as String] as? [String: CGFloat],
                  let width = boundsDict["Width"], width > 0,
                  let height = boundsDict["Height"], height > 0,
                  let x = boundsDict["X"],
                  let y = boundsDict["Y"]
            else { continue }

            // Window center point in CG global coordinates (origin at top-left)
            let center = CGPoint(x: x + width / 2, y: y + height / 2)

            // Find which screen contains this point
            for screen in NSScreen.screens {
                let frame = screen.frame
                if frame.contains(NSPoint(x: center.x, y: center.y)) {
                    return screen
                }
            }
        }

        return NSScreen.main ?? NSScreen.screens.first!
    }
}
