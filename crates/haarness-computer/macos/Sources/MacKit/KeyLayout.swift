import Foundation

/// What each key of a keyboard layout types, by virtual key code (Carbon `kVK_*`), in the three layers a key press can
/// use: no modifier, Shift, and Command. macOS matches a Command shortcut against what the Command layer types, which
/// on Cyrillic, Greek or Arabic layouts is the Latin letter printed beside the native one.
public struct KeyLayout: Equatable {
    public var plain: [UInt16: String]
    public var shifted: [UInt16: String]
    public var command: [UInt16: String]

    public init(plain: [UInt16: String], shifted: [UInt16: String], command: [UInt16: String]) {
        self.plain = plain
        self.shifted = shifted
        self.command = command
    }

    /// The main block of a US keyboard, used for characters the current layout cannot type.
    public static let us: KeyLayout = {
        let plain: [UInt16: String] = [
            0x00: "a", 0x01: "s", 0x02: "d", 0x03: "f", 0x04: "h", 0x05: "g", 0x06: "z", 0x07: "x", 0x08: "c",
            0x09: "v", 0x0B: "b", 0x0C: "q", 0x0D: "w", 0x0E: "e", 0x0F: "r", 0x10: "y", 0x11: "t", 0x12: "1",
            0x13: "2", 0x14: "3", 0x15: "4", 0x16: "6", 0x17: "5", 0x18: "=", 0x19: "9", 0x1A: "7", 0x1B: "-",
            0x1C: "8", 0x1D: "0", 0x1E: "]", 0x1F: "o", 0x20: "u", 0x21: "[", 0x22: "i", 0x23: "p", 0x25: "l",
            0x26: "j", 0x27: "'", 0x28: "k", 0x29: ";", 0x2A: "\\", 0x2B: ",", 0x2C: "/", 0x2D: "n", 0x2E: "m",
            0x2F: ".", 0x32: "`",
        ]
        let symbols: [UInt16: String] = [
            0x12: "!", 0x13: "@", 0x14: "#", 0x15: "$", 0x16: "^", 0x17: "%", 0x18: "+", 0x19: "(", 0x1A: "&",
            0x1B: "_", 0x1C: "*", 0x1D: ")", 0x1E: "}", 0x21: "{", 0x27: "\"", 0x29: ":", 0x2A: "|", 0x2B: "<",
            0x2C: "?", 0x2F: ">", 0x32: "~",
        ]
        var shifted = plain.mapValues { $0.uppercased() }
        shifted.merge(symbols) { _, symbol in symbol }
        return KeyLayout(plain: plain, shifted: shifted, command: plain)
    }()

    /// The key to press for `combo` and every modifier that goes down with it.
    ///
    /// A shortcut (Command or Control held) looks for its character in the Command layer, ignoring case, and prefers
    /// the key whose cap shows that character; it adds no Shift of its own, so `Meta+S` is Command-S and Command-Shift-S
    /// has to name Shift. A shifted symbol (`Meta+?`) adds the Shift it needs. A plain key types its character exactly,
    /// adding Shift where the layout needs it. Characters the layout lacks are taken from a US keyboard. Nil when
    /// neither layout has the character.
    public func resolve(_ combo: KeyCombo) -> (code: UInt16, held: [Held])? {
        if combo.key.count > 1, let named = KeyCombo.keyCodes[combo.key.lowercased()] {
            return (named, combo.held)
        }
        let shortcut = combo.held.contains(.command) || combo.held.contains(.control)
        for layout in [self, KeyLayout.us] {
            if let found = layout.find(combo.key, shortcut: shortcut) {
                return (found.code, combo.held + (found.shift && !combo.held.contains(.shift) ? [.shift] : []))
            }
        }
        return nil
    }

    private func find(_ character: String, shortcut: Bool) -> (code: UInt16, shift: Bool)? {
        func codes(_ layer: [UInt16: String], _ test: (String) -> Bool) -> [UInt16] {
            layer.filter { test($0.value) }.map(\.key).sorted()
        }
        if shortcut {
            let lower = character.lowercased()
            let candidates = codes(command) { $0.lowercased() == lower }
            if let keycap = candidates.first(where: { plain[$0]?.lowercased() == lower }) ?? candidates.first {
                return (keycap, false)
            }
            if let code = codes(plain, { $0.lowercased() == lower }).first { return (code, false) }
            if let code = codes(shifted, { $0 == character }).first { return (code, true) }
            return nil
        }
        if let code = codes(plain, { $0 == character }).first { return (code, false) }
        if let code = codes(shifted, { $0 == character }).first { return (code, true) }
        return nil
    }
}
