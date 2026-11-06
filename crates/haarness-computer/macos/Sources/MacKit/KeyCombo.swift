import Foundation

/// A modifier key held while another key is pressed.
public enum Held: String, CaseIterable, Hashable {
    case command, shift, option, control, fn
}

/// A key and the modifiers held with it, written the way the browser tools write keys: `Meta+S`, `Shift+Tab`,
/// `Enter`, `ArrowDown`. Common Mac spellings work too (`cmd+s`, `opt+left`, `ctrl+tab`).
public struct KeyCombo: Equatable {
    public let held: [Held]
    /// A named key (`enter`, `arrowdown`, `f5`, lower case) or one printable character.
    public let key: String

    public init(held: [Held], key: String) {
        self.held = held
        self.key = key
    }

    static let modifierNames: [String: Held] = [
        "meta": .command, "command": .command, "cmd": .command, "super": .command, "⌘": .command,
        "shift": .shift, "⇧": .shift,
        "alt": .option, "option": .option, "opt": .option, "⌥": .option,
        "control": .control, "ctrl": .control, "⌃": .control,
        "fn": .fn, "globe": .fn,
    ]

    /// Named keys and their virtual key codes (Carbon `kVK_*` values), including the browser-style names.
    public static let keyCodes: [String: UInt16] = [
        "enter": 0x24, "return": 0x24, "tab": 0x30, "space": 0x31, " ": 0x31,
        "backspace": 0x33, "delete": 0x75, "forwarddelete": 0x75,
        "escape": 0x35, "esc": 0x35,
        "home": 0x73, "end": 0x77, "pageup": 0x74, "pagedown": 0x79,
        "arrowleft": 0x7B, "left": 0x7B, "arrowright": 0x7C, "right": 0x7C,
        "arrowdown": 0x7D, "down": 0x7D, "arrowup": 0x7E, "up": 0x7E,
        "f1": 0x7A, "f2": 0x78, "f3": 0x63, "f4": 0x76, "f5": 0x60, "f6": 0x61, "f7": 0x62, "f8": 0x64,
        "f9": 0x65, "f10": 0x6D, "f11": 0x67, "f12": 0x6F,
    ]

    /// Reads `Meta+Shift+S`. The last part is the key, the others are modifiers in any order. `Meta++` and
    /// `Meta+Plus` both press the plus key.
    public static func read(_ text: String) throws -> KeyCombo {
        let raw = text.trimmingCharacters(in: .whitespaces)
        if raw.isEmpty { throw HelperFailure("invalid", "Name a key, for example Enter or Meta+S.") }
        if raw == "+" { return KeyCombo(held: [], key: "+") }
        var parts = raw.components(separatedBy: "+")
        if raw.hasSuffix("++") { parts = Array(parts.dropLast(2)) + ["+"] }
        guard let last = parts.last, !last.isEmpty else {
            throw HelperFailure("invalid", "\(text) names modifiers but no key.")
        }
        var held: [Held] = []
        for part in parts.dropLast() {
            guard let modifier = modifierNames[part.lowercased()] else {
                throw HelperFailure("invalid", "\(part) in \(text) is not a modifier. Use Meta, Control, Alt or Shift.")
            }
            if !held.contains(modifier) { held.append(modifier) }
        }
        let lower = last.lowercased()
        if lower == "plus" { return KeyCombo(held: held, key: "+") }
        if keyCodes[lower] != nil { return KeyCombo(held: held, key: lower == " " ? "space" : lower) }
        // A control character is only accepted by its name, so a refused combination cannot hide behind one.
        if last.count == 1, last.unicodeScalars.allSatisfy({ printable($0) }) {
            return KeyCombo(held: held, key: last)
        }
        throw HelperFailure("invalid", "\(last) is not a key name. Use Enter, Escape, Tab, Backspace, ArrowDown, F5 or one character.")
    }

    /// Combinations that log the person out, lock the screen or open Force Quit: they take the Mac away from the
    /// person. `held` is what is really pressed, including the shift an upper-case letter needs. More modifiers do
    /// not make a combination harmless, since macOS still matches it.
    public static func endsSession(held: [Held], key: String) -> Bool {
        let down = Set(held)
        let name = key.lowercased()
        if name == "q" && down.isSuperset(of: [.command, .shift]) { return true }
        if name == "q" && down.isSuperset(of: [.command, .control]) { return true }
        if (name == "escape" || name == "esc") && down.isSuperset(of: [.command, .option]) { return true }
        return false
    }

    /// The same check by key position: on some layouts macOS matches Command shortcuts by the key, not the letter it
    /// types, so Command-Shift and the key where Q sits on a US keyboard logs out whatever that key types.
    public static func endsSession(held: [Held], code: UInt16) -> Bool {
        let down = Set(held)
        if code == 0x0C && (down.isSuperset(of: [.command, .shift]) || down.isSuperset(of: [.command, .control])) { return true }
        return code == 0x35 && down.isSuperset(of: [.command, .option])
    }

    /// Whether pressing this types a character into the focused field (rather than running a shortcut).
    public var types: Bool {
        !held.contains(.command) && !held.contains(.control) && (key.count == 1 || key == "space")
    }
}

func printable(_ scalar: Unicode.Scalar) -> Bool {
    switch scalar.properties.generalCategory {
    case .control, .format, .surrogate, .privateUse, .unassigned: return false
    default: return true
    }
}

/// Splits UTF-16 into key-event chunks of at most `size` units, never separating a surrogate pair (such as an emoji).
public func pieces(_ units: [UInt16], size: Int = 16) -> [[UInt16]] {
    var out: [[UInt16]] = []
    var start = 0
    let step = max(2, size)
    while start < units.count {
        var end = min(start + step, units.count)
        if end < units.count && UTF16.isLeadSurrogate(units[end - 1]) { end -= 1 }
        out.append(Array(units[start..<end]))
        start = end
    }
    return out
}
