import AppKit
import QuartzCore

/// Briefly circles the click target. It ignores the mouse, floats above every window and is
/// excluded from screenshots.
final class Marker {
    static let shared = Marker()
    static let color = NSColor(srgbRed: 0.86, green: 0.64, blue: 0.45, alpha: 1)

    var enabled = true
    private var panel: NSPanel?

    /// Shows the ring at a top-left-origin screen point for `seconds`, then hides it.
    func flash(_ x: Double, _ y: Double, seconds: Double = 0.35) {
        guard enabled else { return }
        DispatchQueue.main.sync { self.place(x, y) }
        Thread.sleep(forTimeInterval: seconds)
        DispatchQueue.main.async { self.panel?.orderOut(nil) }
    }

    private func place(_ x: Double, _ y: Double) {
        let side: CGFloat = 46
        // AppKit measures from the main screen's bottom-left corner.
        let screenHeight = NSScreen.screens.first?.frame.height ?? 0
        let frame = NSRect(x: x - side / 2, y: screenHeight - y - side / 2, width: side, height: side)
        let panel = self.panel ?? make(side)
        panel.setFrame(frame, display: true)
        panel.orderFrontRegardless()
    }

    private func make(_ side: CGFloat) -> NSPanel {
        let panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: side, height: side), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.level = .screenSaver
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        panel.sharingType = .none
        let view = NSView(frame: NSRect(x: 0, y: 0, width: side, height: side))
        view.wantsLayer = true
        let halo = CAShapeLayer()
        halo.path = CGPath(ellipseIn: CGRect(x: 2, y: 2, width: side - 4, height: side - 4), transform: nil)
        halo.fillColor = Self.color.withAlphaComponent(0.16).cgColor
        halo.strokeColor = Self.color.withAlphaComponent(0.35).cgColor
        halo.lineWidth = 6
        let ring = CAShapeLayer()
        ring.path = CGPath(ellipseIn: CGRect(x: 6, y: 6, width: side - 12, height: side - 12), transform: nil)
        ring.fillColor = NSColor.clear.cgColor
        ring.strokeColor = Self.color.cgColor
        ring.lineWidth = 2.5
        view.layer?.addSublayer(halo)
        view.layer?.addSublayer(ring)
        panel.contentView = view
        self.panel = panel
        return panel
    }
}
