import Foundation

/// The bits of Accessibility's `AXMenuItemCmdModifiers`: which modifiers a menu item's key equivalent adds to Command,
/// and one saying Command is not part of it.
public enum MenuModifier {
    public static let shift = 1
    public static let option = 2
    public static let control = 4
    public static let noCommand = 8
}

extension KeyCombo {
    /// Whether this combination can stand for a menu item: Command plus one printable character. Named keys (Return,
    /// arrows, F5) and combinations without Command stay key presses.
    public var mayBeMenuShortcut: Bool {
        held.contains(.command) && !held.contains(.fn) && key.count == 1 && key != " "
    }

    /// Whether a menu item whose key equivalent reads `character` (`AXMenuItemCmdChar`) with `modifiers`
    /// (`AXMenuItemCmdModifiers`) is what this combination presses. Letters match in either case; Shift, Option and
    /// Control must agree exactly, so Command-S never reaches Save As (Command-Shift-S).
    public func matchesMenuItem(character: String, modifiers: Int) -> Bool {
        guard mayBeMenuShortcut, !character.isEmpty, modifiers & MenuModifier.noCommand == 0 else { return false }
        guard character.uppercased() == key.uppercased() else { return false }
        return (modifiers & MenuModifier.shift != 0) == held.contains(.shift)
            && (modifiers & MenuModifier.option != 0) == held.contains(.option)
            && (modifiers & MenuModifier.control != 0) == held.contains(.control)
    }
}
