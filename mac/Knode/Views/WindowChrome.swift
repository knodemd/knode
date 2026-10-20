import AppKit
import SwiftUI

/// The main window without the system title bar: the content runs under it, the window takes
/// the background colour, and the traffic lights sit centred in our own 52 pt top bar.
struct WindowChrome: NSViewRepresentable {
    static let barHeight: CGFloat = 52
    /// Room the traffic lights take at the leading edge of the top bar.
    static let trafficLightsWidth: CGFloat = 78

    func makeNSView(context: Context) -> WindowChromeView { WindowChromeView() }
    func updateNSView(_ view: WindowChromeView, context: Context) { view.placeButtons() }
}

final class WindowChromeView: NSView {
    private var observers: [NSObjectProtocol] = []

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers = []
        guard let window else { return }
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.styleMask.insert(.fullSizeContentView)
        window.titlebarSeparatorStyle = .none
        window.backgroundColor = KColor.ns(\.bg)
        let names: [Notification.Name] = [NSWindow.didResizeNotification, NSWindow.didEndLiveResizeNotification,
                                          NSWindow.didExitFullScreenNotification, NSWindow.didBecomeKeyNotification,
                                          NSWindow.didResignKeyNotification]
        for name in names {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.placeButtons() }
            })
        }
        placeButtons()
    }

    override func layout() {
        super.layout()
        placeButtons()
    }

    /// Makes the title bar area as tall as our top bar and centres the traffic lights in it.
    func placeButtons() {
        guard let window, !window.styleMask.contains(.fullScreen),
              let close = window.standardWindowButton(.closeButton),
              let titlebar = close.superview, let container = titlebar.superview else { return }
        let height = WindowChrome.barHeight
        if container.frame.height != height {
            var frame = container.frame
            frame.origin.y += frame.height - height
            frame.size.height = height
            container.frame = frame
            titlebar.frame = container.bounds
        }
        var x: CGFloat = 20
        for type in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            guard let button = window.standardWindowButton(type) else { continue }
            button.setFrameOrigin(NSPoint(x: x, y: ((height - button.frame.height) / 2).rounded()))
            x += button.frame.width + 6
        }
    }
}
