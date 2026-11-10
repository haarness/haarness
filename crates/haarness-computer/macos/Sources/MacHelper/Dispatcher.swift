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
    /// The app the latest look at a single app showed; a look at the whole screen forgets it. Keys and text without a
    /// ref go to its own focused element, and pixels inside its windows belong to it even where another app's window
    /// covers them. Without one, the app in front is used.
    var lookedAt: NSRunningApplication?
    /// Where the agent's latest screen work happened, read when the person presses Escape.
    let escapeLock = NSLock()
    var workingPid: pid_t?
    var workingInFront = true
    /// Set once macOS's Accessibility prompt was shown, so the next request for permissions asks for Screen Recording.
    var askedForAccessibility = false

    /// Requests that use the screen: they put the bar up and are turned away during a pause.
    static let onScreen: Set<String> = ["snapshot", "inspect", "click", "type", "fill", "press", "scroll", "drag", "hover", "activate", "quit", "launch"]

    /// Whether the person's Escape stops the agent now: only while it works in the app in front. While it works in an
    /// app behind their window, they keep typing in their own apps, Escape included, and the strip's Stop is the
    /// control.
    var escapeStops: Bool {
        escapeLock.lock()
        let (pid, front) = (workingPid, workingInFront)
        escapeLock.unlock()
        if front { return true }
        guard let pid else { return true }
        return WindowServer.frontPID() == pid
    }

    func handle(_ op: String, _ input: [String: Any]) throws -> [String: Any] {
        Marker.shared.enabled = (input["ring"] as? Bool) ?? true
        if let bar = input["bar"] as? [String: Any] { ControlBar.shared.use(BarText(bar)) }
        if Self.onScreen.contains(op) {
            try ControlBar.shared.refuseWhilePaused()
            ControlBar.shared.show(app: Self.appName(input))
            KeepAwake.shared.hold()
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

    /// Triggers macOS's own prompt for the next permission the app running Haarness lacks: Accessibility first, then,
    /// once that prompt was shown, Screen Recording, which also lists the app on that page. macOS can drop a second
    /// prompt raised while the first is up, so they come one at a time. Granting is up to the person.
    func prompt() -> [String: Any] {
        if !AXIsProcessTrusted() && !askedForAccessibility {
            askedForAccessibility = true
            _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
        } else if !CGPreflightScreenCaptureAccess() {
            _ = CGRequestScreenCaptureAccess()
        }
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
        if let bundle, let app = pick(NSRunningApplication.runningApplications(withBundleIdentifier: bundle)) {
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

    /// When an app runs more than once: the newest copy showing a window, else the newest.
    static func pick(_ copies: [NSRunningApplication]) -> NSRunningApplication? {
        guard copies.count > 1 else { return copies.first }
        let showing = Set(WindowList.entries().filter { $0.layer == 0 }.map(\.pid))
        let newest = copies.sorted { ($0.launchDate ?? .distantPast) > ($1.launchDate ?? .distantPast) }
        return newest.first { showing.contains($0.processIdentifier) } ?? newest.first
    }

    func running(_ input: [String: Any]) throws -> NSRunningApplication {
        if let bundle = input["bundle"] as? String, let app = Self.pick(NSRunningApplication.runningApplications(withBundleIdentifier: bundle)) {
            return app
        }
        if let name = input["name"] as? String,
           let app = Self.pick(NSWorkspace.shared.runningApplications.filter({ $0.localizedName?.compare(name, options: .caseInsensitive) == .orderedSame })) {
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

    /// Whether `app` shows an ordinary window, waiting up to `seconds` for one.
    func waitForWindow(_ app: NSRunningApplication, seconds: Double) -> Bool {
        let end = Date().addingTimeInterval(seconds)
        repeat {
            if WindowList.entries().contains(where: { $0.pid == app.processIdentifier && $0.layer == 0 }) { return true }
            Thread.sleep(forTimeInterval: 0.1)
        } while Date() < end && !app.isTerminated
        return false
    }

    /// Starts an app behind the person's window and waits for its window. An app that only opens one once it is active
    /// is brought forward; one that brings itself forward hands the front back.
    func launch(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: false)
        let url = try Self.installed(input)
        let prior = frontApp()
        let started = Date()
        let before = windowFacts(nil)
        let signal = DispatchSemaphore(value: 0)
        var launched: NSRunningApplication?
        var problem: Error?
        let options = NSWorkspace.OpenConfiguration()
        options.activates = Behind.forced
        NSWorkspace.shared.openApplication(at: url, configuration: options) { app, error in
            launched = app
            problem = error
            signal.signal()
        }
        if signal.wait(timeout: .now() + 20) == .timedOut { throw HelperFailure("failed", "The app was still starting up after 20 seconds.") }
        guard let app = launched else { throw HelperFailure("failed", problem?.localizedDescription ?? "The app did not start.") }
        var fallback: String? = Behind.forced ? "forced" : nil
        if app.activationPolicy != .regular {
            // Menu bar apps open no window to wait for.
            Thread.sleep(forTimeInterval: 0.6)
        } else if !waitForWindow(app, seconds: 4) && fallback == nil {
            Self.bringForward(app)
            fallback = "no-key-window"
            _ = waitForWindow(app, seconds: 4)
        }
        let back = fallback == nil && handBack(from: app, to: prior, since: started)
        ControlBar.shared.show(app: app.localizedName)
        var out = reply(app, via: "process", background: fallback == nil, before: before, fallback: fallback, handedBack: back)
        out.removeValue(forKey: "via")
        out["pid"] = Int(app.processIdentifier)
        out["bundle"] = app.bundleIdentifier ?? ""
        out["name"] = app.localizedName ?? ""
        return out
    }

    func activate(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: false)
        let app = try running(input)
        Self.bringForward(app)
        working(app, inFront: true)
        return ["pid": Int(app.processIdentifier), "bundle": app.bundleIdentifier ?? "", "active": app.isActive]
    }

    func quit(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: false)
        let app = try running(input)
        guard app.terminate() else { throw HelperFailure("failed", "The app refused to quit.") }
        return ["bundle": app.bundleIdentifier ?? ""]
    }

    // MARK: Looking

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
        let own = [bundle] + Behind.helperProcesses
        lookedAt = whole || matches(bundle, hidden) ? nil : app
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
        var behind = false
        if let app, !matches(bundle, hidden) {
            out["app"] = ["pid": Int(app.processIdentifier), "bundle": bundle, "name": app.localizedName ?? ""]
            ControlBar.shared.show(app: app.localizedName)
            if !whole {
                behind = WindowServer.frontPID() != app.processIdentifier || WindowList.covered(app.processIdentifier, besides: Behind.helperPids())
                out["behind"] = behind
                out["offscreen"] = offscreen(app) ?? NSNull()
            }
            if let pane = Self.settingsPane(for: app) { out["pane"] = pane }
        }
        working(app, inFront: !behind)
        var budget = OutlineBudget(limit: Int(double(input["budget"]) ?? 40_000))
        let depth = Int(double(input["depth"]) ?? 25)
        let query = ((input["find"] as? String) ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        // A search reads the whole outline first and then keeps the matches, so the budget applies to what is kept.
        var found: [OutlineEntry] = []
        let take: (OutlineEntry) -> Bool = { entry in
            if query.isEmpty {
                budget.take(entry.line)
                return !budget.exhausted
            }
            found.append(entry)
            return found.count < 50_000
        }
        if let app, matches(bundle, hidden) {
            budget.take("(\(app.localizedName ?? "The frontmost app") is hidden from agents; its outline is withheld.)")
        } else if let app, AXIsProcessTrusted() {
            Engine.prepare(app)
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
                tree.walk(window, depthLimit: depth, viewport: view, take: take)
                if budget.exhausted { break }
            }
            if let bar = AXTree.element(AXTree.value(root, kAXMenuBarAttribute)) {
                // Include any menu currently pulled down from the menu bar, so the agent can choose an item. Closed menus have a
                // zero size.
                for item in AXTree.children(bar) {
                    for menu in AXTree.children(item) where AXTree.text(menu, kAXRoleAttribute) == "AXMenu" {
                        guard let box = AXTree.box(menu), box.width > 0, box.height > 0, !budget.exhausted else { continue }
                        tree.walk(menu, depthLimit: depth, viewport: view, take: take)
                    }
                }
                if input["menuBar"] as? Bool == true {
                    tree.walk(bar, depthLimit: 2, viewport: view, take: take)
                }
            }
            if !query.isEmpty {
                let kept = filterOutline(found, keeping: query)
                for entry in kept where budget.take(entry.line) {}
                if kept.isEmpty { budget.take("(Nothing in \(app.localizedName ?? "this app")'s outline matches \"\(shorten(query, 80))\".)") }
            }
            if chosen.isEmpty && floating.isEmpty {
                budget.take("(\(app.localizedName ?? "This app") exposes no Accessibility elements in any window, so only the screenshot can guide you.)")
            }
            if let focused = whole ? AXTree.focused() : AXTree.focused(in: app.processIdentifier) {
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
                if let zoom = input["zoom"] as? [String: Any], let region = closeUpRegion(zoom, view) {
                    // A close-up at full resolution; every coordinate in the reply stays in the snapshot's frame.
                    let picture = try Screenshot.closeUp(region, longest: Int(double(input["longest"]) ?? 1456), hidden: hidden, only: whole ? nil : own)
                    out["image"] = try Screenshot.jpeg(picture).base64EncodedString()
                    let area = view.pixels(region)
                    out["zoom"] = ["x": area.x.rounded(), "y": area.y.rounded(), "width": area.width.rounded(), "height": area.height.rounded(),
                                   "scale": Double(picture.width) / max(1, area.width)]
                } else {
                    let picture = try Screenshot.capture(size: view.size, hidden: hidden, only: whole ? nil : own)
                    out["image"] = try Screenshot.jpeg(picture).base64EncodedString()
                }
            }
        }
        return out
    }

    /// A zoom request's area in screen points: pixels of the snapshot, kept on the display and at least 8 pixels wide.
    func closeUpRegion(_ zoom: [String: Any], _ view: Viewport) -> Box? {
        guard let x = double(zoom["x"]), let y = double(zoom["y"]), let width = double(zoom["width"]), let height = double(zoom["height"]),
              width >= 8, height >= 8 else { return nil }
        let size = view.size
        let left = min(max(0, x), Double(size.width) - 8), top = min(max(0, y), Double(size.height) - 8)
        let area = Box(x: left, y: top, width: min(width, Double(size.width) - left), height: min(height, Double(size.height) - top))
        let start = view.points(area.x, area.y)
        let end = view.points(area.x + area.width, area.y + area.height)
        return Box(x: start.x, y: start.y, width: end.x - start.x, height: end.y - start.y)
    }

    /// Why none of `app`'s windows is on screen: `minimized`, `other-space` or `hidden`; nil when one is.
    func offscreen(_ app: NSRunningApplication) -> String? {
        let pid = app.processIdentifier
        if WindowList.entries().contains(where: { $0.pid == pid && $0.layer == 0 }) { return nil }
        if app.isHidden { return "hidden" }
        let windows = (AXTree.value(AXTree.application(pid), kAXWindowsAttribute) as? [AnyObject] ?? []).compactMap { AXTree.element($0) }
        if windows.contains(where: { AXTree.flag($0, kAXMinimizedAttribute) == true }) { return "minimized" }
        if WindowList.entries(onScreenOnly: false).contains(where: { $0.pid == pid && $0.layer == 0 && !$0.onScreen }) { return "other-space" }
        return nil
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

    /// The app keys and text without a ref go to: the looked-at app while it runs, else the app in front.
    func workingApp() -> NSRunningApplication? {
        if let app = lookedAt, !app.isTerminated { return app }
        return frontApp()
    }

    /// Reports the element and app an action would reach. Without a ref or a point that is where keystrokes go: the
    /// looked-at app's focused element, or the front app's.
    func inspect(_ input: [String: Any]) throws -> [String: Any] {
        // An action follows: a missing permission is reported now, before the person is asked to approve it.
        try ready(toAct: (input["acting"] as? Bool) ?? false)
        var out: [String: Any]
        var spot: (Double, Double)?
        var owner: NSRunningApplication?
        var target: AXUIElement?
        var inFront = true
        if input["ref"] != nil || input["x"] != nil {
            let found = try reach(input)
            spot = (found.x, found.y)
            owner = found.app
            inFront = found.inFront
            if (input["acting"] as? Bool) == true { try refusePasswordMenu(found.x, found.y, app: inFront ? nil : found.app) }
            target = found.element ?? AXTree.hit(found.x, found.y, in: found.app.processIdentifier)
                ?? (inFront ? AXTree.hit(found.x, found.y) : nil)
            out = target.map { AXTree.summary($0) } ?? [:]
            // A bare point names no native control: it counts as content.
            if found.element == nil { out["content"] = true }
        } else {
            owner = workingApp()
            target = owner.flatMap { AXTree.focused(in: $0.processIdentifier) }
            out = target.map { AXTree.summary($0) } ?? [:]
        }
        if out["bundle"] == nil, let owner {
            out["bundle"] = owner.bundleIdentifier ?? ""
            out["app"] = owner.localizedName ?? ""
            out["pid"] = Int(owner.processIdentifier)
        }
        if let owner { describe(into: &out, app: owner, element: target, spot: spot, input: input) }
        if let spot, let viewport {
            let pixel = (x: (spot.0 * viewport.factor).rounded(), y: (spot.1 * viewport.factor).rounded())
            out["point"] = ["x": pixel.x, "y": pixel.y]
            // The picture of a covered app shows that app's own windows, not the person's window over it.
            let only = inFront ? nil : [(out["bundle"] as? String) ?? ""] + Behind.helperProcesses
            if input["preview"] as? Bool == true, CGPreflightScreenCaptureAccess(),
               let picture = try? Screenshot.capture(size: viewport.size, hidden: (input["hidden"] as? [String]) ?? [], only: only) {
                out["preview"] = try? Screenshot.preview(picture, around: pixel).base64EncodedString()
            }
        }
        return out
    }

    /// What an approval or the reviewer should know about a target beyond the element itself: whether it is a terminal
    /// (where typing runs commands) or a Paste menu item, the window and the labelled area around it, the menu item a
    /// Command shortcut (`keys`) would press, and System Settings' open pane. Text read from the app is untrusted.
    func describe(into out: inout [String: Any], app: NSRunningApplication, element: AXUIElement?, spot: (Double, Double)?, input: [String: Any]) {
        let pid = app.processIdentifier
        if let element {
            if AXTree.isTerminal(element) { out["terminal"] = true }
            if AXTree.isPasteItem(element) { out["paste"] = true }
            if let context = AXTree.context(element) { out["context"] = context }
        }
        let window = element.flatMap(AXTree.window(of:))
            ?? spot.flatMap { WindowList.topmost(of: [pid], at: $0.0, $0.1) }.flatMap { axWindow(pid, $0.id) }
            ?? (element == nil && spot == nil ? AXTree.focusedWindow(in: pid) : nil)
        if let title = window.flatMap({ AXTree.text($0, kAXTitleAttribute) }), !title.isEmpty { out["window"] = shorten(title, 120) }
        if let keys = input["keys"] as? String, let combo = try? KeyCombo.read(keys), let found = menuItem(pid, for: combo), found.enabled {
            out["menu"] = shorten(found.path.filter { !$0.isEmpty }.joined(separator: " ▸ "), 160)
        }
        if let pane = Self.settingsPane(for: app) { out["pane"] = pane }
    }

    static let systemSettings = "com.apple.systempreferences"

    /// The System Settings pane a target in `app` belongs to: the selected sidebar entry of System Settings, read
    /// for System Settings itself and for the extension processes that draw its panes. Such a process is otherwise
    /// named by its own bundle identifier, which is the pane's.
    static func settingsPane(for app: NSRunningApplication) -> String? {
        let bundle = app.bundleIdentifier ?? ""
        let extensionLike = bundle.hasPrefix("com.apple.") && (bundle.lowercased().contains("settings") || bundle.hasSuffix(".extension"))
        guard bundle == systemSettings || extensionLike else { return nil }
        if let settings = NSRunningApplication.runningApplications(withBundleIdentifier: systemSettings).first,
           let pane = AXTree.settingsPane(settings.processIdentifier) {
            return pane
        }
        return bundle == systemSettings ? nil : bundle
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
        let button = (input["button"] as? String) ?? "left"
        let count = max(1, min(3, Int(double(input["count"]) ?? 1)))
        let modifiers = try held(input)
        var found = try reach(input)
        try confirm(input, "expect", found.app.bundleIdentifier)
        try refusePaste(found, input)
        var fallback: String?
        switch route(inFront: found.inFront) {
        case .behind:
            do {
                return try clickBehind(found, button: button, count: count, held: modifiers)
            } catch let forward as ComeForward {
                fallback = forward.reason
            }
        case .forward(let reason): fallback = reason
        case .front: break
        }
        // Hold the click until the person's typing pauses, so it cannot redirect their keystrokes.
        try Presence.shared.waitForQuiet()
        found = try reach(input)
        try confirm(input, "expect", found.app.bundleIdentifier)
        return try clickInFront(found, input, button: button, count: count, held: modifiers, fallback: fallback)
    }

    /// With `refusePaste` (the server sends it for terminals and script runners), a click on a Paste menu item is
    /// refused: what it pastes runs there as a command, and nobody saw it.
    func refusePaste(_ found: Reach, _ input: [String: Any]) throws {
        guard input["refusePaste"] as? Bool == true,
              let element = found.element ?? AXTree.hit(found.x, found.y, in: found.app.processIdentifier),
              AXTree.isPasteItem(element) else { return }
        throw HelperFailure("paste", "Pasting into \(found.app.localizedName ?? "this app") runs the pasted text as a command, outside Haarness's approvals, so agents never use Paste there. Run commands with the shell tool instead.")
    }

    /// A click through the system's pointer, on an app in front or brought forward (`fallback` says why).
    func clickInFront(_ found: Reach, _ input: [String: Any], button: String, count: Int, held modifiers: [Held], fallback: String?) throws -> [String: Any] {
        let (x, y) = (found.x, found.y)
        let before = windowFacts(found.app)
        Marker.shared.flash(x, y)
        // A plain click on an element that can be pressed goes through Accessibility: the pointer stays where it is
        // and the keyboard focus does not jump. A press that reached the app counts as done, even when its answer
        // timed out because a menu or sheet it opened held it up: clicking again would act a second time.
        if let element = found.element, button == "left", count == 1, modifiers.isEmpty, AXTree.actions(element).contains(kAXPressAction) {
            let root = AXTree.window(of: element)
            let earlier = observe(found.app, root: root)
            let arrived = Self.pressArrived(AXUIElementPerformAction(element, kAXPressAction as CFString))
            Thread.sleep(forTimeInterval: settle)
            if arrived == true || (arrived == nil && observe(found.app, root: root) != earlier) {
                return reply(found.app, via: "ax", background: WindowServer.frontPID() != found.app.processIdentifier, before: before)
            }
        }
        if fallback != nil { comeForward(found) } else if let element = found.element { Self.raise(element) }
        try ControlBar.shared.outOfTheWay(x, y) {
            // Whatever is topmost receives the pointer event, so confirm it is the approved app.
            try confirm(input, point: (x, y))
            try refusePasswordMenu(x, y)
            try ready(toAct: true)
            try Presence.shared.waitForQuiet()
            Synth.click(x, y, button: button, count: count, held: modifiers)
        }
        Thread.sleep(forTimeInterval: settle)
        return reply(found.app, via: "pointer", background: false, before: before, fallback: fallback)
    }

    /// While a password field has the keyboard focus, macOS may open its password suggestions under it: a pop-up in
    /// the app's own process offering the Passwords app or a password manager. Pointer input landing on any pop-up
    /// then is refused, so an agent never reaches saved passwords that way. For an app behind the person's window
    /// (`app`), its own pop-up at the spot counts, while it has a password field in use; the person's own password
    /// field elsewhere does not.
    func refusePasswordMenu(_ x: Double, _ y: Double, app: NSRunningApplication? = nil) throws {
        let refusal = HelperFailure("password", "A password suggestion menu is open there, under a focused password field, and passwords stay with the person. Press Escape to close it, or click another field first.")
        if let app {
            if let own = WindowList.topmost(of: [app.processIdentifier], at: x, y), own.layer != 0, passwordInUse(app) { throw refusal }
            return
        }
        guard IsSecureEventInputEnabled(), let top = WindowList.top(x, y), top.layer != 0 else { return }
        throw refusal
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

    /// Refuses keystrokes bound for a terminal inside another app, such as a code editor's built-in terminal: whatever
    /// is typed there runs as a command, outside Haarness's approvals, as in a terminal app.
    func refuseTerminal(_ element: AXUIElement?, in app: NSRunningApplication?) throws {
        guard let element, AXTree.isTerminal(element) else { return }
        throw HelperFailure("terminal", "\(app?.localizedName ?? "This app")'s built-in terminal runs whatever is typed into it, outside Haarness's approvals, so agents may click and scroll there but never type or press keys. Run commands with the shell tool instead, under its usual approvals.")
    }

    /// Puts the keyboard focus in `field` before typing, and makes sure it went there: setting focus can report success
    /// while it stays in the field that had it (web content does this), and the text would go to that other field.
    /// Where focus cannot be read at all, the click is trusted as before.
    func focus(_ field: AXUIElement, in app: NSRunningApplication, at spot: (Double, Double), _ input: [String: Any]) throws {
        let pid = app.processIdentifier
        let lands = { AXTree.focused(in: pid).map { AXTree.serves($0, field) } }
        if AXUIElementSetAttributeValue(field, kAXFocusedAttribute as CFString, kCFBooleanTrue) == .success {
            Thread.sleep(forTimeInterval: 0.1)
            if lands() != false { return }
        }
        try ControlBar.shared.outOfTheWay(spot.0, spot.1) {
            try confirm(input, point: spot)
            Marker.shared.flash(spot.0, spot.1)
            Synth.click(spot.0, spot.1, button: "left", count: 1, held: [])
        }
        Thread.sleep(forTimeInterval: 0.1)
        if lands() == false {
            throw HelperFailure("focus_unknown", "The field did not take the keyboard focus, so nothing was typed: the text would have gone to another field. Click the field with mac_click, then type without a ref.")
        }
    }

    func type(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let text = input["text"] as? String, !text.isEmpty else { throw HelperFailure("invalid", "There is nothing to type.") }
        let app: NSRunningApplication
        var field: AXUIElement?
        var spot: (Double, Double)?
        if input["ref"] != nil {
            let (x, y, found) = try locate(input)
            guard let found else { throw HelperFailure("invalid", "Typing needs a ref. Click the field first to type at a point.") }
            try confirm(input, element: found)
            try refusePassword(found, secureInput: false)
            guard let owner = AXTree.pid(found).flatMap({ NSRunningApplication(processIdentifier: $0) }) else {
                throw HelperFailure("not_found", "The app holding that field is gone; call mac_snapshot again.")
            }
            app = owner
            field = found
            spot = (x, y)
        } else {
            guard let owner = workingApp() else { throw HelperFailure("not_found", "No app is open to type into. Look at one with mac_snapshot first.") }
            app = owner
            try confirm(input, "expect", app.bundleIdentifier)
        }
        try refuseTerminal(field ?? AXTree.focused(in: app.processIdentifier), in: app)
        var fallback: String?
        switch route(inFront: WindowServer.frontPID() == app.processIdentifier) {
        case .behind:
            do {
                return try typeBehind(app, field: field, spot: spot, text: text)
            } catch let forward as ComeForward {
                fallback = forward.reason
            }
        case .forward(let reason): fallback = reason
        case .front: break
        }
        // Wait for the person's keyboard to go quiet; interleaved typing would garble both.
        try Presence.shared.waitForQuiet()
        let before = windowFacts(app)
        if fallback != nil { Self.bringForward(app) }
        if let field, let spot {
            Self.raise(field)
            try focus(field, in: app, at: spot, input)
        }
        // Keystrokes land in the focused element; it has to stay inside the approved app and may never be a password field,
        // both now and for every chunk.
        let check = {
            try self.confirm(input, "expect", Self.keyboardApp())
            try self.refusePassword(nil)
            try self.refuseTerminal(Self.keyboardFocus(), in: app)
        }
        try check()
        try Synth.type(text, check: check)
        Thread.sleep(forTimeInterval: settle)
        return reply(app, via: "pointer", background: false, before: before, fallback: fallback)
    }

    func fill(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let value = input["value"] as? String else { throw HelperFailure("invalid", "There is no value to set.") }
        let (x, y, element) = try locate(input)
        guard let element else { throw HelperFailure("invalid", "Replacing a value needs a ref.") }
        try confirm(input, element: element)
        guard let app = AXTree.pid(element).flatMap({ NSRunningApplication(processIdentifier: $0) }) else {
            throw HelperFailure("not_found", "The app holding that field is gone; call mac_snapshot again.")
        }
        let inFront = WindowServer.frontPID() == app.processIdentifier
        // Setting a value never touches the person's keyboard, so it waits for their typing only in the front app.
        if inFront { try Presence.shared.waitForQuiet() }
        // The value is set on the element itself, whatever has the keyboard.
        try refusePassword(element, secureInput: false)
        try refuseTerminal(element, in: app)
        let before = windowFacts(app)
        if inFront { Marker.shared.flash(x, y) } else { AgentPointer.shared.show(at: x, y) }
        guard AXUIElementSetAttributeValue(element, kAXValueAttribute as CFString, value as CFString) == .success else {
            throw HelperFailure("failed", "Setting this element's value directly failed; click into it and use mac_type.")
        }
        Thread.sleep(forTimeInterval: settle)
        let confirmed = AXTree.text(element, kAXValueAttribute).map { $0 == value }
        return reply(app, via: "ax", background: !inFront, before: before, confirmed: confirmed)
    }

    func press(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let keys = input["keys"] as? String else { throw HelperFailure("invalid", "Name the key to press.") }
        let combo = try KeyCombo.read(keys)
        let times = max(1, min(50, Int(double(input["times"]) ?? 1)))
        guard let app = workingApp() else { throw HelperFailure("not_found", "No app is open to press keys in. Look at one with mac_snapshot first.") }
        try confirm(input, "expect", app.bundleIdentifier)
        try refuseTerminal(AXTree.focused(in: app.processIdentifier), in: app)
        var fallback: String?
        switch route(inFront: WindowServer.frontPID() == app.processIdentifier) {
        case .behind:
            do {
                return try pressBehind(app, combo: combo, times: times)
            } catch let forward as ComeForward {
                fallback = forward.reason
            }
        case .forward(let reason): fallback = reason
        case .front: break
        }
        try Presence.shared.waitForQuiet()
        let before = windowFacts(app)
        if fallback != nil { Self.bringForward(app) }
        try confirm(input, "expect", Self.keyboardApp())
        // A printable key sent to a password field would be part of a password.
        if combo.types { try refusePassword(nil) }
        try refuseTerminal(Self.keyboardFocus(), in: app)
        try Synth.press(combo, times: times) {
            try self.confirm(input, "expect", Self.keyboardApp())
            if combo.types { try self.refusePassword(nil) }
            try self.refuseTerminal(Self.keyboardFocus(), in: app)
        }
        Thread.sleep(forTimeInterval: settle)
        return reply(app, via: "pointer", background: false, before: before, fallback: fallback)
    }

    func scroll(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        var found = try reach(input)
        try confirm(input, "expect", found.app.bundleIdentifier)
        let lines = Int32(max(1, min(50, double(input["amount"]) ?? 3)))
        let direction = (input["direction"] as? String) ?? "down"
        var fallback: String?
        switch route(inFront: found.inFront) {
        case .behind:
            do {
                return try scrollBehind(found, direction: direction, lines: lines)
            } catch let forward as ComeForward {
                fallback = forward.reason
            }
        case .forward(let reason): fallback = reason
        case .front: break
        }
        let before = windowFacts(found.app)
        if fallback != nil {
            comeForward(found)
            found = try reach(input)
        }
        let (x, y) = (found.x, found.y)
        // Scroll events go wherever the pointer is.
        try confirm(input, point: (x, y))
        try refusePasswordMenu(x, y)
        let (dx, dy): (Int32, Int32) = switch direction {
        case "up": (0, lines)
        case "left": (lines, 0)
        case "right": (-lines, 0)
        default: (0, -lines)
        }
        ControlBar.shared.outOfTheWay(x, y) { Synth.scroll(x, y, dx: dx, dy: dy) }
        Thread.sleep(forTimeInterval: settle)
        return reply(found.app, via: "pointer", background: false, before: before, fallback: fallback)
    }

    /// Drags need the real pointer, which carries what is dragged: an app behind the person's window comes forward.
    func drag(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        guard let pickup = input["from"] as? [String: Any], let drop = input["to"] as? [String: Any] else {
            throw HelperFailure("invalid", "mac_drag needs both a from and a to.")
        }
        try Presence.shared.waitForQuiet()
        let first = try reach(pickup)
        let fallback: String? = first.inFront ? nil : "drag"
        let before = windowFacts(first.app)
        if fallback != nil { comeForward(first) } else if let element = first.element { Self.raise(element) }
        let start = try locate(pickup)
        let end = try locate(drop)
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
        return reply(first.app, via: "pointer", background: false, before: before, fallback: fallback)
    }

    /// Hovering needs the real pointer too, so an app behind the person's window comes forward.
    func hover(_ input: [String: Any]) throws -> [String: Any] {
        try ready(toAct: true)
        var found = try reach(input)
        let fallback: String? = found.inFront ? nil : "hover"
        let before = windowFacts(found.app)
        if fallback != nil {
            comeForward(found)
            found = try reach(input)
        }
        let (x, y) = (found.x, found.y)
        try ControlBar.shared.outOfTheWay(x, y) {
            try confirm(input, point: (x, y))
            try refusePasswordMenu(x, y)
            Synth.hover(x, y)
        }
        Thread.sleep(forTimeInterval: settle)
        return reply(found.app, via: "pointer", background: false, before: before, fallback: fallback)
    }
}
