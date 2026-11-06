import Foundation

/// One accessibility element as it appears in a snapshot outline.
public struct OutlineItem {
    public var ref: Int?
    public var role: String
    public var subrole: String?
    public var name: String?
    public var detail: String?
    public var value: String?
    public var placeholder: String?
    /// Position and size in screenshot pixels.
    public var box: Box?
    public var enabled: Bool
    public var focused: Bool
    public var selected: Bool
    public var expanded: Bool?
    public var password: Bool
    public var level: Int

    public init(
        ref: Int? = nil, role: String, subrole: String? = nil, name: String? = nil, detail: String? = nil,
        value: String? = nil, placeholder: String? = nil, box: Box? = nil, enabled: Bool = true, focused: Bool = false,
        selected: Bool = false, expanded: Bool? = nil, password: Bool = false, level: Int = 0
    ) {
        self.ref = ref
        self.role = role
        self.subrole = subrole
        self.name = name
        self.detail = detail
        self.value = value
        self.placeholder = placeholder
        self.box = box
        self.enabled = enabled
        self.focused = focused
        self.selected = selected
        self.expanded = expanded
        self.password = password
        self.level = level
    }
}

/// Pure container roles. Unless they carry text they produce no line and no ref, and their children are promoted a
/// level, which keeps the outline compact.
public let containerRoles: Set<String> = [
    "AXGroup", "AXSplitGroup", "AXScrollArea", "AXLayoutArea", "AXLayoutItem", "AXUnknown", "AXSplitter",
    "AXList", "AXOutline", "AXTable", "AXRow", "AXCell", "AXColumn", "AXBrowser", "AXGrowArea", "AXMatte",
]

/// One line, flattened and cut to `limit` characters.
public func shorten(_ text: String, _ limit: Int) -> String {
    let flat = text.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
    return flat.count <= limit ? flat : String(flat.prefix(limit)) + "…"
}

func quote(_ text: String) -> String {
    let escaped = text.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
    return "\"" + escaped + "\""
}

/// `button`, `text-field`, `secure-text-field`: an Accessibility role in the lower-case style browser snapshots use.
public func roleName(_ role: String) -> String {
    let bare = role.hasPrefix("AX") ? String(role.dropFirst(2)) : role
    var out = ""
    for (index, char) in bare.enumerated() {
        if char.isUppercase && index > 0 { out.append("-") }
        out.append(contentsOf: char.lowercased())
    }
    return out.isEmpty ? "element" : out
}

/// The outline line of one element, in the shape of the browser snapshot:
/// `  @e12 [button] "Save" description="…" at=410,220 size=64x22 disabled`.
/// A password field's value never appears.
public func outlineLine(_ item: OutlineItem) -> String {
    var out = String(repeating: "  ", count: item.level)
    if let ref = item.ref { out += "@e\(ref) " }
    var role = roleName(item.role)
    if let sub = item.subrole, !sub.isEmpty, sub != "AXStandardWindow" || role != "window" {
        role += ":" + roleName(sub)
    }
    out += "[\(role)]"
    out += " " + quote(shorten(item.name ?? "", 120))
    if let detail = item.detail, !detail.isEmpty, detail != item.name {
        out += " description=" + quote(shorten(detail, 120))
    }
    if item.password {
        out += " value=[redacted]"
    } else if let value = item.value, !value.isEmpty, value != item.name {
        out += " value=" + quote(shorten(value, 200))
    }
    if let hint = item.placeholder, !hint.isEmpty { out += " placeholder=" + quote(shorten(hint, 80)) }
    if let box = item.box {
        out += " at=\(Int(box.x.rounded())),\(Int(box.y.rounded())) size=\(Int(box.width.rounded()))x\(Int(box.height.rounded()))"
    }
    if !item.enabled { out += " disabled" }
    if item.focused { out += " focused" }
    if item.selected { out += " selected" }
    if let expanded = item.expanded { out += expanded ? " expanded" : " collapsed" }
    if item.password { out += " password" }
    return out
}

/// An element gets a line when it has text, accepts input, or is more than a container.
public func worthShowing(_ item: OutlineItem) -> Bool {
    let words = [item.name, item.detail, item.value, item.placeholder].contains { !($0 ?? "").isEmpty }
    return words || item.password || !containerRoles.contains(item.role)
}

/// Whether an element shown in the outline gets a ref the agent can act on. Labels get one too: in a sidebar or a
/// list the row that takes the click has no text of its own, and a click on its label lands on it.
public func takesRef(_ item: OutlineItem) -> Bool {
    worthShowing(item)
}

/// Collects outline lines up to a character budget and says how many were left out.
public struct OutlineBudget {
    public let limit: Int
    public private(set) var spent = 0
    public private(set) var skipped = 0
    public private(set) var lines: [String] = []

    public init(limit: Int) {
        self.limit = limit
    }

    public var exhausted: Bool { spent >= limit }

    @discardableResult
    public mutating func take(_ line: String) -> Bool {
        if spent + line.count + 1 > limit {
            skipped += 1
            spent = limit
            return false
        }
        spent += line.count + 1
        lines.append(line)
        return true
    }

    public var text: String {
        if skipped == 0 { return lines.joined(separator: "\n") }
        return (lines + ["(More elements were left out. Pass window to mac_snapshot to read one window.)"]).joined(separator: "\n")
    }
}
