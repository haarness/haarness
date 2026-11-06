import AppKit
import Carbon.HIToolbox
import CoreGraphics
import MacKit

/// Pointer and keyboard events posted the way a real device posts them, each tagged as the helper's own.
enum Synth {
    static let source = CGEventSource(stateID: .hidSystemState)

    static func post(_ event: CGEvent?) {
        event?.setIntegerValueField(.eventSourceUserData, value: syntheticTag)
        event?.post(tap: .cghidEventTap)
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

    /// Clicks at a screen point. The pointer arrives first, so controls that only appear on hover exist when it presses.
    static func click(_ x: Double, _ y: Double, button: String, count: Int, held: [Held]) {
        let point = CGPoint(x: x, y: y)
        let (down, up, which): (CGEventType, CGEventType, CGMouseButton) = switch button {
        case "right": (.rightMouseDown, .rightMouseUp, .right)
        case "middle": (.otherMouseDown, .otherMouseUp, .center)
        default: (.leftMouseDown, .leftMouseUp, .left)
        }
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

    /// Sends text as Unicode key events, so the keyboard layout does not matter. Stops and reports progress when the
    /// person types, clicks or pauses, since their input would interleave. `check` runs before every chunk and every
    /// Return or Tab and throws once focus has left the approved app or landed on a password field, which the typing
    /// itself can cause (a Return that opens a new window, say).
    static func type(_ text: String, check: () throws -> Void) throws {
        let began = Date()
        let total = text.count
        var done = 0
        var pending: [UniChar] = []
        func stillOk() throws {
            if ControlBar.shared.isPaused {
                throw HelperFailure("paused", "Paused by the person after \(done) of \(total) characters.")
            }
            if Presence.shared.happened(since: began, .key, .button) {
                throw HelperFailure("person_active", "Interrupted after \(done) of \(total) characters: the person typed or clicked at the same time. Look again with mac_snapshot and carry on only if it still makes sense.")
            }
            do {
                try check()
            } catch let failure as HelperFailure {
                throw HelperFailure(failure.code, "Interrupted after \(done) of \(total) characters. " + failure.message)
            }
        }
        func flush() throws {
            for piece in pieces(pending) {
                try stillOk()
                let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true)
                let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false)
                piece.withUnsafeBufferPointer { buffer in
                    down?.keyboardSetUnicodeString(stringLength: piece.count, unicodeString: buffer.baseAddress)
                    up?.keyboardSetUnicodeString(stringLength: piece.count, unicodeString: buffer.baseAddress)
                }
                // Clear the flags, so a held modifier cannot turn letters into shortcuts.
                down?.flags = []
                up?.flags = []
                post(down)
                post(up)
                done += String(utf16CodeUnits: piece, count: piece.count).count
                wait(0.012)
            }
            pending.removeAll()
        }
        for char in text {
            if char == "\n" || char == "\r\n" || char == "\t" {
                try flush()
                try stillOk()
                tap(CGKeyCode(char == "\t" ? kVK_Tab : kVK_Return), held: [])
                done += 1
            } else {
                pending.append(contentsOf: Array(String(char).utf16))
            }
        }
        try flush()
    }

    /// Presses one key with modifiers. Modifiers go down first and come up last, also when posting stops part way,
    /// so no modifier is left held.
    static func tap(_ key: CGKeyCode, held: [Held]) {
        let mask = flags(held)
        var pressed: [Held] = []
        var current = CGEventFlags()
        defer {
            for modifier in pressed.reversed() {
                current.remove(flags([modifier]))
                let up = CGEvent(keyboardEventSource: source, virtualKey: keyCode(modifier), keyDown: false)
                up?.flags = current
                post(up)
            }
        }
        for modifier in held {
            current.insert(flags([modifier]))
            let down = CGEvent(keyboardEventSource: source, virtualKey: keyCode(modifier), keyDown: true)
            down?.flags = current
            post(down)
            pressed.append(modifier)
        }
        let down = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: true)
        let up = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: false)
        down?.flags = mask
        up?.flags = mask
        post(down)
        wait(0.02)
        post(up)
    }

    static func press(_ combo: KeyCombo, times: Int, check: () throws -> Void) throws {
        let (code, extra) = try locate(combo.key)
        let held = combo.held + extra.filter { !combo.held.contains($0) }
        // Checked on what is really pressed: an upper-case letter adds Shift, and the key's position counts too.
        if KeyCombo.endsSession(held: held, key: combo.key) || KeyCombo.endsSession(held: held, code: UInt16(code)) {
            throw HelperFailure("blocked", "Blocked: this combination can sign the person out, lock the Mac or bring up Force Quit. Leave it to the person.")
        }
        for index in 0..<max(1, times) {
            // Repeats can shift focus, so each press is checked against the approved app again.
            if index > 0 { try check() }
            tap(code, held: held)
            if index < times - 1 { wait(0.04) }
        }
    }

    /// Maps a key name, or a character on the active layout, to its key code, adding Shift where the layout needs it.
    static func locate(_ key: String) throws -> (CGKeyCode, [Held]) {
        if let named = KeyCombo.keyCodes[key] { return (CGKeyCode(named), []) }
        let keys = layout()
        if let found = keys[key] { return found }
        if key != key.lowercased(), let found = keys[key.lowercased()] { return (found.0, found.1 + [.shift]) }
        throw HelperFailure("invalid", "The active keyboard layout has no \(key) key; send it with mac_type instead.")
    }

    /// What each key of the current layout types, without and with Shift.
    static func layout() -> [String: (CGKeyCode, [Held])] {
        var out: [String: (CGKeyCode, [Held])] = [:]
        guard let source = TISCopyCurrentKeyboardLayoutInputSource()?.takeRetainedValue(),
              let pointer = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) else { return out }
        let data = Unmanaged<CFData>.fromOpaque(pointer).takeUnretainedValue() as Data
        data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress?.assumingMemoryBound(to: UCKeyboardLayout.self) else { return }
            for shifted in [false, true] {
                // Keypad keys type digits and + - * / too; shortcuts mean the main keyboard's keys.
                for code in 0..<128 where !(0x41...0x5C).contains(code) {
                    var dead: UInt32 = 0
                    var length = 0
                    var chars = [UniChar](repeating: 0, count: 4)
                    let state = shifted ? UInt32((shiftKey >> 8) & 0xFF) : 0
                    let status = UCKeyTranslate(
                        base, UInt16(code), UInt16(kUCKeyActionDown), state, UInt32(LMGetKbdType()),
                        OptionBits(kUCKeyTranslateNoDeadKeysBit), &dead, 4, &length, &chars
                    )
                    guard status == noErr, length > 0 else { continue }
                    let typed = String(utf16CodeUnits: chars, count: length)
                    if out[typed] == nil { out[typed] = (CGKeyCode(code), shifted ? [.shift] : []) }
                }
            }
        }
        return out
    }
}
