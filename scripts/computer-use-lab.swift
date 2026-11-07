// A small Mac app for the live computer use test (scripts/test-computer-use-live.py): a counter button, a name field,
// a password field, a slider, a scrolling list and a Submit button that Return presses. After every change it writes
// its state as JSON to the file named by its first argument, so the test checks what the agent's actions did.
import AppKit
import ApplicationServices

// Errands for the test that need no window: press Escape as the person would (an event the helper did not post),
// list the on-screen windows of a process (the helper's bar), and read the bar's words or press one of its buttons
// through Accessibility, as a person clicking it would.
if CommandLine.arguments.dropFirst().first == "--press-escape" {
    let source = CGEventSource(stateID: .hidSystemState)
    CGEvent(keyboardEventSource: source, virtualKey: 0x35, keyDown: true)?.post(tap: .cghidEventTap)
    usleep(40_000)
    CGEvent(keyboardEventSource: source, virtualKey: 0x35, keyDown: false)?.post(tap: .cghidEventTap)
    // Leaving at once can lose the press on a busy Mac: neither the front app nor the helper ever sees it.
    usleep(300_000)
    exit(0)
}
if CommandLine.arguments.dropFirst().first == "--windows", let pid = Int32(CommandLine.arguments.last ?? "") {
    let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    let mine = info.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid }
    print(mine.count)
    exit(0)
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

let statePath = CommandLine.arguments.dropFirst().first ?? "/tmp/haarness-lab-state.json"

final class TopDownStack: NSStackView {
    override var isFlipped: Bool { true }
}

final class Lab: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    var window: NSWindow!
    var count = 0
    var submitted = false
    var keys: [String] = []
    let counter = NSTextField(labelWithString: "Count: 0")
    let name = NSTextField(string: "")
    let password = NSSecureTextField(string: "")
    let slider = NSSlider(value: 10, minValue: 0, maxValue: 100, target: nil, action: nil)
    let scroll = NSScrollView()

    func applicationDidFinishLaunching(_ notification: Notification) {
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 520, height: 420), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Haarness Lab"
        window.center()
        let count = NSButton(title: "Count", target: self, action: #selector(countPressed))
        let submit = NSButton(title: "Submit", target: self, action: #selector(submitPressed))
        submit.keyEquivalent = "\r"
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
        let stack = NSStackView(views: [counter, count, name, password, slider, scroll, submit])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        for view in [name, password, slider, scroll] as [NSView] {
            view.translatesAutoresizingMaskIntoConstraints = false
            view.widthAnchor.constraint(equalToConstant: 300).isActive = true
        }
        scroll.heightAnchor.constraint(equalToConstant: 120).isActive = true
        window.contentView = stack
        NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            self?.keys.append(event.charactersIgnoringModifiers ?? "")
            self?.save()
            return event
        }
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        save()
    }

    @objc func countPressed() {
        count += 1
        counter.stringValue = "Count: \(count)"
        save()
    }

    @objc func submitPressed() {
        submitted = true
        save()
    }

    @objc func slid() { save() }
    @objc func scrolled() { save() }
    func controlTextDidChange(_ notification: Notification) { save() }

    func save() {
        let state: [String: Any] = [
            "count": count,
            "name": name.stringValue,
            "passwordLength": password.stringValue.count,
            "slider": slider.doubleValue,
            "scrollY": scroll.contentView.bounds.origin.y,
            "submitted": submitted,
            "keys": Array(keys.suffix(50)),
            "frame": ["x": window.frame.origin.x, "y": window.frame.origin.y],
        ]
        if let data = try? JSONSerialization.data(withJSONObject: state) {
            try? data.write(to: URL(fileURLWithPath: statePath), options: .atomic)
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

let app = NSApplication.shared
let lab = Lab()
app.delegate = lab
app.setActivationPolicy(.regular)
app.run()
