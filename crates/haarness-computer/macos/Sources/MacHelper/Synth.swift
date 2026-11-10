import AppKit
import Carbon.HIToolbox
import CoreGraphics
import MacKit

/// Pointer and keyboard events posted the way a real device posts them, each tagged as the helper's own. Keys and text
/// go either through the system, to whatever has the keyboard, or straight to one app's process when it works behind
/// the person's window.
enum Synth {
    static let source = CGEventSource(stateID: .hidSystemState)

    /// Where keyboard events go.
    enum Destination {
        /// The system's input stream, which delivers them to the app in front.
        case system
        /// One app's process, which gets them although it is not in front.
        case process(pid_t)
    }

    static func post(_ event: CGEvent?, to destination: Destination = .system, keyboard: Bool = false) {
        guard let event else { return }
        switch destination {
        case .system:
            event.setIntegerValueField(.eventSourceUserData, value: syntheticTag)
            event.post(tap: .cghidEventTap)
        case .process(let pid):
            WindowServer.post(event, to: pid, keyboard: keyboard)
        }
    }

    static func wait(_ seconds: Double) {
        Thread.sleep(forTimeInterval: seconds)
    }

    static func flags(_ held: [Held]) -> CGEventFlags {
        var out = CGEventFlags()
        for key in held {
            switch key {
            case .command: out.insert(.maskCommand)
            case .shift: out.insert(.maskShift)
            case .option: out.insert(.maskAlternate)
            case .control: out.insert(.maskControl)
            case .fn: out.insert(.maskSecondaryFn)
            }
        }
        return out
    }

    static func keyCode(_ held: Held) -> CGKeyCode {
        switch held {
        case .command: return CGKeyCode(kVK_Command)
        case .shift: return CGKeyCode(kVK_Shift)
        case .option: return CGKeyCode(kVK_Option)
        case .control: return CGKeyCode(kVK_Control)
        case .fn: return CGKeyCode(kVK_Function)
        }
    }

    // MARK: Pointer

    static func hover(_ x: Double, _ y: Double) {
        post(CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: CGPoint(x: x, y: y), mouseButton: .left))
    }

    static func mouseTypes(_ button: String) -> (down: CGEventType, up: CGEventType, which: CGMouseButton, number: Int) {
        switch button {
        case "right": return (.rightMouseDown, .rightMouseUp, .right, 1)
        case "middle": return (.otherMouseDown, .otherMouseUp, .center, 2)
        default: return (.leftMouseDown, .leftMouseUp, .left, 0)
        }
    }

    /// Clicks at a screen point. The pointer arrives first, so controls that only appear on hover exist when it presses.
    static func click(_ x: Double, _ y: Double, button: String, count: Int, held: [Held]) {
        let point = CGPoint(x: x, y: y)
        let (down, up, which, _) = mouseTypes(button)
        hover(x - 2, y - 1)
        wait(0.05)
        hover(x, y)
        wait(0.08)
        let mask = flags(held)
        for number in 1...max(1, count) {
            let press = CGEvent(mouseEventSource: source, mouseType: down, mouseCursorPosition: point, mouseButton: which)
            let release = CGEvent(mouseEventSource: source, mouseType: up, mouseCursorPosition: point, mouseButton: which)
            for event in [press, release] {
                event?.setIntegerValueField(.mouseEventClickState, value: Int64(number))
                // Always set explicitly, or a modifier the person happens to hold would turn this into a Command-click.
                event?.flags = mask
            }
            post(press)
            wait(0.03)
            post(release)
            if number < count { wait(0.06) }
        }
    }

    // The background click and scroll below derive from Cua Driver, https://github.com/trycua/cua, Copyright (c)
    // 2025-2026 Cua AI, Inc., under the MIT License; its text is in ../../THIRD-PARTY-NOTICES.md.

    /// Presses at a screen spot within `window` of `pid` without the system's pointer: the events go to that process
    /// alone, addressed to the window, and the person's pointer and front app stay as they are. Modifiers go down as
    /// keys to the same process around the presses, since some views read those rather than the events' flags.
    static func clickBehind(pid: pid_t, window: CGWindowID, origin: Point, at spot: Point, button: String, count: Int, held: [Held], outsidePress: Bool) {
        let (down, up, which, number) = mouseTypes(button)
        let plan = backgroundClickPlan(at: spot, origin: origin, button: number, count: count, outsidePress: outsidePress, windowLocal: Behind.windowLocal)
        // One gesture number shared by every event, so the app takes them as one click.
        let gesture = Int64(Date().timeIntervalSince1970 * 1000) & 0x7FFF_FFFF
        let mask = flags(held)
        holding(held, at: .process(pid)) {
            for step in plan {
                let type: CGEventType = switch step.kind {
                case .move: .mouseMoved
                case .down: step.button == number ? down : .leftMouseDown
                case .up: step.button == number ? up : .leftMouseUp
                }
                guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: CGPoint(x: step.screen.x, y: step.screen.y), mouseButton: step.kind == .move ? .left : which) else { continue }
                WindowServer.stamp(event, WindowServer.Field.mouseNumber, Int64(step.phase))
                WindowServer.stamp(event, WindowServer.Field.clickState, Int64(step.clickState))
                WindowServer.stamp(event, WindowServer.Field.buttonNumber, Int64(step.button))
                // The touch subtype, as a trackpad press reports.
                WindowServer.stamp(event, WindowServer.Field.subtype, 3)
                WindowServer.stamp(event, WindowServer.Field.targetPid, Int64(pid))
                for field in [WindowServer.Field.windowNumber, WindowServer.Field.windowUnderPointer, WindowServer.Field.windowThatCanHandle] {
                    WindowServer.stamp(event, field, Int64(window))
                }
                WindowServer.stamp(event, WindowServer.Field.gesture, gesture)
                WindowServer.stampLocation(event, CGPoint(x: step.stamped.x, y: step.stamped.y))
                event.flags = mask
                WindowServer.post(event, to: pid, keyboard: false)
                if step.pause > 0 { wait(step.pause) }
            }
        }
    }

    /// Turns the wheel over `pid`'s `window` at a screen spot, one notch per line, without the system's pointer.
    static func scrollBehind(pid: pid_t, window: CGWindowID, origin: Point, at spot: Point, dx: Int32, dy: Int32) {
        let stamped = Behind.windowLocal ? CGPoint(x: spot.x - origin.x, y: spot.y - origin.y) : CGPoint(x: spot.x, y: spot.y)
        let address = { (event: CGEvent) in
            WindowServer.stamp(event, WindowServer.Field.targetPid, Int64(pid))
            for field in [WindowServer.Field.windowNumber, WindowServer.Field.windowUnderPointer, WindowServer.Field.windowThatCanHandle] {
                WindowServer.stamp(event, field, Int64(window))
            }
            WindowServer.stampLocation(event, stamped)
            event.flags = []
        }
        if let move = CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: CGPoint(x: spot.x, y: spot.y), mouseButton: .left) {
            address(move)
            WindowServer.post(move, to: pid, keyboard: false)
        }
        wait(0.03)
        let notches = max(abs(dx), abs(dy))
        for _ in 0..<notches {
            let event = CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2, wheel1: dy.signum(), wheel2: dx.signum(), wheel3: 0)
            guard let event else { continue }
            event.location = CGPoint(x: spot.x, y: spot.y)
            address(event)
            WindowServer.post(event, to: pid, keyboard: false)
            wait(0.012)
        }
    }

    /// Drags between two screen points. If the person moves the pointer or clicks meanwhile, or pauses the agent,
    /// the drop would land somewhere else, so the drag returns to where it began, lets go there and stops.
    static func drag(from start: (Double, Double), to end: (Double, Double)) throws {
        let origin = CGPoint(x: start.0, y: start.1)
        let target = CGPoint(x: end.0, y: end.1)
        let began = Date()
        func event(_ type: CGEventType, _ point: CGPoint) -> CGEvent? {
            let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: point, mouseButton: .left)
            event?.flags = []
            return event
        }
        func interrupted() throws {
            let held = ControlBar.shared.isPaused
            guard held || Presence.shared.happened(since: began, .pointer, .button) else { return }
            post(event(.leftMouseDragged, origin))
            wait(0.05)
            post(event(.leftMouseUp, origin))
            if held { try ControlBar.shared.refuseWhilePaused() }
            throw HelperFailure("person_active", "The drag was undone: the person moved the pointer while it ran, so the item was put back where it started. Look again with mac_snapshot and carry on only if it still makes sense.")
        }
        hover(start.0, start.1)
        wait(0.08)
        post(event(.leftMouseDown, origin))
        wait(0.2)
        let steps = 14
        for step in 1...steps {
            try interrupted()
            let t = Double(step) / Double(steps)
            post(event(.leftMouseDragged, CGPoint(x: origin.x + (target.x - origin.x) * t, y: origin.y + (target.y - origin.y) * t)))
            wait(0.02)
        }
        wait(0.15)
        try interrupted()
        post(event(.leftMouseUp, target))
    }

    /// Scrolls by whole lines at a screen point. A positive `dy` moves the content down (towards its top).
    static func scroll(_ x: Double, _ y: Double, dx: Int32, dy: Int32) {
        hover(x, y)
        wait(0.05)
        post(CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0))
    }

    // MARK: Keyboard

    /// Sends text as Unicode key events, so the keyboard layout does not matter; line breaks press Return and tabs Tab.
    /// Stops and reports progress when the person pauses, and also when they type or click while the text goes
    /// through the system (`interruptible`), since their input would interleave; text sent to an app behind their
    /// window cannot mix with theirs. `check` runs before every chunk and every Return or Tab and throws once focus has
    /// left the approved field or app or landed on a password field, which the typing itself can cause (a Return that
    /// opens a new window, say).
    static func type(_ text: String, to destination: Destination = .system, interruptible: Bool = true, check: () throws -> Void) throws {
        let began = Date()
        let total = text.count
        var done = 0
        func stillOk() throws {
            if ControlBar.shared.isPaused {
                throw HelperFailure("paused", "Paused by the person after \(done) of \(total) characters.")
            }
            if interruptible && Presence.shared.happened(since: began, .key, .button) {
                throw HelperFailure("person_active", "Interrupted after \(done) of \(total) characters: the person typed or clicked at the same time. Look again with mac_snapshot and carry on only if it still makes sense.")
            }
            do {
                try check()
            } catch let failure as HelperFailure {
                throw HelperFailure(failure.code, "Interrupted after \(done) of \(total) characters. " + failure.message)
            }
        }
        for step in typingSteps(text) {
            try stillOk()
            switch step {
            case .text(let piece):
                let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true)
                let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false)
                piece.withUnsafeBufferPointer { buffer in
                    down?.keyboardSetUnicodeString(stringLength: piece.count, unicodeString: buffer.baseAddress)
                    up?.keyboardSetUnicodeString(stringLength: piece.count, unicodeString: buffer.baseAddress)
                }
                // Clear the flags, so a held modifier cannot turn letters into shortcuts.
                down?.flags = []
                up?.flags = []
                post(down, to: destination, keyboard: true)
                post(up, to: destination, keyboard: true)
                done += String(utf16CodeUnits: piece, count: piece.count).count
                wait(0.012)
            case .enter, .tab:
                tap(CGKeyCode(step == .tab ? kVK_Tab : kVK_Return), held: [], to: destination)
                done += 1
                wait(0.012)
            }
        }
    }

    /// Runs `body` with modifiers down, posted in order and released in reverse, also when `body` throws, so no
    /// modifier is left held.
    static func holding<T>(_ held: [Held], at destination: Destination, _ body: () throws -> T) rethrows -> T {
        var current = CGEventFlags()
        var pressed: [Held] = []
        defer {
            for modifier in pressed.reversed() {
                current.remove(flags([modifier]))
                let up = CGEvent(keyboardEventSource: source, virtualKey: keyCode(modifier), keyDown: false)
                up?.flags = current
                post(up, to: destination, keyboard: true)
            }
        }
        for modifier in held where !pressed.contains(modifier) {
            current.insert(flags([modifier]))
            let down = CGEvent(keyboardEventSource: source, virtualKey: keyCode(modifier), keyDown: true)
            down?.flags = current
            post(down, to: destination, keyboard: true)
            pressed.append(modifier)
        }
        return try body()
    }

    /// Presses one key with modifiers. Modifiers go down first and come up last.
    static func tap(_ key: CGKeyCode, held: [Held], to destination: Destination = .system) {
        let mask = flags(held)
        holding(held, at: destination) {
            let down = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: true)
            let up = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: false)
            down?.flags = mask
            up?.flags = mask
            post(down, to: destination, keyboard: true)
            wait(0.02)
            post(up, to: destination, keyboard: true)
        }
    }

    /// The key and modifiers `combo` really presses, refused when that combination can end the person's session.
    static func resolve(_ combo: KeyCombo) throws -> (code: CGKeyCode, held: [Held]) {
        guard let found = Keyboard.layout().resolve(combo) else {
            throw HelperFailure("invalid", "The keyboard layout has no \(combo.key) key; send it with mac_type instead.")
        }
        // Checked on what is really pressed: the Shift a character needs, and the key's position, count too.
        if KeyCombo.endsSession(held: found.held, key: combo.key) || KeyCombo.endsSession(held: found.held, code: found.code) {
            throw HelperFailure("blocked", "Blocked: this combination can sign the person out, lock the Mac or bring up Force Quit. Leave it to the person.")
        }
        return (CGKeyCode(found.code), found.held)
    }

    static func press(_ combo: KeyCombo, times: Int, to destination: Destination = .system, check: () throws -> Void) throws {
        let (code, held) = try resolve(combo)
        for index in 0..<max(1, times) {
            // Repeats can shift focus, so each press is checked against the approved app again.
            if index > 0 { try check() }
            tap(code, held: held, to: destination)
            if index < times - 1 { wait(0.04) }
        }
    }
}

/// Which key types what on the active layout. Text Input Sources is asked from the main thread only, since some macOS
/// releases abort when another thread asks, and the answer is cached until the input source changes.
enum Keyboard {
    private static let lock = NSLock()
    private static var cached: KeyLayout?
    private static var watching = false

    static func layout() -> KeyLayout {
        lock.lock()
        if let cached {
            lock.unlock()
            return cached
        }
        lock.unlock()
        let read = { () -> KeyLayout in
            if !watching {
                watching = true
                DistributedNotificationCenter.default().addObserver(
                    forName: NSNotification.Name(kTISNotifySelectedKeyboardInputSourceChanged as String), object: nil, queue: nil
                ) { _ in
                    lock.lock()
                    cached = nil
                    lock.unlock()
                }
            }
            return current()
        }
        let layout = Thread.isMainThread ? read() : DispatchQueue.main.sync(execute: read)
        lock.lock()
        cached = layout
        lock.unlock()
        return layout
    }

    /// What each key of the current layout types without modifiers, with Shift and with Command. Empty when the
    /// layout has no Unicode data, so every key comes from the US fallback.
    private static func current() -> KeyLayout {
        var layout = KeyLayout(plain: [:], shifted: [:], command: [:])
        guard let source = TISCopyCurrentKeyboardLayoutInputSource()?.takeRetainedValue(),
              let pointer = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) else { return layout }
        let data = Unmanaged<CFData>.fromOpaque(pointer).takeUnretainedValue() as Data
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress?.assumingMemoryBound(to: UCKeyboardLayout.self) else { return }
            let layers: [(UInt32, WritableKeyPath<KeyLayout, [UInt16: String]>)] = [
                (0, \.plain), (UInt32((shiftKey >> 8) & 0xFF), \.shifted), (UInt32((cmdKey >> 8) & 0xFF), \.command),
            ]
            for (state, layer) in layers {
                // Keypad keys type digits and + - * / too; shortcuts mean the main keyboard's keys.
                for code in 0..<128 where !(0x41...0x5C).contains(code) {
                    var dead: UInt32 = 0
                    var length = 0
                    var chars = [UniChar](repeating: 0, count: 4)
                    let status = UCKeyTranslate(
                        base, UInt16(code), UInt16(kUCKeyActionDisplay), state, UInt32(LMGetKbdType()),
                        OptionBits(kUCKeyTranslateNoDeadKeysBit), &dead, 4, &length, &chars
                    )
                    guard status == noErr, length > 0 else { continue }
                    let typed = String(utf16CodeUnits: chars, count: length)
                    guard typed.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) else { continue }
                    layout[keyPath: layer][UInt16(code)] = typed
                }
            }
        }
        return layout
    }
}
