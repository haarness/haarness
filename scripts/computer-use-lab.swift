// A small Mac app for the live computer use test (scripts/test-computer-use-live.py): a counter button, a name field,
// a password field, a slider, a scrolling list and a Submit button that Return presses. After every change it writes
// its state as JSON to the file named by its first argument, so the test checks what the agent's actions did.
import AppKit

// Two errands for the test that need no window: press Escape as the person would (an event the helper did not
// post), and list the on-screen windows of a process (the helper's bar).
if CommandLine.arguments.dropFirst().first == "--press-escape" {
    let source = CGEventSource(stateID: .hidSystemState)
    CGEvent(keyboardEventSource: source, virtualKey: 0x35, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: source, virtualKey: 0x35, keyDown: false)?.post(tap: .cghidEventTap)
    exit(0)
}
if CommandLine.arguments.dropFirst().first == "--windows", let pid = Int32(CommandLine.arguments.last ?? "") {
    let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    let mine = info.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid }
    print(mine.count)
    exit(0)
}

let statePath = CommandLine.arguments.dropFirst().first ?? "/tmp/haarness-lab-state.json"

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
        let list = NSStackView(views: (1...60).map { NSTextField(labelWithString: "Row \($0)") })
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
