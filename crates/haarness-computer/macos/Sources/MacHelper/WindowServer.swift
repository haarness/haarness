// Private window-server calls for working in an app behind the person's window: posting events to one process, making
// one of its windows key without activating it, and asking which process is in front. Every call is looked up when
// first used, so a macOS release that lacks one makes the helper fall back to acting in the foreground instead of
// failing to start.
//
// Derived from the macOS input code of Cua Driver, https://github.com/trycua/cua (libs/cua-driver): the call names and
// signatures, the 248-byte event records and the event fields stamped on posted events follow it. Its sequence of
// focus records that gives a window key status without raising it comes from yabai,
// https://github.com/koekeishiya/yabai. Both are used under the MIT License:
//
//   Copyright (c) 2025-2026 Cua AI, Inc.
//   Copyright (c) 2019 Åsmund Vikane
//
//   Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
//   documentation files (the "Software"), to deal in the Software without restriction, including without limitation
//   the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to
//   permit persons to whom the Software is furnished to do so, subject to the following conditions:
//
//   The above copyright notice and this permission notice shall be included in all copies or substantial portions of
//   the Software.
//
//   THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE
//   WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS
//   OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
//   OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//
// The same notices are in ../../THIRD-PARTY-NOTICES.md.
import AppKit
import ApplicationServices
import Darwin
import MacKit

enum WindowServer {
    private typealias PostToPid = @convention(c) (pid_t, CGEvent) -> Void
    private typealias SetIntegerField = @convention(c) (CGEvent, UInt32, Int64) -> Void
    private typealias SetWindowLocation = @convention(c) (CGEvent, CGPoint) -> Void
    private typealias SetAuthentication = @convention(c) (CGEvent, AnyObject) -> Void
    private typealias PostRecord = @convention(c) (UnsafeRawPointer, UnsafeRawPointer) -> Int32
    private typealias GetFrontProcess = @convention(c) (UnsafeMutableRawPointer) -> Int32
    private typealias ProcessPID = @convention(c) (UnsafeRawPointer, UnsafeMutablePointer<pid_t>) -> Int32
    private typealias ProcessForPID = @convention(c) (pid_t, UnsafeMutableRawPointer) -> Int32
    private typealias MainConnection = @convention(c) () -> UInt32
    private typealias WindowOwner = @convention(c) (UInt32, UInt32, UnsafeMutablePointer<UInt32>) -> Int32
    private typealias ConnectionPSN = @convention(c) (UInt32, UnsafeMutableRawPointer) -> Int32
    private typealias AXWindowNumber = @convention(c) (AXUIElement, UnsafeMutablePointer<CGWindowID>) -> AXError
    private typealias AuthenticationFactory = @convention(c) (AnyClass, Selector, UnsafeMutableRawPointer, Int32, UInt32) -> Unmanaged<AnyObject>?

    /// Loads SkyLight once so the lookups below find its symbols.
    private static let loaded: Bool = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY | RTLD_GLOBAL) != nil

    private static func symbol<T>(_ names: String..., as type: T.Type) -> T? {
        _ = loaded
        // RTLD_DEFAULT: search every image loaded into the process.
        let everywhere = UnsafeMutableRawPointer(bitPattern: -2)
        for name in names {
            if let pointer = dlsym(everywhere, name) { return unsafeBitCast(pointer, to: type) }
        }
        return nil
    }

    private static let postToPid = symbol("SLEventPostToPid", as: PostToPid.self)
    private static let setIntegerField = symbol("SLEventSetIntegerValueField", as: SetIntegerField.self)
    private static let setWindowLocation = symbol("CGEventSetWindowLocation", as: SetWindowLocation.self)
    private static let setAuthentication = symbol("SLEventSetAuthenticationMessage", as: SetAuthentication.self)
    private static let postRecord = symbol("SLPSPostEventRecordTo", as: PostRecord.self)
    private static let getFrontProcess = symbol("_SLPSGetFrontProcess", as: GetFrontProcess.self)
    private static let processPID = symbol("GetProcessPID", as: ProcessPID.self)
    private static let processForPID = symbol("GetProcessForPID", as: ProcessForPID.self)
    private static let mainConnection = symbol("SLSMainConnectionID", "CGSMainConnectionID", as: MainConnection.self)
    private static let windowOwner = symbol("SLSGetWindowOwner", as: WindowOwner.self)
    private static let connectionPSN = symbol("SLSGetConnectionPSN", as: ConnectionPSN.self)
    private static let axWindowNumber = symbol("_AXUIElementGetWindow", as: AXWindowNumber.self)

    /// Whether everything background delivery needs was found: posting to a process, stamping its fields, the window
    /// records, a way to name a process to them, and Accessibility's window numbers.
    static var available: Bool {
        postToPid != nil && setIntegerField != nil && postRecord != nil && axWindowNumber != nil
            && ((mainConnection != nil && windowOwner != nil && connectionPSN != nil) || processForPID != nil)
    }

    // MARK: Events

    /// Event fields stamped on background pointer events, by number: several have no public name.
    enum Field {
        static let mouseNumber: UInt32 = 0
        static let clickState: UInt32 = 1
        static let buttonNumber: UInt32 = 3
        static let subtype: UInt32 = 7
        static let targetPid: UInt32 = 40
        static let windowNumber: UInt32 = 51
        static let gesture: UInt32 = 58
        static let windowUnderPointer: UInt32 = 91
        static let windowThatCanHandle: UInt32 = 92
    }

    static func stamp(_ event: CGEvent, _ field: UInt32, _ value: Int64) {
        if let setIntegerField {
            setIntegerField(event, field, value)
        } else if let known = CGEventField(rawValue: field) {
            event.setIntegerValueField(known, value: value)
        }
    }

    /// The point a pointer event addressed to a window carries for that window (see `windowLocationIsLocal`).
    static func stampLocation(_ event: CGEvent, _ point: CGPoint) {
        setWindowLocation?(event, point)
    }

    /// Hands `event` to `pid`'s process, tagged as the helper's own. The window server's own route is used where it
    /// exists: unlike the public one it counts as input activity, which Chromium and Catalyst apps require. Keyboard
    /// events also carry the authentication message Chromium checks on macOS 14 and later.
    static func post(_ event: CGEvent, to pid: pid_t, keyboard: Bool) {
        event.setIntegerValueField(.eventSourceUserData, value: syntheticTag)
        if keyboard { authenticate(event, pid: pid) }
        if let postToPid {
            postToPid(pid, event)
        } else {
            event.postToPid(pid)
        }
    }

    private static func authenticate(_ event: CGEvent, pid: pid_t) {
        guard #available(macOS 14.0, *), let setAuthentication,
              let factory = NSClassFromString("SLSEventAuthenticationMessage") else { return }
        let selector = NSSelectorFromString("messageWithEventRecord:pid:version:")
        // The class exists on macOS 14, the method only from 15.
        guard let method = class_getClassMethod(factory, selector), let record = eventRecord(event) else { return }
        let make = unsafeBitCast(method_getImplementation(method), to: AuthenticationFactory.self)
        guard let message = make(factory, selector, record, pid, 0)?.takeUnretainedValue() else { return }
        setAuthentication(event, message)
    }

    /// The window server's record inside a CGEvent object, which the authentication message is built from.
    private static func eventRecord(_ event: CGEvent) -> UnsafeMutableRawPointer? {
        let base = Unmanaged.passUnretained(event).toOpaque()
        for offset in [24, 32, 16] {
            if let record = base.load(fromByteOffset: offset, as: UnsafeMutableRawPointer?.self) { return record }
        }
        return nil
    }

    // MARK: Processes and windows

    /// The window server's number for an Accessibility window.
    static func windowNumber(_ window: AXUIElement) -> CGWindowID? {
        guard let axWindowNumber else { return nil }
        var number: CGWindowID = 0
        return axWindowNumber(window, &number) == .success && number != 0 ? number : nil
    }

    /// The process serial number the window records are addressed to, from the window's owner or else from the pid.
    private static func serial(pid: pid_t, window: CGWindowID) -> [UInt8]? {
        var psn = [UInt8](repeating: 0, count: 8)
        if let mainConnection, let windowOwner, let connectionPSN {
            var owner: UInt32 = 0
            if windowOwner(mainConnection(), window, &owner) == 0, owner != 0,
               psn.withUnsafeMutableBytes({ connectionPSN(owner, $0.baseAddress!) }) == 0 {
                return psn
            }
        }
        if let processForPID, psn.withUnsafeMutableBytes({ processForPID(pid, $0.baseAddress!) }) == 0 { return psn }
        return nil
    }

    /// The pid of the process the window server has in front. It sees an app that activated itself at once, before
    /// AppKit's notice of it reaches this process.
    static func frontPID() -> pid_t? {
        if let getFrontProcess, let processPID {
            var psn = [UInt8](repeating: 0, count: 8)
            var pid: pid_t = 0
            if psn.withUnsafeMutableBytes({ getFrontProcess($0.baseAddress!) }) == 0, psn != [UInt8](repeating: 0, count: 8),
               psn.withUnsafeBytes({ processPID($0.baseAddress!, &pid) }) == 0, pid > 0 {
                return pid
            }
        }
        return NSWorkspace.shared.frontmostApplication?.processIdentifier
    }

    /// A 248-byte window record. `kind` 0x0D is a focus change, with byte 0x8A saying 1 (focus) or 2 (defocus); kinds 1
    /// and 2 make the window key.
    private static func record(kind: UInt8, window: CGWindowID) -> [UInt8] {
        var bytes = [UInt8](repeating: 0, count: 0xF8)
        bytes[0x04] = 0xF8
        bytes[0x08] = kind
        withUnsafeBytes(of: window.littleEndian) { number in
            for (index, byte) in number.enumerated() { bytes[0x3C + index] = byte }
        }
        if kind != 0x0D {
            bytes[0x3A] = 0x10
            for index in 0x20..<0x30 { bytes[index] = 0xFF }
        }
        return bytes
    }

    private static func send(_ bytes: [UInt8], to psn: [UInt8]) -> Bool {
        guard let postRecord else { return false }
        return psn.withUnsafeBytes { serial in bytes.withUnsafeBytes { postRecord(serial.baseAddress!, $0.baseAddress!) } } == 0
    }

    /// Lets `window` of `pid` take pointer input while the app stays behind: a focus record to that app alone. No
    /// defocus goes to the app in front, whose key window would otherwise stop taking the person's typing until they
    /// click it.
    @discardableResult
    static func focusWithoutRaise(pid: pid_t, window: CGWindowID) -> Bool {
        guard let psn = serial(pid: pid, window: window) else { return false }
        var bytes = record(kind: 0x0D, window: window)
        bytes[0x8A] = 0x01
        let sent = send(bytes, to: psn)
        Thread.sleep(forTimeInterval: 0.05)
        return sent
    }

    /// Moves key status inside `pid` from its window `current` to `window` without activating the app, raising a window
    /// or moving the pointer: every record goes to that app. Whether the app took it is for the caller to check.
    static func makeKey(pid: pid_t, window: CGWindowID, current: CGWindowID?) -> Bool {
        guard let psn = serial(pid: pid, window: window) else { return false }
        var sent = true
        // The app can drop a record that arrives while it still handles the one before.
        let settle = 0.12
        if let current, current != window {
            var away = record(kind: 0x0D, window: current)
            away[0x8A] = 0x02
            sent = send(away, to: psn) && sent
            Thread.sleep(forTimeInterval: settle)
        }
        var toward = record(kind: 0x0D, window: window)
        toward[0x8A] = 0x01
        sent = send(toward, to: psn) && sent
        Thread.sleep(forTimeInterval: settle)
        for kind: UInt8 in [0x01, 0x02] {
            sent = send(record(kind: kind, window: window), to: psn) && sent
        }
        return sent
    }
}
