import Foundation

/// Whether an element's Accessibility facts mark it as a terminal's input, such as a code editor's built-in terminal
/// panel: a web terminal built on xterm.js says so in its DOM classes, and editors label or identify the field as their
/// terminal ("Terminal 1, zsh"). `label` is the element's own description or title; `nearby` tells whether it belongs
/// to the element or one just around it, since a window or panel further out may carry the word for other reasons.
public func looksLikeTerminal(classes: [String], identifier: String?, label: String?, nearby: Bool) -> Bool {
    let markers: Set<String> = ["terminal", "integrated-terminal", "terminal-wrapper", "terminal-container"]
    if classes.contains(where: { $0.lowercased().hasPrefix("xterm") || markers.contains($0.lowercased()) }) { return true }
    if let identifier = identifier?.lowercased(), identifier.hasPrefix("terminal") || identifier.contains(".terminal") { return true }
    guard nearby, let label = label?.trimmingCharacters(in: .whitespaces).lowercased(), label.hasPrefix("terminal") else { return false }
    // "Terminal 1, zsh" or "Terminal", but not "Terminals" or "Terminal…ly".
    let rest = label.dropFirst("terminal".count)
    return rest.first.map { !$0.isLetter } ?? true
}
