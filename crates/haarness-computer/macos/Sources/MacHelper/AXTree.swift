import AppKit
import ApplicationServices
import MacKit

/// Reads apps through Accessibility and remembers the elements of the latest snapshot by ref. Refs only grow within
/// one helper process, so a ref from an earlier snapshot is reported as expired instead of naming another element.
final class AXTree {
    private var counter = 0
    private var known: [Int: AXUIElement] = [:]

    /// How long one Accessibility call waits for an app: an app that hangs must not hang the helper.
    static let patience: Float = 1.5

    func forget() {
        known.removeAll()
    }

    func element(_ ref: Int) throws -> AXUIElement {
        guard let element = known[ref] else {
            throw HelperFailure("stale", "@e\(ref) has expired: refs last only until the next snapshot. Call mac_snapshot for current ones.")
        }
        return element
    }

    static func application(_ pid: pid_t) -> AXUIElement {
        let app = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(app, patience)
        return app
    }

    static func systemWide() -> AXUIElement {
        let system = AXUIElementCreateSystemWide()
        AXUIElementSetMessagingTimeout(system, patience)
        return system
    }

    static func value(_ element: AXUIElement, _ name: String) -> AnyObject? {
        var out: AnyObject?
        return AXUIElementCopyAttributeValue(element, name as CFString, &out) == .success ? out : nil
    }

    static func text(_ element: AXUIElement, _ name: String) -> String? {
        switch value(element, name) {
        case let text as String: return text
        case let number as NSNumber: return number.stringValue
        default: return nil
        }
    }

    static func flag(_ element: AXUIElement, _ name: String) -> Bool? {
        (value(element, name) as? NSNumber)?.boolValue
    }

    static func element(_ object: AnyObject?) -> AXUIElement? {
        guard let object, CFGetTypeID(object) == AXUIElementGetTypeID() else { return nil }
        return (object as! AXUIElement)
    }

    static func children(_ element: AXUIElement) -> [AXUIElement] {
        guard let list = value(element, kAXChildrenAttribute) as? [AnyObject] else { return [] }
        return list.compactMap { self.element($0) }
    }

    static func parent(_ element: AXUIElement) -> AXUIElement? {
        self.element(value(element, kAXParentAttribute))
    }

    static func box(_ element: AXUIElement) -> Box? {
        guard let position = value(element, kAXPositionAttribute), let size = value(element, kAXSizeAttribute),
              CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero
        var extent = CGSize.zero
        guard AXValueGetValue(position as! AXValue, .cgPoint, &point),
              AXValueGetValue(size as! AXValue, .cgSize, &extent) else { return nil }
        return Box(x: point.x, y: point.y, width: extent.width, height: extent.height)
    }

    static func pid(_ element: AXUIElement) -> pid_t? {
        var pid: pid_t = 0
        return AXUIElementGetPid(element, &pid) == .success ? pid : nil
    }

    static func bundle(_ element: AXUIElement) -> String? {
        pid(element).flatMap { NSRunningApplication(processIdentifier: $0)?.bundleIdentifier }
    }

    static func actions(_ element: AXUIElement) -> [String] {
        var names: CFArray?
        guard AXUIElementCopyActionNames(element, &names) == .success else { return [] }
        return (names as? [String]) ?? []
    }

    static func isPassword(_ element: AXUIElement) -> Bool {
        text(element, kAXRoleAttribute) == "AXSecureTextField" || text(element, kAXSubroleAttribute) == "AXSecureTextField"
    }

    static func focused() -> AXUIElement? {
        element(value(systemWide(), kAXFocusedUIElementAttribute))
    }

    /// Hit-tests a screen point. Hits on the helper's own bar are ignored, since it belongs to no app.
    static func hit(_ x: Double, _ y: Double) -> AXUIElement? {
        var found: AXUIElement?
        guard AXUIElementCopyElementAtPosition(systemWide(), Float(x), Float(y), &found) == .success, let found else { return nil }
        return pid(found) == ProcessInfo.processInfo.processIdentifier ? nil : found
    }

    /// Roles that are not a native control: a terminal's text, web content, and what holds them. A click on one can
    /// land on anything that text or page draws, a prompt asking for approval among them.
    static let surfaceRoles: Set<String> = [
        "AXTextArea", "AXWebArea", "AXGroup", "AXUnknown", "AXScrollArea", "AXLayoutArea", "AXLayoutItem",
        "AXSplitGroup", "AXWindow", "AXApplication",
    ]

    /// True for text and web content as opposed to a native control: a surface role, or anything inside a text area
    /// or a web area (an Electron app's buttons count as web content).
    static func isContent(_ element: AXUIElement) -> Bool {
        if surfaceRoles.contains(text(element, kAXRoleAttribute) ?? "AXUnknown") { return true }
        var current = element
        for _ in 0..<40 {
            guard let up = parent(current) else { return false }
            current = up
            switch text(current, kAXRoleAttribute) ?? "" {
            case "AXTextArea", "AXWebArea": return true
            case "AXWindow", "AXApplication": return false
            default: continue
            }
        }
        return true
    }

    /// The facts the server weighs before an action: owning app, role and label.
    static func summary(_ element: AXUIElement) -> [String: Any] {
        var out: [String: Any] = [:]
        if let pid = pid(element) {
            out["pid"] = Int(pid)
            if let app = NSRunningApplication(processIdentifier: pid) {
                out["bundle"] = app.bundleIdentifier ?? ""
                out["app"] = app.localizedName ?? ""
            }
        }
        out["role"] = text(element, kAXRoleAttribute) ?? ""
        if let sub = text(element, kAXSubroleAttribute) { out["subrole"] = sub }
        if let name = text(element, kAXTitleAttribute), !name.isEmpty { out["name"] = shorten(name, 120) }
        if let detail = text(element, kAXDescriptionAttribute), !detail.isEmpty { out["description"] = shorten(detail, 120) }
        out["password"] = isPassword(element)
        out["content"] = isContent(element)
        return out
    }

    private func item(_ element: AXUIElement, level: Int, viewport: Viewport) -> OutlineItem {
        let role = Self.text(element, kAXRoleAttribute) ?? "AXUnknown"
        let password = Self.isPassword(element)
        var name = Self.text(element, kAXTitleAttribute)
        if (name ?? "").isEmpty, role == "AXStaticText" { name = Self.text(element, kAXValueAttribute) }
        var item = OutlineItem(
            role: role,
            subrole: Self.text(element, kAXSubroleAttribute),
            name: name,
            detail: Self.text(element, kAXDescriptionAttribute),
            value: password || role == "AXStaticText" ? nil : Self.text(element, kAXValueAttribute),
            placeholder: Self.text(element, kAXPlaceholderValueAttribute),
            box: Self.box(element).map { viewport.pixels($0) },
            enabled: Self.flag(element, kAXEnabledAttribute) ?? true,
            focused: Self.flag(element, kAXFocusedAttribute) ?? false,
            selected: Self.flag(element, kAXSelectedAttribute) ?? false,
            expanded: Self.flag(element, kAXExpandedAttribute),
            password: password,
            level: level
        )
        if worthShowing(item) && takesRef(item) {
            counter += 1
            item.ref = counter
            known[counter] = element
        }
        return item
    }

    /// Depth-first walk of `root` into `budget`, assigning refs to the elements worth acting on.
    func walk(_ root: AXUIElement, depthLimit: Int, viewport: Viewport, into budget: inout OutlineBudget) {
        var stack: [(element: AXUIElement, level: Int, depth: Int)] = [(root, 0, 0)]
        var seen = 0
        while let next = stack.popLast() {
            if budget.exhausted { return }
            seen += 1
            // A runaway tree (a huge table) stops here instead of taking the helper's whole time limit.
            if seen > 20_000 { return }
            let item = item(next.element, level: next.level, viewport: viewport)
            let shown = worthShowing(item)
            if shown { budget.take(outlineLine(item)) }
            if next.depth >= depthLimit { continue }
            for child in Self.children(next.element).reversed() {
                stack.append((child, shown ? next.level + 1 : next.level, next.depth + 1))
            }
        }
    }
}
