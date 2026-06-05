import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem?
    private var socketServer: SocketServer?
    private var notificationManager: NotificationManager?
    private var screenResolver: ScreenResolver?

    func applicationDidFinishLaunching(_ notification: Notification) {
        setupStatusBar()
        setupSocket()

        // Start screen resolver after a short delay to allow the app to be fully launched
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
            self?.screenResolver?.start()
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        screenResolver?.stop()
    }

    private func setupStatusBar() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem?.button {
            button.title = "🏝"
            button.toolTip = "HebIsland daemon"
        }
        let menu = NSMenu()
        menu.addItem(NSMenuItem(title: "Listening: ~/.hebbian/island.sock", action: nil, keyEquivalent: ""))
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        statusItem?.menu = menu
    }

    private func setupSocket() {
        let sockPath = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".hebbian/island.sock").path

        let resolver = ScreenResolver()
        self.screenResolver = resolver

        let server = SocketServer(sockPath: sockPath)
        self.socketServer = server

        let mgr = NotificationManager(socketServer: server, screenResolver: resolver)
        self.notificationManager = mgr

        // Wire up callbacks
        server.onShow = { [weak mgr] card, conn in
            mgr?.show(card: card, connection: conn)
        }
        server.onDismiss = { [weak mgr] id in
            mgr?.dismiss(id: id)
        }

        resolver.onScreenChange = { [weak mgr] _ in
            mgr?.relayout()
        }

        server.start()
    }
}
