// A small Mac app for the live computer use test (scripts/test-computer-use-live.py): a counter button, a name field,
// a password field, a slider, a scrolling list, a canvas that counts clicks, a notes text view, a button that brings the
// app forward and a Submit button that Return presses; its Lab menu has Bump (Command-B, adds ten). After every change
// it writes its state as JSON to the file named by its first argument, so the test checks what the agent's actions
// did.
//
// `--person <state>` runs it as the person's own app instead: a large window over the lab with a text view and a
// password field, recording what the person sees (whether it stays in front and keeps its key window, where the
// pointer is, what was typed into it). Lines written to `<state>.cmd` steer it: `secure` and `text` move its focus,
// `front` brings it forward.
import AppKit
import ApplicationServices
import Carbon.HIToolbox

// Errands for the test that need no window: press Escape as the person would (an event the helper did not post),
// type as the person would, list the on-screen windows of a process (the helper's bar and pointer), and read the
// bar's words or press one of its buttons through Accessibility, as a person clicking it would.
if CommandLine.arguments.dropFirst().first == "--press-escape" {
    let source = CGEventSource(stateID: .hidSystemState)
    CGEvent(keyboardEventSource: source, virtualKey: 0x35, keyDown: true)?.post(tap: .cghidEventTap)
    usleep(40_000)
    CGEvent(keyboardEventSource: source, virtualKey: 0x35, keyDown: false)?.post(tap: .cghidEventTap)
    // Leaving at once can lose the press on a busy Mac: neither the front app nor the helper ever sees it.
    usleep(300_000)
    exit(0)
}
// `--typist <seconds> <text>`: types `text` over and over into whatever is in front, one character every 120 ms.
if CommandLine.arguments.dropFirst().first == "--typist", CommandLine.arguments.count >= 4, let seconds = Double(CommandLine.arguments[2]) {
    let source = CGEventSource(stateID: .hidSystemState)
    let units = Array(CommandLine.arguments[3].utf16)
    let end = Date().addingTimeInterval(seconds)
    var index = 0
    while Date() < end {
        var unit = units[index % units.count]
        for down in [true, false] {
            let event = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
            event?.keyboardSetUnicodeString(stringLength: 1, unicodeString: &unit)
            event?.flags = []
            event?.post(tap: .cghidEventTap)
            usleep(15_000)
        }
        index += 1
        usleep(105_000)
    }
    exit(0)
}
// `--windows <pid>` prints how many windows the process has on screen; `--window-sizes <pid>` lists their sizes.
if ["--windows", "--window-sizes"].contains(CommandLine.arguments.dropFirst().first ?? ""), let pid = Int32(CommandLine.arguments.last ?? "") {
    let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    let mine = info.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid }
    if CommandLine.arguments[1] == "--windows" {
        print(mine.count)
    } else {
        let sizes = mine.compactMap { $0[kCGWindowBounds as String] as? [String: Double] }.map { [$0["Width"] ?? 0, $0["Height"] ?? 0] }
        print(String(decoding: try JSONSerialization.data(withJSONObject: sizes), as: UTF8.self))
    }
    exit(0)
}

// `--settings-pane` prints the identifier of the pane System Settings shows (empty when it is not running or none is
// selected); `--settings-pane <identifier>` selects that pane's sidebar entry again, as a person clicking it would, and
// exits 1 when there is none. The test puts System Settings back the way it found it with these.
if CommandLine.arguments.dropFirst().first == "--settings-pane" {
    func value(_ element: AXUIElement, _ name: String) -> AnyObject? {
        var out: AnyObject?
        return AXUIElementCopyAttributeValue(element, name as CFString, &out) == .success ? out : nil
    }
    func children(_ element: AXUIElement) -> [AXUIElement] { (value(element, kAXChildrenAttribute) as? [AXUIElement]) ?? [] }
    func pane(of row: AXUIElement) -> String? {
        var queue = [row]
        while !queue.isEmpty {
            let item = queue.removeFirst()
            if let identifier = value(item, kAXIdentifierAttribute) as? String, identifier.contains(".") {
                return String(identifier.prefix { $0 != "*" && $0 != ":" })
            }
            queue += children(item)
        }
        return nil
    }
    let wanted = CommandLine.arguments.dropFirst(2).first
    guard let settings = NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.systempreferences").first else {
        print("")
        exit(wanted == nil ? 0 : 1)
    }
    var queue = (value(AXUIElementCreateApplication(settings.processIdentifier), kAXWindowsAttribute) as? [AXUIElement]) ?? []
    var index = 0
    while index < queue.count, index < 400 {
        let next = queue[index]
        index += 1
        if (value(next, kAXRoleAttribute) as? String) == "AXOutline" {
            guard let wanted else {
                print(((value(next, kAXSelectedRowsAttribute) as? [AXUIElement]) ?? []).first.flatMap(pane) ?? "")
                exit(0)
            }
            for row in (value(next, kAXRowsAttribute) as? [AXUIElement]) ?? [] where pane(of: row) == wanted {
                exit(AXUIElementSetAttributeValue(next, kAXSelectedRowsAttribute as CFString, [row] as CFArray) == .success ? 0 : 1)
            }
            exit(1)
        }
        queue += children(next)
    }
    print("")
    exit(wanted == nil ? 0 : 1)
}

// `--bar <pid>` prints {"texts": [...], "buttons": [...]} for the helper's bar; `--bar <pid> press <title>` presses
// the button with that title and exits 1 when there is none. A press that takes the bar down (Stop) can report an
// error although it happened, so the test judges by what followed.
if CommandLine.arguments.dropFirst().first == "--bar", CommandLine.arguments.count >= 3, let pid = Int32(CommandLine.arguments[2]) {
    func value(_ element: AXUIElement, _ name: String) -> AnyObject? {
        var out: AnyObject?
        return AXUIElementCopyAttributeValue(element, name as CFString, &out) == .success ? out : nil
    }
    var texts: [String] = []
    var buttons: [(String, AXUIElement)] = []
    func walk(_ element: AXUIElement, _ depth: Int) {
        guard depth < 12 else { return }
        let role = value(element, kAXRoleAttribute) as? String ?? ""
        if role == kAXStaticTextRole, let text = value(element, kAXValueAttribute) as? String, !text.isEmpty { texts.append(text) }
        if role == kAXButtonRole { buttons.append(((value(element, kAXTitleAttribute) as? String) ?? "", element)) }
        for child in (value(element, kAXChildrenAttribute) as? [AXUIElement]) ?? [] { walk(child, depth + 1) }
    }
    for window in (value(AXUIElementCreateApplication(pid), kAXWindowsAttribute) as? [AXUIElement]) ?? [] { walk(window, 0) }
    let arguments = Array(CommandLine.arguments.dropFirst(3))
    if arguments.first == "press", arguments.count == 2 {
        guard let button = buttons.first(where: { $0.0 == arguments[1] }) else { exit(1) }
        print(AXUIElementPerformAction(button.1, kAXPressAction as CFString).rawValue)
        exit(0)
    }
    let data = try JSONSerialization.data(withJSONObject: ["texts": texts, "buttons": buttons.map(\.0)])
    print(String(decoding: data, as: UTF8.self))
    exit(0)
}

let person = CommandLine.arguments.dropFirst().first == "--person"
// Opened by an app launcher, which passes no arguments, it writes its state beside its own bundle.
let statePath = (person ? CommandLine.arguments.dropFirst(2).first : CommandLine.arguments.dropFirst().first)
    ?? Bundle.main.bundleURL.deletingLastPathComponent().appendingPathComponent(Bundle.main.bundleURL.deletingPathExtension().lastPathComponent + "-state.json").path

final class TopDownStack: NSStackView {
    override var isFlipped: Bool { true }
}

/// A view without controls that counts the clicks it gets: only a pixel click can reach it.
final class Canvas: NSView {
    var clicks = 0
    var last: NSPoint = .zero
    var changed: () -> Void = {}

    override func draw(_ dirtyRect: NSRect) {
        NSColor.systemTeal.withAlphaComponent(0.25).setFill()
        bounds.fill()
    }

    override func mouseDown(with event: NSEvent) {
        clicks += 1
        last = convert(event.locationInWindow, from: nil)
        changed()
    }
}

final class Lab: NSObject, NSApplicationDelegate, NSTextFieldDelegate, NSTextViewDelegate {
    var window: NSWindow!
    var count = 0
    var submitted = false
    var forwardPresses = 0
    var keys: [String] = []
    var pointer: [String] = []
    let counter = NSTextField(labelWithString: "Count: 0")
    let name = NSTextField(string: "")
    let password = NSSecureTextField(string: "")
    let slider = NSSlider(value: 10, minValue: 0, maxValue: 100, target: nil, action: nil)
    let scroll = NSScrollView()
    let canvas = Canvas()
    let canvasLabel = NSTextField(labelWithString: "Canvas clicks: 0")
    let notesScroll = NSTextView.scrollableTextView()
    var notes: NSTextView { notesScroll.documentView as! NSTextView }

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildMenu()
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 640), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Haarness Lab"
        window.isRestorable = false
        window.center()
        let count = NSButton(title: "Count", target: self, action: #selector(countPressed))
        let submit = NSButton(title: "Submit", target: self, action: #selector(submitPressed))
        submit.keyEquivalent = "\r"
        let forward = NSButton(title: "Come forward", target: self, action: #selector(forwardPressed))
        name.placeholderString = "Name"
        name.setAccessibilityLabel("Name")
        name.delegate = self
        password.placeholderString = "Password"
        password.setAccessibilityLabel("Password")
        slider.target = self
        slider.action = #selector(slid)
        slider.setAccessibilityLabel("Level")
        // Top-down, like a real list: it opens at Row 1 and scrolling down moves toward Row 60.
        let list = TopDownStack(views: (1...60).map { NSTextField(labelWithString: "Row \($0)") })
        list.orientation = .vertical
        list.alignment = .leading
        list.frame = NSRect(x: 0, y: 0, width: 200, height: 1200)
        scroll.documentView = list
        scroll.hasVerticalScroller = true
        scroll.setAccessibilityLabel("Rows")
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(scrolled), name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        canvas.setAccessibilityElement(true)
        canvas.setAccessibilityRole(.group)
        canvas.setAccessibilityLabel("Canvas")
        canvas.changed = { [weak self] in
            guard let self else { return }
            self.canvasLabel.stringValue = "Canvas clicks: \(self.canvas.clicks)"
            self.save()
        }
        notes.delegate = self
        notes.setAccessibilityLabel("Notes")
        notesScroll.setAccessibilityLabel("Notes area")
        let row = NSStackView(views: [count, forward])
        row.orientation = .horizontal
        let stack = NSStackView(views: [counter, row, name, password, slider, scroll, canvasLabel, canvas, notesScroll, submit])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        for view in [name, password, slider, scroll, canvas, notesScroll] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            view.widthAnchor.constraint(equalToConstant: 300).isActive = true
        }
        scroll.heightAnchor.constraint(equalToConstant: 120).isActive = true
        canvas.heightAnchor.constraint(equalToConstant: 60).isActive = true
        notesScroll.heightAnchor.constraint(equalToConstant: 70).isActive = true
        window.contentView = stack
        NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            self?.keys.append(event.charactersIgnoringModifiers ?? "")
            self?.save()
            return event
        }
        // Pointer events as the app receives them, to tell an event that never arrived from one the app ignored.
        NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .leftMouseUp, .rightMouseDown, .scrollWheel]) { [weak self] event in
            // Records the window server sends to change key status arrive as presses without a usable location.
            let at = event.locationInWindow
            let spot = at.x.isFinite && at.y.isFinite ? "\(Int(at.x)),\(Int(at.y))" : "-"
            self?.pointer.append("\(event.type.rawValue) w\(event.windowNumber) \(spot)")
            self?.pointer = Array(self?.pointer.suffix(20) ?? [])
            self?.save()
            return event
        }
        for name in [NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification] {
            NotificationCenter.default.addObserver(self, selector: #selector(changed), name: name, object: nil)
        }
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        save()
    }

    func buildMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu(title: "Haarness Lab")
        appItem.submenu = appMenu
        main.addItem(appItem)
        let labItem = NSMenuItem()
        let labMenu = NSMenu(title: "Lab")
        labMenu.addItem(NSMenuItem(title: "Bump", action: #selector(bumpPressed), keyEquivalent: "b"))
        labItem.submenu = labMenu
        main.addItem(labItem)
        NSApp.mainMenu = main
    }

    @objc func countPressed() {
        count += 1
        counter.stringValue = "Count: \(count)"
        save()
    }

    @objc func bumpPressed() {
        count += 10
        counter.stringValue = "Count: \(count)"
        save()
    }

    @objc func forwardPressed() {
        forwardPresses += 1
        save()
        NSApp.activate(ignoringOtherApps: true)
    }

    @objc func submitPressed() {
        submitted = true
        save()
    }

    @objc func slid() { save() }
    @objc func scrolled() { save() }
    @objc func changed() { save() }
    func controlTextDidChange(_ notification: Notification) { save() }
    func textDidChange(_ notification: Notification) { save() }

    func save() {
        let state: [String: Any] = [
            "pid": Int(ProcessInfo.processInfo.processIdentifier),
            "count": count,
            "name": name.stringValue,
            "passwordLength": password.stringValue.count,
            "slider": slider.doubleValue,
            "scrollY": scroll.contentView.bounds.origin.y,
            "submitted": submitted,
            "keys": Array(keys.suffix(50)),
            "frame": ["x": window.frame.origin.x, "y": window.frame.origin.y],
            "canvasClicks": canvas.clicks,
            "canvasAt": ["x": canvas.last.x, "y": canvas.last.y],
            "notes": notes.string,
            "forwardPresses": forwardPresses,
            "active": NSApp.isActive,
            "pointer": pointer,
            "keyWindow": window.isKeyWindow,
        ]
        if let data = try? JSONSerialization.data(withJSONObject: state) {
            try? data.write(to: URL(fileURLWithPath: statePath), options: .atomic)
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

/// The person's own app in the background tests.
final class Person: NSObject, NSApplicationDelegate, NSTextViewDelegate {
    var window: NSWindow!
    let editor = NSTextView.scrollableTextView()
    let secret = NSSecureTextField(string: "")
    var text: NSTextView { editor.documentView as! NSTextView }
    var resigned = 0
    var keyLost = 0
    var pointer: [Double] = []
    var pointerMoved = false
    var frontmost = ""
    var command = ""
    var escapes = 0

    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 780), styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "Haarness Person"
        window.isRestorable = false
        window.center()
        text.delegate = self
        text.font = .systemFont(ofSize: 15)
        secret.placeholderString = "Person's password"
        let stack = NSStackView(views: [secret, editor])
        stack.orientation = .vertical
        stack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
        secret.translatesAutoresizingMaskIntoConstraints = false
        secret.widthAnchor.constraint(equalToConstant: 300).isActive = true
        window.contentView = stack
        NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            if event.keyCode == 0x35 { self?.escapes += 1 }
            return event
        }
        NotificationCenter.default.addObserver(self, selector: #selector(resignedActive), name: NSApplication.didResignActiveNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(lostKey), name: NSWindow.didResignKeyNotification, object: window)
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(text)
        NSApp.activate(ignoringOtherApps: true)
        let start = NSEvent.mouseLocation
        pointer = [start.x, start.y]
        Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in self?.tick() }
        save()
    }

    func tick() {
        let now = NSEvent.mouseLocation
        if abs(now.x - pointer[0]) > 0.5 || abs(now.y - pointer[1]) > 0.5 { pointerMoved = true }
        frontmost = NSWorkspace.shared.frontmostApplication?.bundleIdentifier ?? ""
        let path = statePath + ".cmd"
        if let line = try? String(contentsOfFile: path, encoding: .utf8), !line.isEmpty, line != command {
            command = line
            switch line.trimmingCharacters(in: .whitespacesAndNewlines) {
            case "secure": window.makeFirstResponder(secret)
            case "text": window.makeFirstResponder(text)
            case "front": NSApp.activate(ignoringOtherApps: true)
            default: break
            }
        }
        save()
    }

    @objc func resignedActive() {
        resigned += 1
        save()
    }

    @objc func lostKey() {
        keyLost += 1
        save()
    }

    func textDidChange(_ notification: Notification) { save() }

    func save() {
        let state: [String: Any] = [
            "pid": Int(ProcessInfo.processInfo.processIdentifier),
            "text": text.string,
            "active": NSApp.isActive,
            "key": window.isKeyWindow,
            "resigned": resigned,
            "keyLost": keyLost,
            "pointerMoved": pointerMoved,
            "frontmost": frontmost,
            "secureInput": IsSecureEventInputEnabled(),
            "escapes": escapes,
        ]
        if let data = try? JSONSerialization.data(withJSONObject: state) {
            try? data.write(to: URL(fileURLWithPath: statePath), options: .atomic)
        }
    }
}

// Never offer to restore windows after a run that was cut short: that alert would stop the next run's launch.
UserDefaults.standard.register(defaults: ["ApplePersistenceIgnoreState": true, "NSQuitAlwaysKeepsWindows": false])
let app = NSApplication.shared
let lab = Lab()
let personApp = Person()
app.delegate = person ? personApp : lab
app.setActivationPolicy(.regular)
app.run()
