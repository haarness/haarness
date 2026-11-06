import AppKit
import MacKit

/// The status strip along the screen's top edge during an agent's work: it names the app in use, with Pause and
/// Stop buttons. Pausing returns control until Resume; Stop cancels the turn, as Escape does. Clicking it
/// leaves keyboard focus where it was, and screenshots never include it.
final class ControlBar {
    static let shared = ControlBar()

    /// Seconds the bar stays up after the latest request unless paused; the server removes it when the turn ends.
    static let linger: TimeInterval = 45

    /// Callback for the buttons: `pause`, `resume` or `stop`. Assigned once at launch.
    var report: (String) -> Void = { _ in }

    private let lock = NSLock()
    private var held = false
    private var showing = false
    private var appName = ""
    private var words = BarText.english
    private var panel: NSPanel?
    private var label: NSTextField?
    private var toggle: NSButton?
    private var stop: NSButton?
    private var dot: NSView?
    private var timer: Timer?

    var isPaused: Bool {
        lock.lock()
        defer { lock.unlock() }
        return held
    }

    func refuseWhilePaused() throws {
        if isPaused { throw HelperFailure("paused", "Skipped: the agent is paused.") }
    }

    /// Takes the words for the person's language from a request.
    func use(_ text: BarText) {
        lock.lock()
        let changed = text != words
        words = text
        lock.unlock()
        if changed { DispatchQueue.main.async { self.redraw() } }
    }

    /// Puts the bar up, naming `app` when it is known, and restarts the `linger` timer.
    func show(app: String?) {
        lock.lock()
        if let app, !app.isEmpty { appName = app }
        showing = true
        lock.unlock()
        DispatchQueue.main.async { self.redraw() }
    }

    /// Takes the bar down and clears any pause, because the turn has finished.
    func hide() {
        lock.lock()
        showing = false
        held = false
        appName = ""
        lock.unlock()
        DispatchQueue.main.async {
            self.timer?.invalidate()
            self.panel?.orderOut(nil)
        }
    }

    /// The bar's frame in top-left-origin screen points, while it is visible.
    func frame() -> Box? {
        let read = { () -> Box? in
            guard let panel = self.panel, panel.isVisible else { return nil }
            let screenHeight = NSScreen.screens.first?.frame.height ?? 0
            let rect = panel.frame
            return Box(x: rect.origin.x, y: screenHeight - rect.origin.y - rect.height, width: rect.width, height: rect.height)
        }
        return Thread.isMainThread ? read() : DispatchQueue.main.sync(execute: read)
    }

    /// Temporarily hides the bar if it covers `x`,`y`, so a pointer action there reaches the app below.
    func outOfTheWay<T>(_ x: Double, _ y: Double, _ body: () throws -> T) rethrows -> T {
        guard let box = frame(), box.grown(by: 10).holds(x, y) else { return try body() }
        DispatchQueue.main.sync { self.panel?.orderOut(nil) }
        defer { DispatchQueue.main.async { self.redraw() } }
        return try body()
    }

    // MARK: Rendering (main thread only)

    private func redraw() {
        lock.lock()
        let (visible, paused, name, text) = (showing, held, appName, words)
        lock.unlock()
        timer?.invalidate()
        guard visible else {
            panel?.orderOut(nil)
            return
        }
        let panel = self.panel ?? build()
        label?.stringValue = paused ? text.paused : text.label(for: name)
        toggle?.title = paused ? text.resume : text.pause
        stop?.title = text.stop
        stop?.toolTip = text.stopHint
        dot?.layer?.backgroundColor = (paused ? NSColor.systemGray : Marker.color).cgColor
        panel.contentView?.layoutSubtreeIfNeeded()
        let size = panel.contentView?.fittingSize ?? NSSize(width: 320, height: 36)
        let area = NSScreen.screens.first?.visibleFrame ?? .zero
        panel.setFrame(NSRect(x: area.midX - size.width / 2, y: area.maxY - size.height - 10, width: size.width, height: size.height), display: true)
        panel.orderFrontRegardless()
        // A paused bar remains until Resume or the end of the turn.
        if !paused {
            timer = Timer.scheduledTimer(withTimeInterval: Self.linger, repeats: false) { [weak self] _ in
                guard let self else { return }
                self.lock.lock()
                let idle = !self.held
                if idle { self.showing = false }
                self.lock.unlock()
                if idle { self.panel?.orderOut(nil) }
            }
        }
    }

    private func build() -> NSPanel {
        let panel = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 320, height: 36), styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
        panel.sharingType = .none
        panel.becomesKeyOnlyIfNeeded = true
        panel.hidesOnDeactivate = false
        panel.appearance = NSAppearance(named: .darkAqua)

        let back = NSVisualEffectView()
        back.material = .hudWindow
        back.state = .active
        back.blendingMode = .behindWindow
        back.wantsLayer = true
        back.layer?.cornerRadius = 18
        back.layer?.masksToBounds = true

        let dot = NSView()
        dot.wantsLayer = true
        dot.layer?.cornerRadius = 4
        dot.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([dot.widthAnchor.constraint(equalToConstant: 8), dot.heightAnchor.constraint(equalToConstant: 8)])

        let label = NSTextField(labelWithString: "")
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.textColor = .labelColor
        label.lineBreakMode = .byTruncatingTail
        label.translatesAutoresizingMaskIntoConstraints = false
        label.widthAnchor.constraint(lessThanOrEqualToConstant: 360).isActive = true

        let toggle = FirstClickButton(title: "", target: self, action: #selector(togglePressed(_:)))
        let stop = FirstClickButton(title: "", target: self, action: #selector(stopPressed(_:)))
        for button in [toggle, stop] {
            button.bezelStyle = .rounded
            button.controlSize = .small
            button.font = .systemFont(ofSize: 11, weight: .medium)
        }

        let row = NSStackView(views: [dot, label, toggle, stop])
        row.orientation = .horizontal
        row.alignment = .centerY
        row.spacing = 6
        row.setCustomSpacing(10, after: dot)
        row.setCustomSpacing(14, after: label)
        row.edgeInsets = NSEdgeInsets(top: 6, left: 14, bottom: 6, right: 8)
        row.translatesAutoresizingMaskIntoConstraints = false
        back.addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: back.leadingAnchor),
            row.trailingAnchor.constraint(equalTo: back.trailingAnchor),
            row.topAnchor.constraint(equalTo: back.topAnchor),
            row.bottomAnchor.constraint(equalTo: back.bottomAnchor),
        ])
        panel.contentView = back
        self.panel = panel
        self.label = label
        self.toggle = toggle
        self.stop = stop
        self.dot = dot
        return panel
    }

    @objc private func togglePressed(_ sender: Any?) {
        lock.lock()
        held.toggle()
        let now = held
        lock.unlock()
        report(now ? "pause" : "resume")
        redraw()
    }

    @objc private func stopPressed(_ sender: Any?) {
        hide()
        report("stop")
    }
}

/// A button that answers the first click although its panel never becomes key.
private final class FirstClickButton: NSButton {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}
