import AppKit
import ApplicationServices
import Carbon.HIToolbox
import IOKit
import MacKit

/// Working in an app behind the person's window, while their pointer, their front app and their typing stay as they
/// were. Each action first tries Accessibility (a press, a value, a menu item), then events posted to the app's own
/// process with the target window made key inside the app. Whatever cannot be proven to land where it should comes
/// forward and works as in the foreground instead.
enum Behind {
    /// `HAARNESS_MAC_FOREGROUND=1` brings every action's app forward, as before background delivery, for diagnosis.
    static let forced = ProcessInfo.processInfo.environment["HAARNESS_MAC_FOREGROUND"] == "1"

    /// Whether the window location of a posted event is relative to the window (see `windowLocationIsLocal`).
    static let windowLocal = windowLocationIsLocal(macOSMajor: ProcessInfo.processInfo.operatingSystemVersion.majorVersion)

    /// Processes whose windows count as part of the app they work for, such as the file dialogs sandboxed apps borrow.
    static let helperProcesses = ["com.apple.appkit.xpc.openAndSavePanelService"]

    private typealias Responsible = @convention(c) (pid_t) -> pid_t
    private static let responsible: Responsible? = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "responsibility_get_pid_responsible_for_pid")
        .map { unsafeBitCast($0, to: Responsible.self) }

    /// The file-dialog processes working for `app`. macOS runs one for each app that shows such a dialog, so only those
    /// it holds `app` responsible for count; none when that cannot be told.
    static func panels(of app: pid_t) -> Set<pid_t> {
        guard let responsible else { return [] }
        return Set(helperProcesses.flatMap { NSRunningApplication.runningApplications(withBundleIdentifier: $0) }
            .map(\.processIdentifier).filter { responsible($0) == app })
    }

    /// The processes that ask for a password or a fingerprint on macOS's behalf: replies never mention their windows.
    static let authorization: Set<String> = [
        "SecurityAgent", "coreautha", "LocalAuthenticationRemoteService", "com.apple.SecurityAgent", "com.apple.coreautha",
        "com.apple.LocalAuthentication.UIAgent", "com.apple.LocalAuthenticationRemoteService",
    ]

    /// `app` and its own file dialogs.
    static func own(_ app: pid_t) -> Set<pid_t> {
        Set([app]).union(panels(of: app))
    }

    /// The process that switched on secure keyboard input, which apps do while one of their password fields has focus.
    /// Nil when it is off or its owner cannot be told.
    static func secureInputOwner() -> pid_t? {
        if let pid = (CGSessionCopyCurrentDictionary() as? [String: Any])?["kCGSSessionSecureInputPID"] as? Int { return pid_t(pid) }
        let root = IORegistryGetRootEntry(kIOMainPortDefault)
        defer { IOObjectRelease(root) }
        guard let users = IORegistryEntryCreateCFProperty(root, "IOConsoleUsers" as CFString, kCFAllocatorDefault, 0)?
            .takeRetainedValue() as? [[String: Any]] else { return nil }
        return users.lazy.compactMap { $0["kCGSSessionSecureInputPID"] as? Int }.first.map { pid_t($0) }
    }
}

/// Chromium and Electron apps, which need a press outside their windows before a background click, ignore wheel events
/// sent to their process, and build their Accessibility tree only when asked.
enum Engine {
    private static let lock = NSLock()
    private static var known: [String: Bool] = [:]
    private static var asked: Set<String> = []

    static let browsers = [
        "com.google.Chrome*", "org.chromium.*", "com.microsoft.edgemac*", "com.brave.Browser*", "com.vivaldi.Vivaldi*",
        "com.operasoftware.Opera*", "company.thebrowser.*",
    ]

    static func isChromium(_ app: NSRunningApplication) -> Bool {
        if matches(app.bundleIdentifier ?? "", browsers) { return true }
        guard let url = app.bundleURL else { return false }
        lock.lock()
        if let answer = known[url.path] {
            lock.unlock()
            return answer
        }
        lock.unlock()
        let frameworks = url.appendingPathComponent("Contents/Frameworks")
        let names = (try? FileManager.default.contentsOfDirectory(atPath: frameworks.path)) ?? []
        let answer = names.contains { name in
            if name == "Electron Framework.framework" || name == "Chromium Embedded Framework.framework" { return true }
            // Chromium browsers keep their renderer helper inside "<Name> Framework.framework".
            guard name.hasSuffix(" Framework.framework") else { return false }
            let helpers = frameworks.appendingPathComponent(name).appendingPathComponent("Versions/Current/Helpers").path
            return ((try? FileManager.default.contentsOfDirectory(atPath: helpers)) ?? []).contains { $0.hasSuffix("Helper (Renderer).app") }
        }
        lock.lock()
        known[url.path] = answer
        lock.unlock()
        return answer
    }

    /// Asks a Chromium or Electron app for its Accessibility tree once per run of the app, and gives it a moment to
    /// build it.
    static func prepare(_ app: NSRunningApplication) {
        guard isChromium(app) else { return }
        let run = "\(app.processIdentifier)-\(app.launchDate?.timeIntervalSince1970 ?? 0)"
        lock.lock()
        let first = asked.insert(run).inserted
        lock.unlock()
        guard first else { return }
        AXTree.askForTree(app.processIdentifier)
        let root = AXTree.application(app.processIdentifier)
        let before = AXTree.digest(root, limit: 200)
        for _ in 0..<10 {
            Thread.sleep(forTimeInterval: 0.1)
            if AXTree.digest(root, limit: 200) != before { break }
        }
    }
}

/// Thrown by a background attempt that cannot go on behind the person's window: the action comes forward instead, and
/// the reply's `fallback` gives the reason. Every case where the target's window cannot take the input from behind
/// (no key window proven, no window on screen) reports `no-key-window`.
struct ComeForward: Error {
    let reason: String

    init(_ reason: String) {
        self.reason = reason
    }
}

extension Dispatcher {
    /// How an action reaches its app.
    enum Route {
        /// Frontmost and uncovered where it is acted on: the system's pointer and keyboard, as always.
        case front
        /// The app is behind the person's window: Accessibility, menu items and events posted to its process.
        case behind
        /// The app is brought forward first, for the reason given (the reply's `fallback`).
        case forward(String)
    }

    /// What a pointer action reaches.
    struct Reach {
        let app: NSRunningApplication
        let element: AXUIElement?
        /// The spot, in screen points.
        let x: Double
        let y: Double
        /// The app's own topmost window under the spot.
        let window: WindowList.Entry?
        /// True when the app is frontmost and nothing but the app itself lies at the spot.
        let inFront: Bool
    }

    /// The app whose windows a point belongs to when the agent looks at a covered app: the looked-at app wherever one
    /// of its windows (or a file dialog it borrows) lies under the point, whatever covers it.
    func lookedAtOwner(_ x: Double, _ y: Double) -> NSRunningApplication? {
        guard let app = lookedAt, !app.isTerminated else { return nil }
        guard WindowList.topmost(of: Behind.own(app.processIdentifier), at: x, y) != nil else { return nil }
        return app
    }

    func reach(_ input: [String: Any]) throws -> Reach {
        let (x, y, element) = try locate(input)
        let owner: NSRunningApplication? = {
            if let element, let pid = AXTree.pid(element) { return NSRunningApplication(processIdentifier: pid) }
            if let app = lookedAtOwner(x, y) { return app }
            if let hit = AXTree.hit(x, y), let pid = AXTree.pid(hit) { return NSRunningApplication(processIdentifier: pid) }
            return WindowList.owner(x, y)
        }()
        guard let app = owner else {
            throw HelperFailure("not_found", "Nothing at that position belongs to an app. Call mac_snapshot and use a ref.")
        }
        let pids = Behind.own(app.processIdentifier)
        let window = WindowList.topmost(of: pids, at: x, y)
        // The menu bar, status items and the Dock sit above every window, so the system's pointer reaches what an app
        // shows there whether or not the app is in front.
        let top = WindowList.top(x, y)
        let furniture = top.map { (20...25).contains($0.layer) } ?? false
        let onTop = top.map { pids.contains($0.pid) } ?? true
        let hitHere = furniture && (AXTree.hit(x, y).flatMap(AXTree.pid).map { pids.contains($0) } ?? false)
        let inFront = (furniture && (onTop || hitHere)) || (WindowServer.frontPID() == app.processIdentifier && onTop)
        return Reach(app: app, element: element, x: x, y: y, window: window, inFront: inFront)
    }

    func route(inFront: Bool) -> Route {
        if inFront { return .front }
        if Behind.forced { return .forward("forced") }
        if !WindowServer.available { return .forward("private-calls-missing") }
        return .behind
    }

    /// Notes where the agent works now, for Escape (see `escapeStops`).
    func working(_ app: NSRunningApplication?) {
        escapeLock.lock()
        workingPid = app?.processIdentifier
        escapeLock.unlock()
    }

    // MARK: Replies

    /// What the app showed before an action, to tell afterwards whether it reacted.
    struct Observed: Equatable {
        var windows: [String]
        var focus: String
        var digest: Int
    }

    func observe(_ app: NSRunningApplication, root: AXUIElement?) -> Observed {
        let pid = app.processIdentifier
        let windows = WindowList.entries().filter { $0.pid == pid }.map { "\($0.id) \($0.layer) \($0.title ?? "") \($0.frame.json)" }
        let focus = AXTree.focused(in: pid).map { element in
            [kAXRoleAttribute, kAXTitleAttribute, kAXValueAttribute].map { AXTree.text(element, $0) ?? "" }.joined(separator: "|")
        } ?? ""
        let digest = (root ?? AXTree.focusedWindow(in: pid)).map { AXTree.digest($0) } ?? 0
        return Observed(windows: windows, focus: focus, digest: digest)
    }

    /// Every window on screen, with roles read for the acted-on app's own, leaving out the apps computer use keeps
    /// invisible and macOS's password and fingerprint prompts.
    func windowFacts(_ app: NSRunningApplication?) -> [WindowFacts] {
        var roles: [CGWindowID: (String?, String?)] = [:]
        let windows = app.map { AXTree.value(AXTree.application($0.processIdentifier), kAXWindowsAttribute) as? [AnyObject] ?? [] } ?? []
        for window in windows.compactMap({ AXTree.element($0) }) {
            if let number = WindowServer.windowNumber(window) {
                roles[number] = (AXTree.text(window, kAXRoleAttribute), AXTree.text(window, kAXSubroleAttribute))
            }
        }
        return WindowList.entries()
            .filter { entry in
                guard !(20...25).contains(entry.layer), !Behind.authorization.contains(entry.app) else { return false }
                let bundle = NSRunningApplication(processIdentifier: entry.pid)?.bundleIdentifier ?? ""
                return !matches(bundle, hiddenApps) && !Behind.authorization.contains(bundle)
            }
            .map { entry in
                WindowFacts(id: Int(entry.id), pid: entry.pid, app: entry.app, layer: entry.layer, title: entry.title,
                            role: roles[entry.id]?.0, subrole: roles[entry.id]?.1)
            }
    }

    /// An action's reply: how it was delivered, to which app, whether the app reacted, and the windows it opened or
    /// closed.
    func reply(_ app: NSRunningApplication, via: String, background: Bool, before: [WindowFacts], fallback: String? = nil,
               confirmed: Bool? = nil, handedBack: Bool = false) -> [String: Any] {
        let changes = windowChanges(before: before, after: windowFacts(app), target: app.processIdentifier)
        var out: [String: Any] = [
            "via": via,
            "background": background,
            "target": ["bundle": app.bundleIdentifier ?? "", "pid": Int(app.processIdentifier), "name": app.localizedName ?? ""],
            "opened": changes.opened,
            "closed": changes.closed,
        ]
        if let fallback { out["fallback"] = fallback }
        if let confirmed { out["confirmed"] = confirmed }
        if handedBack { out["handedBack"] = true }
        working(app)
        return out
    }

    // MARK: The person's front app

    func frontApp() -> NSRunningApplication? {
        NSWorkspace.shared.frontmostApplication
    }

    /// The Accessibility window of `pid` that the window server knows as `id`.
    func axWindow(_ pid: pid_t, _ id: CGWindowID?) -> AXUIElement? {
        guard let id else { return nil }
        return (AXTree.value(AXTree.application(pid), kAXWindowsAttribute) as? [AnyObject] ?? [])
            .compactMap { AXTree.element($0) }
            .first { WindowServer.windowNumber($0) == id }
    }

    /// Returns the front to `prior`, the app the person had in front, when `app` came forward on its own after an
    /// action that began at `start`; not when the person clicked since, which makes the switch theirs. Says whether it
    /// did.
    func handBack(from app: NSRunningApplication, to prior: NSRunningApplication?, since start: Date) -> Bool {
        guard let prior, prior.processIdentifier != app.processIdentifier, !prior.isTerminated else { return false }
        // An app that comes forward does so while it handles the action, so by now it shows; a short second look
        // catches one that does it a moment later.
        var raised = WindowServer.frontPID() == app.processIdentifier
        if !raised {
            Thread.sleep(forTimeInterval: 0.1)
            raised = WindowServer.frontPID() == app.processIdentifier
        }
        guard raised, !Presence.shared.happened(since: start, .button) else { return false }
        Self.bringForward(prior)
        return true
    }

    // MARK: Safety

    /// The background password rule: keystrokes bound for `app` are refused when its own focused element (`element`,
    /// or the one read now) is a password field or cannot be read, and while `app` holds secure keyboard input. Secure
    /// input another app switched on, such as the person's own password field, does not count.
    func refusePasswordBehind(_ app: NSRunningApplication, element: AXUIElement?) throws {
        if IsSecureEventInputEnabled() {
            let owner = Behind.secureInputOwner()
            if owner == nil || owner == app.processIdentifier {
                throw HelperFailure("password", "Password fields are refused; the person enters passwords themselves. \(app.localizedName ?? "The app") has a password field in use right now.")
            }
        }
        guard let target = element ?? AXTree.focused(in: app.processIdentifier) else {
            throw HelperFailure("focus_unknown", "The field with the keyboard focus in \(app.localizedName ?? "the app") can't be read, and it might take a password, so nothing was typed. Pass the field's ref instead.")
        }
        if AXTree.isPassword(target) {
            throw HelperFailure("password", "Password fields are refused; the person enters passwords themselves.")
        }
    }

    /// Whether `app` has a password field in use: focused, or holding secure input.
    func passwordInUse(_ app: NSRunningApplication) -> Bool {
        if IsSecureEventInputEnabled(), Behind.secureInputOwner() == app.processIdentifier { return true }
        return AXTree.focused(in: app.processIdentifier).map(AXTree.isPassword) ?? false
    }

    /// Whether an Accessibility press reached the app (see `pressArrived(code:)`).
    static func pressArrived(_ result: AXError) -> Bool? {
        MacKit.pressArrived(code: result.rawValue)
    }

    /// Brings a reach's app forward and raises its window, for an action that has to use the system's pointer.
    func comeForward(_ found: Reach) {
        if let element = found.element {
            Self.raise(element)
            return
        }
        Self.bringForward(found.app)
        if let window = axWindow(found.app.processIdentifier, found.window?.id) {
            AXUIElementPerformAction(window, kAXRaiseAction as CFString)
            Thread.sleep(forTimeInterval: 0.05)
        }
    }

    // MARK: Clicks

    func clickBehind(_ found: Reach, button: String, count: Int, held: [Held]) throws -> [String: Any] {
        try ControlBar.shared.refuseWhilePaused()
        let app = found.app
        let before = windowFacts(app)
        let rootWindow = found.element.flatMap(AXTree.window(of:)) ?? axWindow(app.processIdentifier, found.window?.id)
        let earlier = observe(app, root: rootWindow)
        let prior = frontApp()
        let started = Date()
        var shown = false
        if let element = found.element, button == "left", count == 1, held.isEmpty, AXTree.actions(element).contains(kAXPressAction) {
            AgentPointer.shared.show(at: found.x, found.y)
            shown = true
            let arrived = Self.pressArrived(AXUIElementPerformAction(element, kAXPressAction as CFString))
            Thread.sleep(forTimeInterval: settle)
            let changed = observe(app, root: rootWindow) != earlier
            if arrived == true || (arrived == nil && changed) {
                let back = handBack(from: app, to: prior, since: started)
                return reply(app, via: "ax", background: true, before: before, confirmed: changed ? true : nil, handedBack: back)
            }
        }
        if app.isHidden { throw Self.hiddenFailure(app) }
        guard let window = found.window else { throw ComeForward("no-key-window") }
        // macOS's password suggestions open as a pop-up of the app under its focused password field.
        if window.layer != 0 && passwordInUse(app) {
            throw HelperFailure("password", "A password suggestion menu is open there, under a focused password field, and passwords stay with the person. Press Escape to close it, or click another field first.")
        }
        // The events go to the app or to a file dialog working for it, never to another app's window.
        guard Behind.own(app.processIdentifier).contains(window.pid) else {
            throw HelperFailure("target_changed", "Something moved: the window there belongs to another app now. Look again with mac_snapshot and retry.")
        }
        // An AppKit window that is not key spends a click on becoming key; made key first, it takes the click itself.
        let owner = window.pid == app.processIdentifier ? app : (NSRunningApplication(processIdentifier: window.pid) ?? app)
        _ = prove(owner, window: window.id, field: nil, spot: nil)
        if !shown { AgentPointer.shared.show(at: found.x, found.y) }
        // The window may have closed or moved since the spot was resolved.
        guard WindowList.topmost(of: [window.pid], at: found.x, found.y)?.id == window.id else {
            throw HelperFailure("target_changed", "Something moved: \(app.localizedName ?? "the app") no longer has a window there. Look again with mac_snapshot and retry.")
        }
        try ControlBar.shared.refuseWhilePaused()
        Synth.clickBehind(pid: window.pid, window: window.id, origin: Point(window.frame.x, window.frame.y), at: Point(found.x, found.y),
                          button: button, count: count, held: held, outsidePress: Engine.isChromium(app))
        Thread.sleep(forTimeInterval: settle)
        let changed = observe(app, root: rootWindow) != earlier
        let back = handBack(from: app, to: prior, since: started)
        return reply(app, via: "process", background: true, before: before, confirmed: changed, handedBack: back)
    }

    static func hiddenFailure(_ app: NSRunningApplication) -> HelperFailure {
        HelperFailure("hidden", "\(app.localizedName ?? "The app") is hidden, and this action can't reach it without showing it. mac_apps with action activate brings it forward.")
    }

    // MARK: Scrolling

    /// The scroll area (or web area) under a spot of one app.
    func scrollArea(_ app: pid_t, _ x: Double, _ y: Double, element: AXUIElement?) -> AXUIElement? {
        var current = element ?? AXTree.hit(x, y, in: app)
        for _ in 0..<30 {
            guard let here = current else { return nil }
            if ["AXScrollArea", "AXWebArea"].contains(AXTree.text(here, kAXRoleAttribute) ?? "") { return here }
            current = AXTree.parent(here)
        }
        return nil
    }

    /// Where a scroll area stands: its scroll bars and the position of its first child.
    func scrollPosition(_ area: AXUIElement) -> String {
        let bars = [kAXVerticalScrollBarAttribute, kAXHorizontalScrollBarAttribute].map { name in
            AXTree.element(AXTree.value(area, name)).flatMap { AXTree.text($0, kAXValueAttribute) } ?? "-"
        }
        let first = AXTree.children(area).first.flatMap(AXTree.children).flatMap(\.first).flatMap(AXTree.box)
            ?? AXTree.children(area).first.flatMap(AXTree.box)
        return bars.joined(separator: ",") + "|" + (first.map { "\($0.x),\($0.y)" } ?? "-")
    }

    /// Scrolls through Accessibility: brings into view the element `amount` places past the last one visible in that
    /// direction. Returns whether the area moved.
    func scrollWithAccessibility(_ area: AXUIElement, direction: String, amount: Int) -> Bool {
        guard let frame = AXTree.box(area) else { return false }
        let before = scrollPosition(area)
        var beyond: [(distance: Double, element: AXUIElement)] = []
        var stack = AXTree.children(area)
        var seen = 0
        while let next = stack.popLast(), seen < 1500 {
            seen += 1
            if let box = AXTree.box(next), box.hasArea {
                let distance: Double? = switch direction {
                case "up": box.y + box.height <= frame.y ? frame.y - (box.y + box.height) : nil
                case "left": box.x + box.width <= frame.x ? frame.x - (box.x + box.width) : nil
                case "right": box.x >= frame.x + frame.width ? box.x - (frame.x + frame.width) : nil
                default: box.y >= frame.y + frame.height ? box.y - (frame.y + frame.height) : nil
                }
                if let distance {
                    beyond.append((distance, next))
                    continue
                }
            }
            stack += AXTree.children(next)
        }
        beyond.sort { $0.distance < $1.distance }
        guard !beyond.isEmpty else { return false }
        let pick = beyond[min(max(1, amount), beyond.count) - 1].element
        AXUIElementPerformAction(pick, "AXScrollToVisible" as CFString)
        Thread.sleep(forTimeInterval: 0.15)
        return scrollPosition(area) != before
    }

    func scrollBehind(_ found: Reach, direction: String, lines: Int32) throws -> [String: Any] {
        try ControlBar.shared.refuseWhilePaused()
        let app = found.app
        let before = windowFacts(app)
        let prior = frontApp()
        let started = Date()
        if let window = found.window, window.layer != 0 && passwordInUse(app) {
            throw HelperFailure("password", "A password suggestion menu is open there, under a focused password field, and passwords stay with the person. Press Escape to close it, or click another field first.")
        }
        let area = scrollArea(app.processIdentifier, found.x, found.y, element: found.element)
        let chromium = Engine.isChromium(app)
        AgentPointer.shared.show(at: found.x, found.y)
        if let area, chromium || app.isHidden, scrollWithAccessibility(area, direction: direction, amount: Int(lines)) {
            return reply(app, via: "ax", background: true, before: before, confirmed: true, handedBack: handBack(from: app, to: prior, since: started))
        }
        if app.isHidden { throw Self.hiddenFailure(app) }
        guard let window = found.window else { throw ComeForward("no-key-window") }
        guard Behind.own(app.processIdentifier).contains(window.pid) else {
            throw HelperFailure("target_changed", "Something moved: the window there belongs to another app now. Look again with mac_snapshot and retry.")
        }
        let position = area.map(scrollPosition)
        let (dx, dy): (Int32, Int32) = switch direction {
        case "up": (0, lines)
        case "left": (lines, 0)
        case "right": (-lines, 0)
        default: (0, -lines)
        }
        Synth.scrollBehind(pid: window.pid, window: window.id, origin: Point(window.frame.x, window.frame.y), at: Point(found.x, found.y), dx: dx, dy: dy)
        Thread.sleep(forTimeInterval: settle)
        var via = "process"
        var moved: Bool? = nil
        if let area, let position {
            moved = scrollPosition(area) != position
            if moved == false && scrollWithAccessibility(area, direction: direction, amount: Int(lines)) {
                via = "ax"
                moved = true
            }
        }
        return reply(app, via: via, background: true, before: before, confirmed: moved, handedBack: handBack(from: app, to: prior, since: started))
    }

    // MARK: Keys and text

    /// The window number of the window that has the keyboard focus inside `app`.
    func keyWindow(_ app: pid_t) -> CGWindowID? {
        AXTree.focusedWindow(in: app).flatMap(WindowServer.windowNumber)
    }

    /// Turns `window` into `app`'s key window without activating the app and, given a `field`, moves the app's keyboard
    /// focus into it, then reads both back. Keystrokes are posted only after this returns true: one that went to another
    /// window or field of the same app (a search field, another document) could do anything there.
    func prove(_ app: NSRunningApplication, window: CGWindowID, field: AXUIElement?, spot: (Double, Double)?) -> Bool {
        let pid = app.processIdentifier
        let keyed = { self.keyWindow(pid) == window }
        let focused = { field.map { field in AXTree.focused(in: pid).map { AXTree.serves($0, field) } ?? false } ?? true }
        func waitFor(_ seconds: Double, _ test: () -> Bool) -> Bool {
            let end = Date().addingTimeInterval(seconds)
            while !test() {
                if Date() >= end { return false }
                Thread.sleep(forTimeInterval: 0.04)
            }
            return true
        }
        // The app the person has in front keeps its key window and focus as they are: they are typing there.
        if WindowServer.frontPID() == pid { return keyed() && focused() }
        // The window is made key even when Accessibility already reports it focused: an app that is not active may have
        // no key window at all, and would drop the keys.
        let current = keyWindow(pid)
        _ = WindowServer.makeKey(pid: pid, window: window, current: current == window ? nil : current)
        for attempt in 0..<2 {
            if !waitFor(0.4, keyed) {
                _ = WindowServer.makeKey(pid: pid, window: window, current: keyWindow(pid))
                if !waitFor(0.4, keyed) { continue }
            }
            if focused() { return true }
            if let field {
                AXUIElementSetAttributeValue(field, kAXFocusedAttribute as CFString, kCFBooleanTrue)
                if waitFor(0.2, { keyed() && focused() }) { return true }
                // A field that will not take focus that way takes it from a click, as a person's would.
                if attempt == 0, let spot, let entry = WindowList.entries().first(where: { $0.id == window }) {
                    Synth.clickBehind(pid: pid, window: window, origin: Point(entry.frame.x, entry.frame.y), at: Point(spot.0, spot.1),
                                      button: "left", count: 1, held: [], outsidePress: Engine.isChromium(app))
                    if waitFor(0.3, { keyed() && focused() }) { return true }
                }
            }
        }
        return keyed() && focused()
    }


    /// Inserts a single line at the cursor of an app's own text field through Accessibility, then reads the field
    /// back (see `insertOutcome`). Nil when the field refused the insert, so the text can go in as keys instead.
    func insertWithAccessibility(_ field: AXUIElement, _ text: String) -> InsertOutcome? {
        let before = AXTree.text(field, kAXValueAttribute) ?? ""
        if AXTree.flag(field, kAXFocusedAttribute) != true {
            AXUIElementSetAttributeValue(field, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        }
        guard AXUIElementSetAttributeValue(field, kAXSelectedTextAttribute as CFString, text as CFString) == .success else { return nil }
        var outcome = InsertOutcome.unchanged
        for _ in 0..<8 {
            outcome = insertOutcome(before: before, after: AXTree.text(field, kAXValueAttribute) ?? "", text: text)
            if outcome == .arrived { break }
            Thread.sleep(forTimeInterval: 0.04)
        }
        return outcome
    }

    func typeBehind(_ app: NSRunningApplication, field: AXUIElement?, spot: (Double, Double)?, text: String) throws -> [String: Any] {
        try ControlBar.shared.refuseWhilePaused()
        try refusePasswordBehind(app, element: field)
        let before = windowFacts(app)
        let prior = frontApp()
        let started = Date()
        let target = field ?? AXTree.focused(in: app.processIdentifier)
        let value = target.flatMap { AXTree.text($0, kAXValueAttribute) }
        if let target, isOneLine(text), AXTree.isNativeTextField(target), let outcome = insertWithAccessibility(target, text), outcome != .unchanged {
            // A field that reformatted the text still took it: typing it again would enter it twice.
            return reply(app, via: "ax", background: true, before: before, confirmed: outcome == .arrived ? true : nil,
                         handedBack: handBack(from: app, to: prior, since: started))
        }
        if app.isHidden { throw Self.hiddenFailure(app) }
        guard let target, let window = AXTree.window(of: target).flatMap(WindowServer.windowNumber),
              prove(app, window: window, field: target, spot: spot) else { throw ComeForward("no-key-window") }
        let pid = app.processIdentifier
        let check = {
            try ControlBar.shared.refuseWhilePaused()
            guard self.keyWindow(pid) == window, let now = AXTree.focused(in: pid), AXTree.serves(now, target) else {
                throw HelperFailure("target_changed", "The field lost the keyboard focus in \(app.localizedName ?? "the app"), so typing stopped. Call mac_snapshot and retry.")
            }
            try self.refusePasswordBehind(app, element: now)
            try self.refuseTerminal(now, in: app)
        }
        try check()
        try Synth.type(text, to: .process(pid), interruptible: false, check: check)
        Thread.sleep(forTimeInterval: settle)
        let confirmed = value.map { $0 != (AXTree.text(target, kAXValueAttribute) ?? $0) }
        return reply(app, via: "process", background: true, before: before, confirmed: confirmed, handedBack: handBack(from: app, to: prior, since: started))
    }

    /// The menu item `combo` stands for in `app`'s menu bar, an enabled one first, with the titles leading to it. The
    /// Apple menu is left alone: its items belong to the system.
    func menuItem(_ app: pid_t, for combo: KeyCombo) -> (item: AXUIElement, enabled: Bool, path: [String])? {
        guard combo.mayBeMenuShortcut, let bar = AXTree.element(AXTree.value(AXTree.application(app), kAXMenuBarAttribute)) else { return nil }
        var found: (item: AXUIElement, enabled: Bool, path: [String])?
        var budget = 2000
        func items(_ parent: AXUIElement) -> [AXUIElement] {
            AXTree.children(parent).flatMap { AXTree.text($0, kAXRoleAttribute) == "AXMenu" ? AXTree.children($0) : [$0] }
        }
        func search(_ parent: AXUIElement, path: [String], depth: Int) {
            for item in items(parent) {
                if budget <= 0 || found?.enabled == true { return }
                budget -= 1
                let title = AXTree.text(item, kAXTitleAttribute) ?? ""
                let character = AXTree.text(item, kAXMenuItemCmdCharAttribute) ?? ""
                if !character.isEmpty {
                    let modifiers = (AXTree.value(item, kAXMenuItemCmdModifiersAttribute) as? NSNumber)?.intValue ?? 0
                    if combo.matchesMenuItem(character: character, modifiers: modifiers) {
                        let enabled = AXTree.flag(item, kAXEnabledAttribute) ?? true
                        if found == nil || enabled { found = (item, enabled, path + [title]) }
                    }
                } else if depth < 3 {
                    search(item, path: path + [title], depth: depth + 1)
                }
            }
        }
        for (index, top) in items(bar).enumerated() where index > 0 {
            search(top, path: [AXTree.text(top, kAXTitleAttribute) ?? ""], depth: 1)
            if found?.enabled == true { break }
        }
        return found
    }

    func pressBehind(_ app: NSRunningApplication, combo: KeyCombo, times: Int) throws -> [String: Any] {
        try ControlBar.shared.refuseWhilePaused()
        let pid = app.processIdentifier
        // Refused before anything is pressed, on the keys that would really go down.
        _ = try Synth.resolve(combo)
        if combo.types { try refusePasswordBehind(app, element: nil) }
        let before = windowFacts(app)
        let prior = frontApp()
        let started = Date()
        let focusedWindow = AXTree.focusedWindow(in: pid)
            ?? AXTree.element(AXTree.value(AXTree.application(pid), kAXMainWindowAttribute))
        let earlier = observe(app, root: focusedWindow)
        if var menu = menuItem(pid, for: combo) {
            if !menu.enabled, let window = focusedWindow.flatMap(WindowServer.windowNumber) {
                // Menu items that act on a window are enabled only while it is key.
                _ = prove(app, window: window, field: nil, spot: nil)
                menu = menuItem(pid, for: combo) ?? menu
            }
            // A disabled item (Copy with nothing selected) would do nothing in front either, so the app is left behind.
            guard menu.enabled else {
                var out = reply(app, via: "menu", background: true, before: before, confirmed: false)
                out["disabled"] = true
                out["menu"] = shorten(menu.path.filter { !$0.isEmpty }.joined(separator: " ▸ "), 160)
                return out
            }
            for index in 0..<max(1, times) {
                if index > 0 { try ControlBar.shared.refuseWhilePaused() }
                AXUIElementPerformAction(menu.item, kAXPressAction as CFString)
            }
            Thread.sleep(forTimeInterval: settle)
            let changed = observe(app, root: focusedWindow) != earlier
            return reply(app, via: "menu", background: true, before: before, confirmed: changed ? true : nil,
                         handedBack: handBack(from: app, to: prior, since: started))
        }
        if app.isHidden { throw Self.hiddenFailure(app) }
        let field = AXTree.focused(in: pid)
        guard let window = (field.flatMap(AXTree.window(of:)) ?? focusedWindow).flatMap(WindowServer.windowNumber),
              prove(app, window: window, field: field, spot: nil) else { throw ComeForward("no-key-window") }
        try Synth.press(combo, times: times, to: .process(pid)) {
            try ControlBar.shared.refuseWhilePaused()
            guard self.keyWindow(pid) == window else {
                throw HelperFailure("target_changed", "\(app.localizedName ?? "The app")'s window lost the keyboard focus, so the remaining presses were not sent. Call mac_snapshot and retry.")
            }
            if combo.types { try self.refusePasswordBehind(app, element: nil) }
            try self.refuseTerminal(AXTree.focused(in: pid), in: app)
        }
        Thread.sleep(forTimeInterval: settle)
        let changed = observe(app, root: focusedWindow) != earlier
        return reply(app, via: "process", background: true, before: before, confirmed: changed,
                     handedBack: handBack(from: app, to: prior, since: started))
    }
}
