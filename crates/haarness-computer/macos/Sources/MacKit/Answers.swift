import Foundation

/// The Accessibility error codes the helper tells apart (`AXError` raw values).
public enum AXCode {
    public static let success: Int32 = 0
    public static let failure: Int32 = -25200
    public static let cannotComplete: Int32 = -25204
}

/// Whether an Accessibility press reached the app, from its error code: done, or timed out while the app handled it
/// (a menu or sheet it opened can hold the answer back), so pressing again would act twice. Nil for a reported
/// failure, which may or may not have come after the app acted. False for every other code (the element is gone,
/// takes no press, Accessibility is off), which show the press never got there.
public func pressArrived(code: Int32) -> Bool? {
    switch code {
    case AXCode.success, AXCode.cannotComplete: return true
    case AXCode.failure: return nil
    default: return false
    }
}

/// Whether a menu item pastes: its key equivalent is Command-V, with or without more modifiers, or its title starts
/// with Paste ("Paste", "Paste and Match Style").
public func isPasteMenuItem(role: String?, character: String?, modifiers: Int, title: String?) -> Bool {
    guard role == "AXMenuItem" else { return false }
    if (character ?? "").uppercased() == "V" && modifiers & MenuModifier.noCommand == 0 { return true }
    return (title ?? "").lowercased().hasPrefix("paste")
}

/// The pane identifier inside an identifier System Settings gives a sidebar entry: the extension identifier, without
/// the item name some entries add after `*` or `:` ("com.apple.Battery-Settings.extension*BatteryPreferences").
/// Nil for identifiers that are not one.
public func settingsPaneIdentifier(_ identifier: String?) -> String? {
    guard let identifier, identifier.contains(".") else { return nil }
    let pane = String(identifier.prefix { $0 != "*" && $0 != ":" })
    return pane.isEmpty ? nil : pane
}

/// What reading a field back after inserting text through Accessibility shows.
public enum InsertOutcome: Equatable {
    /// The text is there.
    case arrived
    /// The value changed but does not hold the text as sent: the field reformatted or cut it. It went in, so it must not
    /// be typed again.
    case changed
    /// Nothing changed: the insert did not take.
    case unchanged
}

public func insertOutcome(before: String, after: String, text: String) -> InsertOutcome {
    if after == before { return .unchanged }
    return after.contains(text) ? .arrived : .changed
}
