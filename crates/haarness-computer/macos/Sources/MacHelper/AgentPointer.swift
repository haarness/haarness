import AppKit
import QuartzCore

/// A small pointer in the ring's colour showing where the agent clicks or scrolls in an app behind the person's window,
/// while their own pointer stays where it is. It glides to each spot, the ring marks it, and it fades after a short
/// rest. Like the ring it ignores the mouse, stays out of screenshots and follows the ring switch.
final class AgentPointer {
    static let shared = AgentPointer()

    private static let size = NSSize(width: 22, height: 28)
    private static let glide = 0.22
    private static let rest = 1.6

    private var panel: NSPanel?
    private var hiding: DispatchWorkItem?

    /// Glides the pointer's tip to a top-left-origin screen point and rings it there; returns once it arrived.
    func show(at x: Double, _ y: Double) {
        guard Marker.shared.enabled else { return }
        DispatchQueue.main.sync { self.glide(to: x, y) }
        Thread.sleep(forTimeInterval: Self.glide)
        Marker.shared.flash(x, y, seconds: 0.3)
        DispatchQueue.main.async { self.scheduleHide() }
    }

    // MARK: Main thread

    private func glide(to x: Double, _ y: Double) {
        hiding?.cancel()
        // AppKit measures from the main screen's bottom-left corner; the tip is the panel's top-left corner.
        let screenHeight = NSScreen.screens.first?.frame.height ?? 0
        let target = NSRect(x: x, y: screenHeight - y - Self.size.height, width: Self.size.width, height: Self.size.height)
        let panel = self.panel ?? make()
        if !panel.isVisible {
            panel.setFrame(target.offsetBy(dx: -36, dy: 28), display: false)
            panel.alphaValue = 1
            panel.orderFrontRegardless()
        }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = Self.glide
            context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            panel.animator().setFrame(target, display: true)
        }
    }

    private func scheduleHide() {
        hiding?.cancel()
        let item = DispatchWorkItem { [weak self] in
            guard let panel = self?.panel else { return }
            NSAnimationContext.runAnimationGroup({ context in
                context.duration = 0.25
                panel.animator().alphaValue = 0
            }, completionHandler: {
                panel.orderOut(nil)
                panel.alphaValue = 1
            })
        }
        hiding = item
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.rest, execute: item)
    }

    private func make() -> NSPanel {
        let frame = NSRect(origin: .zero, size: Self.size)
        let panel = NSPanel(contentRect: frame, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.ignoresMouseEvents = true
        panel.level = .screenSaver
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        panel.sharingType = .none
        let view = NSView(frame: frame)
        view.wantsLayer = true
        // An arrow with its tip at the top-left corner, in a bottom-left-origin layer.
        let h = Self.size.height
        let path = CGMutablePath()
        path.move(to: CGPoint(x: 2, y: h - 2))
        path.addLine(to: CGPoint(x: 2, y: h - 22))
        path.addLine(to: CGPoint(x: 7, y: h - 17.5))
        path.addLine(to: CGPoint(x: 10.5, y: h - 25.5))
        path.addLine(to: CGPoint(x: 14, y: h - 24))
        path.addLine(to: CGPoint(x: 10.5, y: h - 16.5))
        path.addLine(to: CGPoint(x: 17, y: h - 16.5))
        path.closeSubpath()
        let arrow = CAShapeLayer()
        arrow.path = path
        arrow.fillColor = Marker.color.cgColor
        arrow.strokeColor = NSColor.white.cgColor
        arrow.lineWidth = 1.5
        arrow.lineJoin = .round
        view.layer?.addSublayer(arrow)
        panel.contentView = view
        self.panel = panel
        return panel
    }
}
