import AppKit
import IOKit.pwr_mgt

/// Holds off the display's idle dimming and sleep while an agent works on the screen, since the person may not touch
/// the Mac meanwhile. It is let go at the end of the turn, after five minutes without a request, and as soon
/// as the screen locks; it is only taken while the display is awake and unlocked, so it never wakes a display or keeps
/// a lock screen lit.
final class KeepAwake {
    static let shared = KeepAwake()

    /// The name `pmset -g assertions` shows.
    static let name = "Haarness is working on the screen"
    static let idle: TimeInterval = 5 * 60

    private let lock = NSLock()
    private var assertion: IOPMAssertionID?
    private var expiry: DispatchWorkItem?
    private var watching = false

    /// Takes or extends the hold when the screen is in use and awake.
    func hold() {
        let session = (CGSessionCopyCurrentDictionary() as? [String: Any]) ?? [:]
        let locked = (session["CGSSessionScreenIsLocked"] as? Bool) ?? false
        guard !locked, CGDisplayIsAsleep(CGMainDisplayID()) == 0 else { return }
        lock.lock()
        defer { lock.unlock() }
        if !watching {
            watching = true
            // Distributed notifications arrive through the main run loop.
            DispatchQueue.main.async {
                DistributedNotificationCenter.default().addObserver(forName: NSNotification.Name("com.apple.screenIsLocked"), object: nil, queue: nil) { _ in
                    KeepAwake.shared.release()
                }
            }
        }
        if assertion == nil {
            var id: IOPMAssertionID = 0
            if IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleDisplaySleep as CFString, IOPMAssertionLevel(kIOPMAssertionLevelOn), Self.name as CFString, &id) == kIOReturnSuccess {
                assertion = id
            }
        }
        expiry?.cancel()
        let item = DispatchWorkItem { [weak self] in self?.release() }
        expiry = item
        DispatchQueue.global().asyncAfter(deadline: .now() + Self.idle, execute: item)
    }

    func release() {
        lock.lock()
        defer { lock.unlock() }
        expiry?.cancel()
        expiry = nil
        if let assertion { IOPMAssertionRelease(assertion) }
        assertion = nil
    }
}
