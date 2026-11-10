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

    /// The element keystrokes go to. The system-wide query often fails (kAXErrorCannotComplete for most apps on
    /// recent macOS), so the frontmost app is then asked for its own.
    static func focused() -> AXUIElement? {
        if let found = element(value(systemWide(), kAXFocusedUIElementAttribute)) { return found }
        guard let front = NSWorkspace.shared.frontmostApplication else { return nil }
        return focused(in: front.processIdentifier)
    }

    /// Hit-tests a screen point. Hits on the helper's own bar are ignored, since it belongs to no app.
    static func hit(_ x: Double, _ y: Double) -> AXUIElement? {
        var found: AXUIElement?
        guard AXUIElementCopyElementAtPosition(systemWide(), Float(x), Float(y), &found) == .success, let found else { return nil }
        return pid(found) == ProcessInfo.processInfo.processIdentifier ? nil : found
    }

    /// Hit-tests a screen point within one app's own windows, as if no other app's window lay over them.
    static func hit(_ x: Double, _ y: Double, in app: pid_t) -> AXUIElement? {
        var found: AXUIElement?
        guard AXUIElementCopyElementAtPosition(application(app), Float(x), Float(y), &found) == .success, let found,
              pid(found) == app else { return nil }
        return found
    }

    /// The element that has the keyboard focus inside one app, whether or not the app is in front.
    static func focused(in app: pid_t) -> AXUIElement? {
        element(value(application(app), kAXFocusedUIElementAttribute))
    }

    /// The window that has the keyboard focus inside one app.
    static func focusedWindow(in app: pid_t) -> AXUIElement? {
        element(value(application(app), kAXFocusedWindowAttribute))
    }

    /// The window holding `element`.
    static func window(of element: AXUIElement) -> AXUIElement? {
        if text(element, kAXRoleAttribute) == "AXWindow" { return element }
        if let window = self.element(value(element, kAXWindowAttribute)) { return window }
        var current = element
        for _ in 0..<40 {
            guard let up = parent(current) else { return nil }
            if text(up, kAXRoleAttribute) == "AXWindow" { return up }
            current = up
        }
        return nil
    }

    static func same(_ a: AXUIElement, _ b: AXUIElement) -> Bool {
        CFEqual(a, b)
    }

    /// Whether `inner` lies within `outer` in the element tree (or is it).
    static func contains(_ outer: AXUIElement, _ inner: AXUIElement) -> Bool {
        var current = inner
        for _ in 0..<12 {
            if same(current, outer) { return true }
            guard let up = parent(current) else { return false }
            current = up
        }
        return false
    }

    /// Whether the keyboard focus at `focus` serves the field `field`: the field itself, something inside it (a web
    /// field's editor), or what holds it.
    static func serves(_ focus: AXUIElement, _ field: AXUIElement) -> Bool {
        contains(field, focus) || contains(focus, field)
    }

    /// A text field of the app's own controls, which takes one line of text through Accessibility: not web content, not
    /// a document's text view, not a password field.
    static func isNativeTextField(_ element: AXUIElement) -> Bool {
        guard ["AXTextField", "AXComboBox"].contains(text(element, kAXRoleAttribute) ?? ""), !isPassword(element) else { return false }
        var current = element
        for _ in 0..<40 {
            guard let up = parent(current) else { return true }
            switch text(up, kAXRoleAttribute) ?? "" {
            case "AXWebArea", "AXTextArea": return false
            case "AXWindow", "AXApplication": return true
            default: current = up
            }
        }
        return false
    }

    /// Whether `element` is the input of a terminal an app hosts in its window, such as a code editor's built-in
    /// terminal panel (see `looksLikeTerminal`).
    static func isTerminal(_ element: AXUIElement) -> Bool {
        var current: AXUIElement? = element
        for depth in 0..<16 {
            guard let here = current else { return false }
            let role = text(here, kAXRoleAttribute) ?? ""
            if role == "AXWindow" || role == "AXApplication" { return false }
            let label = [text(here, kAXDescriptionAttribute), text(here, kAXTitleAttribute)].compactMap { $0 }.first { !$0.isEmpty }
            if looksLikeTerminal(classes: (value(here, "AXDOMClassList") as? [String]) ?? [], identifier: text(here, kAXIdentifierAttribute),
                                 label: label, nearby: depth <= 1) {
                return true
            }
            current = parent(here)
        }
        return false
    }

    /// The nearest labelled element around `element` inside its window (a group, list, sheet or pane title), one
    /// short line, for an approval to say where a control sits.
    static func context(_ element: AXUIElement) -> String? {
        var current = parent(element)
        for _ in 0..<12 {
            guard let here = current else { return nil }
            let role = text(here, kAXRoleAttribute) ?? ""
            if role == "AXWindow" || role == "AXApplication" { return nil }
            if let label = [text(here, kAXTitleAttribute), text(here, kAXDescriptionAttribute)].compactMap({ $0 }).first(where: { !$0.isEmpty }) {
                return shorten(label, 80)
            }
            current = parent(here)
        }
        return nil
    }

    /// Whether `element` is a menu item that pastes (see `isPasteMenuItem`).
    static func isPasteItem(_ element: AXUIElement) -> Bool {
        isPasteMenuItem(role: text(element, kAXRoleAttribute), character: text(element, kAXMenuItemCmdCharAttribute),
                        modifiers: (value(element, kAXMenuItemCmdModifiersAttribute) as? NSNumber)?.intValue ?? 0,
                        title: text(element, kAXTitleAttribute))
    }

    /// The pane System Settings shows: the identifier of the selected sidebar entry (an extension identifier such as
    /// com.apple.settings.PrivacySecurity.extension), which stays the same in every language. The sidebar sits near the
    /// top of the window's tree, so it is searched breadth first, before a large pane's contents.
    static func settingsPane(_ app: pid_t) -> String? {
        var queue = (value(application(app), kAXWindowsAttribute) as? [AnyObject] ?? []).compactMap { element($0) }
        var index = 0
        while index < queue.count, index < 400 {
            let next = queue[index]
            index += 1
            if text(next, kAXRoleAttribute) == "AXOutline" {
                for row in (value(next, kAXSelectedRowsAttribute) as? [AnyObject] ?? []).compactMap({ element($0) }) {
                    var inner = [row]
                    var looked = 0
                    while !inner.isEmpty, looked < 40 {
                        let item = inner.removeFirst()
                        looked += 1
                        if let pane = settingsPaneIdentifier(text(item, kAXIdentifierAttribute)) { return pane }
                        inner += children(item)
                    }
                }
            }
            queue += children(next)
        }
        return nil
    }

    /// A fingerprint of what `root` shows: roles, titles, values and selection of up to `limit` elements, read a few
    /// attributes per call. Two equal fingerprints mean nothing visible to Accessibility changed.
    static func digest(_ root: AXUIElement, limit: Int = 400) -> Int {
        var hasher = Hasher()
        var stack = [root]
        var seen = 0
        let names = [kAXRoleAttribute, kAXTitleAttribute, kAXValueAttribute, kAXSelectedAttribute, kAXChildrenAttribute] as CFArray
        while let next = stack.popLast(), seen < limit {
            seen += 1
            var values: CFArray?
            guard AXUIElementCopyMultipleAttributeValues(next, names, AXCopyMultipleAttributeOptions(rawValue: 0), &values) == .success,
                  let list = values as? [AnyObject], list.count == 5 else { continue }
            for value in list.prefix(4) {
                switch value {
                case let text as String: hasher.combine(text)
                case let number as NSNumber: hasher.combine(number.doubleValue)
                default: hasher.combine(0)
                }
            }
            if let children = list[4] as? [AnyObject] {
                hasher.combine(children.count)
                stack += children.compactMap { element($0) }.reversed()
            }
        }
        return hasher.finalize()
    }

    /// Asks a Chromium or Electron app to build its Accessibility tree, which it leaves off until an assistive app
    /// asks. The newer switch has no side effects; the older one only stands in where the newer is unknown.
    static func askForTree(_ app: pid_t) {
        let root = application(app)
        if AXUIElementSetAttributeValue(root, "AXManualAccessibility" as CFString, kCFBooleanTrue) == .attributeUnsupported {
            AXUIElementSetAttributeValue(root, "AXEnhancedUserInterface" as CFString, kCFBooleanTrue)
        }
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

    /// What of `element` the scroll areas and window around it leave visible, in screen points.
    static func visibleArea(_ element: AXUIElement) -> Sight {
        guard var visible = box(element) else { return .seen(nil) }
        guard visible.hasArea else { return .seen(visible) }
        var current = element
        for _ in 0..<40 {
            guard let up = parent(current) else { break }
            current = up
            let role = text(current, kAXRoleAttribute) ?? ""
            if clippingRoles.contains(role), let frame = box(current), frame.hasArea {
                guard let inside = visible.intersection(frame) else { return .hidden }
                visible = inside
            }
            if role == "AXWindow" || role == "AXApplication" { break }
        }
        return .seen(visible)
    }

    private func item(_ element: AXUIElement, level: Int, box: Box?, viewport: Viewport) -> OutlineItem {
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
            box: box.map { viewport.pixels($0) },
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

    /// Depth-first walk of `root` into `budget`, assigning refs to the elements worth acting on. Positions are the
    /// visible part of each element; elements scrolled wholly out of their scroll area are left out with their
    /// contents and counted in one line for that area, so no position points at something drawn over them.
    func walk(_ root: AXUIElement, depthLimit: Int, viewport: Viewport, into budget: inout OutlineBudget) {
        walk(root, depthLimit: depthLimit, viewport: viewport) { entry in
            budget.take(entry.line)
            return !budget.exhausted
        }
    }

    /// The same walk handing each line to `take`, which returns false to stop it.
    func walk(_ root: AXUIElement, depthLimit: Int, viewport: Viewport, take: (OutlineEntry) -> Bool) {
        // `element` nil marks the end of a scroll area's contents, where its count of hidden elements is reported.
        struct Step {
            var element: AXUIElement?
            var level: Int
            var depth: Int
            var clip: Box?
            var area: Int?
        }
        var hidden: [Int] = []
        var stack = [Step(element: root, level: 0, depth: 0, clip: nil, area: nil)]
        var seen = 0
        while let next = stack.popLast() {
            guard let element = next.element else {
                if let area = next.area, hidden[area] > 0,
                   !take(OutlineEntry(level: next.level, line: hiddenLine(hidden[area], level: next.level), words: "")) {
                    return
                }
                continue
            }
            seen += 1
            // A runaway tree (a huge table) stops here instead of taking the helper's whole time limit.
            if seen > 20_000 { return }
            guard case .seen(let box) = sight(of: Self.box(element), within: next.clip) else {
                if let area = next.area { hidden[area] += 1 }
                continue
            }
            let item = item(element, level: next.level, box: box, viewport: viewport)
            let shown = worthShowing(item)
            if shown, !take(OutlineEntry(level: next.level, line: outlineLine(item), words: outlineWords(item))) { return }
            if next.depth >= depthLimit { continue }
            let level = shown ? next.level + 1 : next.level
            var clip = next.clip
            var area = next.area
            if clippingRoles.contains(item.role), let box, box.hasArea {
                clip = box
                if item.role == "AXScrollArea" {
                    hidden.append(0)
                    area = hidden.count - 1
                    stack.append(Step(element: nil, level: level, depth: next.depth, clip: nil, area: area))
                }
            }
            for child in Self.children(element).reversed() {
                stack.append(Step(element: child, level: level, depth: next.depth + 1, clip: clip, area: area))
            }
        }
    }
}
