// The order, fields and timing of the pointer events below follow the background click of Cua Driver
// (https://github.com/trycua/cua, libs/cua-driver), Copyright (c) 2025-2026 Cua AI, Inc., used under the MIT License;
// its text is in ../../THIRD-PARTY-NOTICES.md.
import Foundation

/// A point in screen points with a top-left origin.
public struct Point: Equatable {
    public var x: Double
    public var y: Double

    public init(_ x: Double, _ y: Double) {
        self.x = x
        self.y = y
    }
}

/// One pointer event of a click posted to an app's process rather than through the system's pointer.
public struct PointerStep: Equatable {
    public enum Kind: Equatable {
        case move
        case down
        case up
    }

    public var kind: Kind
    /// Where the event says it happens, in screen points.
    public var screen: Point
    /// The location stamped on the event for the window it is addressed to.
    public var stamped: Point
    /// The press count the app reads (1 for the first press of a double click, 2 for the second).
    public var clickState: Int
    /// A marker of the step's part in the gesture, carried in the event-number field: 2 for the move, 1 and 2 for the
    /// press outside the window, 3 for the presses on the target.
    public var phase: Int
    /// 0 left, 1 right, 2 middle.
    public var button: Int
    /// Seconds to wait after posting this step.
    public var pause: Double
}

/// Where a click outside every window lands: Chromium and Electron only treat a background press as the person's
/// own once a press has preceded it, and one here touches nothing on the page.
public let outsidePoint = Point(-1, -1)

/// Whether the event's window location is read as a point inside the window (macOS 27 and later) rather than a screen
/// point the window server converts itself.
public func windowLocationIsLocal(macOSMajor: Int) -> Bool {
    macOSMajor >= 27
}

/// The sequence posted for a click at `screen` on an app in the background: the pointer arrives first, so the window
/// tracks it and controls that appear on hover exist; Chromium and Electron apps (`outsidePress`) get a press outside
/// every window; then `count` presses (1 to 3), the press count rising with each. On macOS 27 and later
/// (`windowLocal`) the stamped location is relative to the window's top-left corner `origin`.
public func backgroundClickPlan(at screen: Point, origin: Point, button: Int, count: Int, outsidePress: Bool, windowLocal: Bool) -> [PointerStep] {
    let stamped = windowLocal ? Point(screen.x - origin.x, screen.y - origin.y) : screen
    var steps = [PointerStep(kind: .move, screen: screen, stamped: stamped, clickState: 0, phase: 2, button: 0, pause: 0.015)]
    if outsidePress && button == 0 {
        steps.append(PointerStep(kind: .down, screen: outsidePoint, stamped: outsidePoint, clickState: 1, phase: 1, button: 0, pause: 0.001))
        steps.append(PointerStep(kind: .up, screen: outsidePoint, stamped: outsidePoint, clickState: 1, phase: 2, button: 0, pause: 0.1))
    }
    let presses = max(1, min(3, count))
    for press in 1...presses {
        steps.append(PointerStep(kind: .down, screen: screen, stamped: stamped, clickState: press, phase: 3, button: button, pause: 0.03))
        steps.append(PointerStep(kind: .up, screen: screen, stamped: stamped, clickState: press, phase: 3, button: button, pause: press < presses ? 0.08 : 0))
    }
    return steps
}
