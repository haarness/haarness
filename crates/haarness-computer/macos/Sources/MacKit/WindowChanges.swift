import Foundation

/// A window as the window server lists it, with what Accessibility adds for the acted-on app's own windows.
public struct WindowFacts: Equatable {
    public var id: Int
    public var pid: Int32
    public var app: String
    public var layer: Int
    public var title: String?
    /// The Accessibility role and subrole of the window, when they were read.
    public var role: String?
    public var subrole: String?

    public init(id: Int, pid: Int32, app: String, layer: Int, title: String? = nil, role: String? = nil, subrole: String? = nil) {
        self.id = id
        self.pid = pid
        self.app = app
        self.layer = layer
        self.title = title
        self.role = role
        self.subrole = subrole
    }

    /// `window`, `sheet`, `dialog`, `menu` or `popover`.
    public var kind: String {
        switch role {
        case "AXSheet": return "sheet"
        case "AXPopover": return "popover"
        case "AXMenu": return "menu"
        default: break
        }
        if subrole == "AXDialog" || subrole == "AXSystemDialog" { return "dialog" }
        switch layer {
        case 101: return "menu"
        case 8: return "dialog"
        default: return "window"
        }
    }
}

/// The windows an action opened and closed, as reply objects: `{"app", "role", "title"}`. Titles are given only for
/// the acted-on app (`target`), since another app's window titles (mail subjects, document names) are the person's
/// business. At most `limit` of each.
public func windowChanges(before: [WindowFacts], after: [WindowFacts], target: Int32, limit: Int = 8) -> (opened: [[String: String]], closed: [[String: String]]) {
    func describe(_ window: WindowFacts) -> [String: String] {
        var out = ["app": window.app, "role": window.kind]
        if window.pid == target, let title = window.title, !title.isEmpty { out["title"] = shorten(title, 120) }
        return out
    }
    let earlier = Set(before.map(\.id))
    let later = Set(after.map(\.id))
    let opened = after.filter { !earlier.contains($0.id) }.prefix(limit).map(describe)
    let closed = before.filter { !later.contains($0.id) }.prefix(limit).map(describe)
    return (Array(opened), Array(closed))
}
