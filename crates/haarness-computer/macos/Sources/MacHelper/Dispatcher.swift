import AppKit
import Carbon.HIToolbox
import ApplicationServices
import MacKit

/// Handles requests one after another. Anything that reads or touches the screen first confirms a user is signed
/// in at the console with the screen awake and unlocked, and right before each event that the app at the target is
/// still the one that was approved.
final class Dispatcher {
    let tree = AXTree()
    /// The viewport of the latest snapshot: coordinates from the agent are pixels of that screenshot.
    var viewport: Viewport?
    /// Time for the screen to settle after an action, before the server looks again.
    let settle = 0.25

    /// Requests that use the screen: they put the bar up and are turned away during a pause.
    static let onScreen: Set<String> = ["snapshot", "inspect", "click", "type", "fill", "press", "scroll", "drag", "hover", "activate", "quit", "launch"]

    func handle(_ op: String, _ input: [String: Any]) throws -> [String: Any] {
        Marker.shared.enabled = (input["ring"] as? Bool) ?? true
        if let bar = input["bar"] as? [String: Any] { ControlBar.shared.use(BarText(bar)) }
        if Self.onScreen.contains(op) {
            try ControlBar.shared.refuseWhilePaused()
            ControlBar.shared.show(app: Self.appName(input))
        }
        switch op {
        case "hello": return ["protocol": protocolVersion, "build": helperBuild, "pid": Int(ProcessInfo.processInfo.processIdentifier)]
        case "status": return status()
        case "prompt": return prompt()
        case "apps": return ["apps": runningApps()]
        case "identify": return try identify(input)
        case "launch": return try launch(input)
        case "activate": return try activate(input)
        case "quit": return try quit(input)
        case "snapshot": return try snapshot(input)
        case "inspect": return try inspect(input)
        case "click": return try click(input)
        case "type": return try type(input)
        case "fill": return try fill(input)
        case "press": return try press(input)
        case "scroll": return try scroll(input)
        case "drag": return try drag(input)
        case "hover": return try hover(input)
        default: throw HelperFailure("invalid", "The helper has no operation called \(op).")
        }
    }

    // MARK: State

    static func consoleSession() -> [String: Any] {
        (CGSessionCopyCurrentDictionary() as? [String: Any]) ?? [:]
    }

    func status() -> [String: Any] {
        let session = Self.consoleSession()
        let onConsole = (session[kCGSessionOnConsoleKey as String] as? Bool) ?? false
        let os = ProcessInfo.processInfo.operatingSystemVersion
        return [
            "accessibility": AXIsProcessTrusted(),
            "screenRecording": CGPreflightScreenCaptureAccess(),
            "console": !session.isEmpty && onConsole,
            "locked": (session["CGSSessionScreenIsLocked"] as? Bool) ?? false,
            "asleep": CGDisplayIsAsleep(CGMainDisplayID()) != 0,
            "macos": "\(os.majorVersion).\(os.minorVersion)",
        ]
    }

    /// Triggers macOS's own Accessibility and Screen Recording prompts for the app running Haarness. Granting is up to the person.
    func prompt() -> [String: Any] {
        if !AXIsProcessTrusted() {
            _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
        }
        if !CGPreflightScreenCaptureAccess() { _ = CGRequestScreenCaptureAccess() }
        return status()
    }

    func ready(toAct: Bool) throws {
        let now = status()
        if now["console"] as? Bool != true {
            throw HelperFailure("no_console", "No user is signed in at this Mac's console, so the screen is unavailable.")
        }
        if now["locked"] as? Bool == true { throw HelperFailure("locked", "The screen is locked; the person has to unlock it first.") }
        if now["asleep"] as? Bool == true { throw HelperFailure("asleep", "The display is asleep; the person has to wake it first.") }
        if toAct && now["accessibility"] as? Bool != true {
            throw HelperFailure("permission", "This needs Accessibility, which the app running Haarness does not have yet. It is granted in System Settings → Privacy & Security → Accessibility.")
        }
    }

    /// App name for the bar, taken from the request.
    static func appName(_ input: [String: Any]) -> String? {
        let bundle = (input["expect"] as? String) ?? (input["bundle"] as? String)
        if let bundle, let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundle).first {
            return app.localizedName
        }
        return input["name"] as? String
    }

    // MARK: Apps

    func runningApps() -> [[String: Any]] {
        NSWorkspace.shared.runningApplications
            .filter { $0.activationPolicy == .regular }
            .map { app in
                [
                    "pid": Int(app.processIdentifier), "name": app.localizedName ?? "", "bundle": app.bundleIdentifier ?? "",
                    "active": app.isActive, "hidden": app.isHidden,
                ]
            }
    }

    func running(_ input: [String: Any]) throws -> NSRunningApplication {
        if let bundle = input["bundle"] as? String, let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundle).first {
            return app
        }
        if let name = input["name"] as? String,
           let app = NSWorkspace.shared.runningApplications.first(where: { $0.localizedName?.compare(name, options: .caseInsensitive) == .orderedSame }) {
            return app
        }
        throw HelperFailure("not_found", "No running app matches. mac_apps with action list shows what is open.")
    }

    /// An installed app by bundle identifier or name, without starting it.
    static func installed(_ input: [String: Any]) throws -> URL {
        if let bundle = input["bundle"] as? String, let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundle) {
            return url
        }
        if let name = input["name"] as? String {
            let file = name.hasSuffix(".app") ? name : name + ".app"
            let folders = ["/Applications", "/System/Applications", "/System/Applications/Utilities", "/Applications/Utilities", NSHomeDirectory() + "/Applications"]
            for folder in folders {
                let url = URL(fileURLWithPath: folder).appendingPathComponent(file)
                if FileManager.default.fileExists(atPath: url.path) { return url }
            }
        }
        throw HelperFailure("not_found", "Nothing installed goes by that name; try the bundle identifier instead (com.apple.TextEdit, say).")
    }

    func identify(_ input: [String: Any]) throws -> [String: Any] {
        if let app = try? running(input) {
            return ["bundle": app.bundleIdentifier ?? "", "name": app.localizedName ?? "", "running": true]
        }
        let url = try Self.installed(input)
        let bundle = Bundle(url: url)
        let name = (bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
            ?? (bundle?.object(forInfoDictionaryKey: "CFBundleName") as? String)
            ?? url.deletingPathExtension().lastPathComponent
        return ["bundle": bundle?.bundleIdentifier ?? "", "name": name, "running": false]
    }

    func launch(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: false)
        let url = try Self.installed(input)
        let signal = DispatchSemaphore(value: 0)
        var launched: NSRunningApplication?
        var problem: Error?
        let options = NSWorkspace.OpenConfiguration()
        options.activates = true
        NSWorkspace.shared.openApplication(at: url, configuration: options) { app, error in
            launched = app
            problem = error
            signal.signal()
        }
        if signal.wait(timeout: .now() + 20) == .timedOut { throw HelperFailure("failed", "The app was still starting up after 20 seconds.") }
        guard let app = launched else { throw HelperFailure("failed", problem?.localizedDescription ?? "The app did not start.") }
        Thread.sleep(forTimeInterval: 0.6)
        ControlBar.shared.show(app: app.localizedName)
        return ["pid": Int(app.processIdentifier), "bundle": app.bundleIdentifier ?? "", "name": app.localizedName ?? ""]
    }

    func activate(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: false)
        let app = try running(input)
        Self.bringForward(app)
        return ["pid": Int(app.processIdentifier), "bundle": app.bundleIdentifier ?? "", "active": app.isActive]
    }

    func quit(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: false)
        let app = try running(input)
        guard app.terminate() else { throw HelperFailure("failed", "The app refused to quit.") }
        return ["bundle": app.bundleIdentifier ?? ""]
    }

    // MARK: Looking

    /// Processes whose windows count as part of the calling app, such as the file dialogs that sandboxed apps borrow.
    static let helperProcesses = ["com.apple.appkit.xpc.openAndSavePanelService"]

    func snapshot(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: false)
        let bounds = Screenshot.display
        let view = Viewport(pointsWide: bounds.width, pointsHigh: bounds.height, longest: double(input["longest"]) ?? 1456)
        viewport = view
        tree.forget()
        let hidden = (input["hidden"] as? [String]) ?? []
        // Apps never picked when none is named: the hidden ones and the app this Haarness runs in.
        let passOver = hidden + ((input["skip"] as? [String]) ?? [])
        let whole = (input["whole"] as? Bool) ?? false
        let windows = WindowList.current(viewport: view, hidden: hidden)
        let front = NSWorkspace.shared.frontmostApplication
        let app: NSRunningApplication? = try {
            if input["bundle"] != nil || input["name"] != nil { return try running(input) }
            if let id = double(input["window"]), let entry = windows.first(where: { double($0["id"]) == id }), let pid = entry["pid"] as? Int {
                return NSRunningApplication(processIdentifier: pid_t(pid))
            }
            if let front, !matches(front.bundleIdentifier ?? "", passOver) { return front }
            // When the front app is Haarness or a hidden one, fall back to whichever app was in front before it.
            let next = windows.first { ($0["layer"] as? Int) == 0 && !matches(($0["bundle"] as? String) ?? "", passOver) }
            return (next?["pid"] as? Int).flatMap { NSRunningApplication(processIdentifier: pid_t($0)) }
        }()
        let bundle = app?.bundleIdentifier ?? ""
        let own = [bundle] + Self.helperProcesses
        // Window titles of other apps (mail subjects, document names) stay private unless the whole screen was approved.
        let listed = whole ? windows : windows.map { entry -> [String: Any] in
            if matches((entry["bundle"] as? String) ?? "", own) { return entry }
            var copy = entry
            copy.removeValue(forKey: "title")
            return copy
        }
        var out: [String: Any] = [
            "size": ["width": view.size.width, "height": view.size.height, "factor": view.factor],
            "windows": listed,
            "accessibility": AXIsProcessTrusted(),
            "whole": whole,
        ]
        if let front {
            out["frontmost"] = ["pid": Int(front.processIdentifier), "bundle": front.bundleIdentifier ?? "", "name": front.localizedName ?? ""]
        }
        if let app, !matches(bundle, hidden) {
            out["app"] = ["pid": Int(app.processIdentifier), "bundle": bundle, "name": app.localizedName ?? ""]
            ControlBar.shared.show(app: app.localizedName)
        }
        var budget = OutlineBudget(limit: Int(double(input["budget"]) ?? 40_000))
        let depth = Int(double(input["depth"]) ?? 25)
        if let app, matches(bundle, hidden) {
            budget.take("(\(app.localizedName ?? "The frontmost app") is hidden from agents; its outline is withheld.)")
        } else if let app, AXIsProcessTrusted() {
            let root = AXTree.application(app.processIdentifier)
            let all = (AXTree.value(root, kAXWindowsAttribute) as? [AnyObject] ?? []).compactMap { AXTree.element($0) }
            let chosen: [AXUIElement] = {
                guard let id = double(input["window"]), let entry = listed.first(where: { double($0["id"]) == id }),
                      let frame = entry["frame"] as? [String: Double] else { return all }
                // Accessibility has no window-server ids, so pick the AX window sitting at the same position.
                return all.filter { window in
                    guard let box = AXTree.box(window).map({ view.pixels($0) }) else { return false }
                    return abs(box.x.rounded() - (frame["x"] ?? 0)) < 2 && abs(box.y.rounded() - (frame["y"] ?? 0)) < 2
                }
            }()
            // Open menus and popovers hang off the app element rather than a window.
            let floating = AXTree.children(root).filter { AXTree.text($0, kAXRoleAttribute) == "AXMenu" }
            for window in chosen + floating {
                tree.walk(window, depthLimit: depth, viewport: view, into: &budget)
                if budget.exhausted { break }
            }
            if let bar = AXTree.element(AXTree.value(root, kAXMenuBarAttribute)) {
                // Include any menu currently pulled down from the menu bar, so the agent can choose an item. Closed menus have a
                // zero size.
                for item in AXTree.children(bar) {
                    for menu in AXTree.children(item) where AXTree.text(menu, kAXRoleAttribute) == "AXMenu" {
                        guard let box = AXTree.box(menu), box.width > 0, box.height > 0, !budget.exhausted else { continue }
                        tree.walk(menu, depthLimit: depth, viewport: view, into: &budget)
                    }
                }
                if input["menuBar"] as? Bool == true {
                    tree.walk(bar, depthLimit: 2, viewport: view, into: &budget)
                }
            }
            if chosen.isEmpty && floating.isEmpty {
                budget.take("(\(app.localizedName ?? "This app") exposes no Accessibility elements in any window, so only the screenshot can guide you.)")
            }
            if let focused = AXTree.focused() {
                let info = AXTree.summary(focused)
                let owner = (info["bundle"] as? String) ?? ""
                // When looking at one app, keep other apps' focused elements (titles, contents) private.
                if !matches(owner, hidden) && (whole || owner == bundle) { out["focused"] = info }
            }
        }
        out["outline"] = budget.text
        if input["image"] as? Bool != false {
            if !CGPreflightScreenCaptureAccess() {
                out["screenRecording"] = false
            } else if whole || app != nil {
                Thread.sleep(forTimeInterval: 0.05)
                let picture = try Screenshot.capture(size: view.size, hidden: hidden, only: whole ? nil : own)
                out["image"] = try Screenshot.jpeg(picture).base64EncodedString()
            }
        }
        return out
    }

    // MARK: Targets

    /// Resolves a ref or a pair of screenshot pixels to a screen point (in points) and, for a ref, its element.
    func locate(_ input: [String: Any]) throws -> (x: Double, y: Double, element: AXUIElement?) {
        if let ref = double(input["ref"]) {
            let element = try tree.element(Int(ref))
            switch AXTree.visibleArea(element) {
            case .hidden:
                // Its frame lies under other elements, so neither an event nor the approval's picture may use it.
                throw HelperFailure("out_of_view", "@e\(Int(ref)) is scrolled out of view. Scroll the area that holds it with mac_scroll, then use the refs of the next snapshot.")
            case .seen(nil):
                throw HelperFailure("not_found", "@e\(Int(ref)) is not visible on screen right now; call mac_snapshot again.")
            case .seen(let box?):
                let middle = box.middle
                return (middle.x, middle.y, element)
            }
        }
        if let x = double(input["x"]), let y = double(input["y"]) {
            guard let viewport else {
                throw HelperFailure("no_snapshot", "Coordinates refer to a screenshot, so call mac_snapshot first.")
            }
            guard viewport.covers(x, y) else {
                throw HelperFailure("invalid", "x \(Int(x)), y \(Int(y)) lies beyond the \(viewport.size.width)x\(viewport.size.height) screenshot.")
            }
            let point = viewport.points(x, y)
            return (point.x, point.y, nil)
        }
        throw HelperFailure("invalid", "This action needs a target: a ref from mac_snapshot, or x and y pixels.")
    }

    static func keyboardFocus() -> AXUIElement? { AXTree.focused() }

    /// Reports the element and app an action would reach; with no ref or point, the focused element, which is where
    /// keystrokes go.
    func inspect(_ input: [String: Any]) throws -> [String: Any] {
        // An action follows: a missing permission is reported now, before the person is asked to approve it.
        try ready(toAct: (input["acting"] as? Bool) ?? false)
        var out: [String: Any]
        var spot: (Double, Double)?
        if input["ref"] != nil || input["x"] != nil {
            let (x, y, element) = try locate(input)
            spot = (x, y)
            if (input["acting"] as? Bool) == true { try refusePasswordMenu(x, y) }
            out = (element ?? AXTree.hit(x, y)).map { AXTree.summary($0) } ?? [:]
            // A bare point names no native control: it counts as content.
            if element == nil { out["content"] = true }
            if out["bundle"] == nil, let app = WindowList.owner(x, y) {
                out["bundle"] = app.bundleIdentifier ?? ""
                out["app"] = app.localizedName ?? ""
                out["pid"] = Int(app.processIdentifier)
            }
        } else {
            out = Self.keyboardFocus().map { AXTree.summary($0) } ?? [:]
            if out["bundle"] == nil, let front = NSWorkspace.shared.frontmostApplication {
                out["bundle"] = front.bundleIdentifier ?? ""
                out["app"] = front.localizedName ?? ""
                out["pid"] = Int(front.processIdentifier)
            }
        }
        if let spot, let viewport {
            let pixel = (x: (spot.0 * viewport.factor).rounded(), y: (spot.1 * viewport.factor).rounded())
            out["point"] = ["x": pixel.x, "y": pixel.y]
            if input["preview"] as? Bool == true, CGPreflightScreenCaptureAccess(),
               let picture = try? Screenshot.capture(size: viewport.size, hidden: (input["hidden"] as? [String]) ?? [], only: nil) {
                out["preview"] = try? Screenshot.preview(picture, around: pixel).base64EncodedString()
            }
        }
        return out
    }

    /// Bundle of the app whose window is topmost at a point, i.e. where a pointer event would land.
    static func topApp(_ x: Double, _ y: Double) -> String? {
        if let hit = AXTree.hit(x, y), let bundle = AXTree.bundle(hit) { return bundle }
        return WindowList.owner(x, y)?.bundleIdentifier
    }

    /// Bundle of the app that would receive keystrokes: the focused element's owner, else the frontmost app.
    static func keyboardApp() -> String? {
        if let focused = keyboardFocus(), let bundle = AXTree.bundle(focused) { return bundle }
        return NSWorkspace.shared.frontmostApplication?.bundleIdentifier
    }

    /// Aborts when the app an event would reach differs from the approved one (`input[key]`). Called immediately
    /// before each event and after any window raising, so a window that slid over the target stops the action.
    func confirm(_ input: [String: Any], _ key: String = "expect", _ actual: String?) throws {
        guard let expected = input[key] as? String else { return }
        if (actual ?? "") != expected {
            throw HelperFailure("target_changed", "Something moved: the target is now in \(actual ?? "another app") instead of \(expected). Call mac_snapshot and retry.")
        }
    }

    func confirm(_ input: [String: Any], element: AXUIElement) throws {
        try confirm(input, "expect", AXTree.bundle(element))
    }

    func confirm(_ input: [String: Any], point: (Double, Double), key: String = "expect") throws {
        try confirm(input, key, Self.topApp(point.0, point.1))
    }

    /// Raises the element's app and window, since synthetic events go to whatever is topmost.
    static func raise(_ element: AXUIElement) {
        if let pid = AXTree.pid(element), let app = NSRunningApplication(processIdentifier: pid) { bringForward(app) }
        if let window = AXTree.element(AXTree.value(element, kAXWindowAttribute)) {
            AXUIElementPerformAction(window, kAXRaiseAction as CFString)
            Thread.sleep(forTimeInterval: 0.05)
        }
    }

    /// Makes `app` frontmost. A background helper may not take the front from another app with `activate` alone,
    /// so it asks through Accessibility first and then waits until it happened.
    static func bringForward(_ app: NSRunningApplication) {
        if app.isHidden { app.unhide() }
        if app.isActive { return }
        let element = AXTree.application(app.processIdentifier)
        if AXUIElementSetAttributeValue(element, kAXFrontmostAttribute as CFString, kCFBooleanTrue) != .success {
            app.activate(options: [.activateAllWindows])
        }
        for _ in 0..<20 where !app.isActive { Thread.sleep(forTimeInterval: 0.05) }
        Thread.sleep(forTimeInterval: 0.1)
    }

    // MARK: Acting

    func held(_ input: [String: Any]) throws -> [Held] {
        try ((input["held"] as? [String]) ?? []).map { name in
            guard let modifier = try? KeyCombo.read(name + "+a").held.first else {
                throw HelperFailure("invalid", "\(name) is not a modifier.")
            }
            return modifier
        }
    }

    func click(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        // Hold the click until the person's typing pauses, so it cannot redirect their keystrokes.
        try Presence.shared.waitForQuiet()
        let (x, y, element) = try locate(input)
        if let element { try confirm(input, element: element) } else { try confirm(input, point: (x, y)) }
        let button = (input["button"] as? String) ?? "left"
        let count = max(1, min(3, Int(double(input["count"]) ?? 1)))
        let modifiers = try held(input)
        Marker.shared.flash(x, y)
        // A plain click on an element that can be pressed goes through Accessibility: the pointer stays where it is
        // and the keyboard focus does not jump.
        if let element, button == "left", count == 1, modifiers.isEmpty, AXTree.actions(element).contains(kAXPressAction),
           AXUIElementPerformAction(element, kAXPressAction as CFString) == .success {
            Thread.sleep(forTimeInterval: settle)
            return ["via": "accessibility"]
        }
        if let element { Self.raise(element) }
        try ControlBar.shared.outOfTheWay(x, y) {
            // Whatever is topmost receives the pointer event, so confirm it is the approved app.
            try confirm(input, point: (x, y))
            try refusePasswordMenu(x, y)
            try ready(toAct: true)
            try Presence.shared.waitForQuiet()
            Synth.click(x, y, button: button, count: count, held: modifiers)
        }
        Thread.sleep(forTimeInterval: settle)
        return ["via": "pointer"]
    }

    /// While a password field has the keyboard focus, macOS may open its password suggestions under it: a pop-up in
    /// the app's own process offering the Passwords app or a password manager. Pointer input landing on any pop-up
    /// then is refused, so an agent never reaches saved passwords that way.
    func refusePasswordMenu(_ x: Double, _ y: Double) throws {
        guard IsSecureEventInputEnabled(), let top = WindowList.top(x, y), top.layer != 0 else { return }
        throw HelperFailure("password", "A password suggestion menu is open there, under a focused password field, and passwords stay with the person. Press Escape to close it, or click another field first.")
    }

    /// Refuses keystrokes bound for a password: `element`'s, or the focused element's when `element` is nil. Typing
    /// where focus cannot be read is refused too, since that field could be one; so is typing at the focus while
    /// macOS holds secure keyboard input, which apps turn on while a password field has focus. A named `element` is
    /// judged by itself (`secureInput` false): focusing it moves the keyboard away from any password field, and the
    /// checks before each keystroke then see that.
    func refusePassword(_ element: AXUIElement?, secureInput: Bool = true) throws {
        if secureInput && IsSecureEventInputEnabled() {
            throw HelperFailure("password", "Password fields are refused; the person enters passwords themselves. macOS reports a password field in use right now.")
        }
        guard let target = element ?? Self.keyboardFocus() else {
            throw HelperFailure("focus_unknown", "The field with the keyboard focus can't be read here, and it might take a password, so nothing was typed. Pass the field's ref instead.")
        }
        if AXTree.isPassword(target) {
            throw HelperFailure("password", "Password fields are refused; the person enters passwords themselves.")
        }
    }

    func type(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let text = input["text"] as? String, !text.isEmpty else { throw HelperFailure("invalid", "There is nothing to type.") }
        // Wait for the person's keyboard to go quiet; interleaved typing would garble both.
        try Presence.shared.waitForQuiet()
        if input["ref"] != nil {
            let (x, y, found) = try locate(input)
            guard let found else { throw HelperFailure("invalid", "Typing needs a ref. Click the field first to type at a point.") }
            try confirm(input, element: found)
            try refusePassword(found, secureInput: false)
            Self.raise(found)
            if AXUIElementSetAttributeValue(found, kAXFocusedAttribute as CFString, kCFBooleanTrue) != .success {
                try ControlBar.shared.outOfTheWay(x, y) {
                    try confirm(input, point: (x, y))
                    Marker.shared.flash(x, y)
                    Synth.click(x, y, button: "left", count: 1, held: [])
                }
            }
            Thread.sleep(forTimeInterval: 0.1)
        }
        // Keystrokes land in the focused element; it has to stay inside the approved app and may never be a password field,
        // both now and for every chunk.
        let check = {
            try self.confirm(input, "expect", Self.keyboardApp())
            try self.refusePassword(nil)
        }
        try check()
        try Synth.type(text, check: check)
        Thread.sleep(forTimeInterval: settle)
        return [:]
    }

    func fill(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let value = input["value"] as? String else { throw HelperFailure("invalid", "There is no value to set.") }
        try Presence.shared.waitForQuiet()
        let (x, y, element) = try locate(input)
        guard let element else { throw HelperFailure("invalid", "Replacing a value needs a ref.") }
        try confirm(input, element: element)
        // The value is set on the element itself, whatever has the keyboard.
        try refusePassword(element, secureInput: false)
        Marker.shared.flash(x, y)
        guard AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFString) == .success else {
            throw HelperFailure("failed", "Setting this element's value directly failed; click into it and use mac_type.")
        }
        Thread.sleep(forTimeInterval: settle)
        return [:]
    }

    func press(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let keys = input["keys"] as? String else { throw HelperFailure("invalid", "Name the key to press.") }
        let combo = try KeyCombo.read(keys)
        try Presence.shared.waitForQuiet()
        try confirm(input, "expect", Self.keyboardApp())
        // A printable key sent to a password field would be part of a password.
        if combo.types { try refusePassword(nil) }
        try Synth.press(combo, times: max(1, min(50, Int(double(input["times"]) ?? 1)))) {
            try self.confirm(input, "expect", Self.keyboardApp())
            if combo.types { try self.refusePassword(nil) }
        }
        Thread.sleep(forTimeInterval: settle)
        return [:]
    }

    func scroll(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        let (x, y, _) = try locate(input)
        // Scroll events go wherever the pointer is.
        try confirm(input, point: (x, y))
        try refusePasswordMenu(x, y)
        let lines = Int32(max(1, min(50, double(input["amount"]) ?? 3)))
        let (dx, dy): (Int32, Int32) = switch (input["direction"] as? String) ?? "down" {
        case "up": (0, lines)
        case "left": (lines, 0)
        case "right": (-lines, 0)
        default: (0, -lines)
        }
        ControlBar.shared.outOfTheWay(x, y) { Synth.scroll(x, y, dx: dx, dy: dy) }
        Thread.sleep(forTimeInterval: settle)
        return [:]
    }

    func drag(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let pickup = input["from"] as? [String: Any], let drop = input["to"] as? [String: Any] else {
            throw HelperFailure("invalid", "mac_drag needs both a from and a to.")
        }
        try Presence.shared.waitForQuiet()
        let start = try locate(pickup)
        let end = try locate(drop)
        if let element = start.element { Self.raise(element) }
        try ControlBar.shared.outOfTheWay(start.x, start.y) {
            try ControlBar.shared.outOfTheWay(end.x, end.y) {
                // Check both ends: the pickup and the drop must each be in an approved app.
                try confirm(input, point: (start.x, start.y))
                try confirm(input, point: (end.x, end.y), key: "expectEnd")
                try refusePasswordMenu(start.x, start.y)
                try refusePasswordMenu(end.x, end.y)
                Marker.shared.flash(start.x, start.y, seconds: 0.25)
                try Synth.drag(from: (start.x, start.y), to: (end.x, end.y))
            }
        }
        Thread.sleep(forTimeInterval: settle)
        return [:]
    }

    func hover(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        let (x, y, _) = try locate(input)
        try ControlBar.shared.outOfTheWay(x, y) {
            try confirm(input, point: (x, y))
            try refusePasswordMenu(x, y)
            Synth.hover(x, y)
        }
        Thread.sleep(forTimeInterval: settle)
        return [:]
    }
}
