import AppKit
import MacKit

/// Tracks the person's own mouse and keyboard, which the agent shares. The agent only yields where the two would
/// collide: it holds off while the person types or has a button down, typing is cut short when they type or click,
/// and a drag is cut short when they move the pointer. Plain pointer movement and scrolling interrupt nothing, since
/// each click names its own position and the app there is checked just before it. Escape ends the agent's turn while
/// it works in the app in front.
/// Events carrying `syntheticTag` are the helper's own and are ignored.
final class Presence {
    static let shared = Presence()

    /// Required keyboard silence before acting: a click mid-word could send the rest of the word elsewhere.
    static let quietTime: TimeInterval = 0.8
    /// Longest wait for that silence, or for a held button to be released.
    static let maxWait: TimeInterval = 5

    enum Input: Hashable {
        case key
        case button
        case pointer
    }

    private let lock = NSLock()
    private var latest: [Input: Date] = [:]
    private var monitor: Any?

    func last(_ input: Input) -> Date? {
        lock.lock()
        defer { lock.unlock() }
        return latest[input]
    }

    /// True when the person produced any of `inputs` since `date`.
    func happened(since date: Date, _ inputs: Input...) -> Bool {
        inputs.contains { input in last(input).map { $0 > date } ?? false }
    }

    static func buttonHeld() -> Bool {
        [CGMouseButton.left, .right, .center].contains { CGEventSource.buttonState(.hidSystemState, button: $0) }
    }

    /// Returns once the keyboard has rested for `quietTime` and no button is held. Throws when the person keeps going
    /// for `maxWait`, or pauses the agent meanwhile.
    func waitForQuiet() throws {
        let deadline = Date().addingTimeInterval(Self.maxWait)
        while true {
            try ControlBar.shared.refuseWhilePaused()
            let typing = last(.key).map { Date().timeIntervalSince($0) < Self.quietTime } ?? false
            let holding = Self.buttonHeld()
            if !typing && !holding { return }
            if Date() > deadline {
                throw HelperFailure("person_active", holding
                    ? "Nothing was done: a mouse button stayed down for \(Int(Self.maxWait)) seconds. Look again with mac_snapshot and carry on only if it still makes sense."
                    : "Nothing was done: the person kept typing for \(Int(Self.maxWait)) seconds. Look again with mac_snapshot and carry on only if it still makes sense.")
            }
            Thread.sleep(forTimeInterval: 0.1)
        }
    }

    /// Installs the monitor on the main thread. A global monitor receives key events only while the app running
    /// Haarness has Accessibility, which actions require anyway.
    func start(onEscape: @escaping () -> Void) {
        let mask: NSEvent.EventTypeMask = [
            .keyDown, .leftMouseDown, .rightMouseDown, .otherMouseDown, .mouseMoved, .leftMouseDragged,
            .rightMouseDragged, .otherMouseDragged,
        ]
        monitor = NSEvent.addGlobalMonitorForEvents(matching: mask) { [weak self] event in
            if event.cgEvent?.getIntegerValueField(.eventSourceUserData) == syntheticTag { return }
            guard let self else { return }
            let input: Input = switch event.type {
            case .keyDown: .key
            case .leftMouseDown, .rightMouseDown, .otherMouseDown: .button
            default: .pointer
            }
            self.lock.lock()
            self.latest[input] = Date()
            self.lock.unlock()
            if event.type == .keyDown && event.keyCode == 0x35 && event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty {
                onEscape()
            }
        }
    }
}
