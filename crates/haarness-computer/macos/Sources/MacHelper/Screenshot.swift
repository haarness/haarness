import AppKit
import CoreGraphics
import ImageIO
import MacKit
import ScreenCaptureKit
import UniformTypeIdentifiers

/// Pictures of the main display at the size the model works with, as JPEG.
enum Screenshot {
    static var display: CGRect { CGDisplayBounds(CGMainDisplayID()) }

    /// Renders the main display into an image of `size`. When `only` is set the listed apps alone are drawn, where they
    /// really are, over an empty background, so coordinates still match the display; otherwise all but the `hidden`
    /// apps appear. The helper's bar and ring never do.
    static func capture(size: (width: Int, height: Int), hidden: [String], only: [String]?) throws -> CGImage {
        let picture: CGImage
        if #available(macOS 14.0, *) {
            picture = try withScreenCaptureKit(size: size, region: nil, hidden: hidden, only: only)
        } else {
            picture = try withWindowList(hidden: hidden, only: only)
        }
        return try resize(picture, to: size)
    }

    /// A close-up of `region` (screen points) at the display's full resolution, no more than `longest` pixels on its
    /// long side, drawn like `capture`.
    static func closeUp(_ region: Box, longest: Int, hidden: [String], only: [String]?) throws -> CGImage {
        let scale = Double(NSScreen.screens.first?.backingScaleFactor ?? 2)
        let shrink = min(1, Double(longest) / (max(region.width, region.height) * scale))
        let size = (width: max(1, Int((region.width * scale * shrink).rounded())), height: max(1, Int((region.height * scale * shrink).rounded())))
        let picture: CGImage
        if #available(macOS 14.0, *) {
            picture = try withScreenCaptureKit(size: size, region: region, hidden: hidden, only: only)
        } else {
            picture = try withWindowList(hidden: hidden, only: only, bounds: CGRect(x: region.x, y: region.y, width: region.width, height: region.height))
        }
        return try resize(picture, to: size)
    }

    @available(macOS 14.0, *)
    private static func withScreenCaptureKit(size: (width: Int, height: Int), region: Box?, hidden: [String], only: [String]?) throws -> CGImage {
        let signal = DispatchSemaphore(value: 0)
        var shareable: SCShareableContent?
        var problem: Error?
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, error in
            shareable = content
            problem = error
            signal.signal()
        }
        // The first answer in a fresh helper can take a while on a busy Mac.
        if signal.wait(timeout: .now() + 20) == .timedOut { throw HelperFailure("failed", "ScreenCaptureKit gave no answer.") }
        guard let shareable else { throw refusal(problem) }
        let main = CGMainDisplayID()
        guard let screen = shareable.displays.first(where: { $0.displayID == main }) ?? shareable.displays.first else {
            throw HelperFailure("failed", "No display is available to capture.")
        }
        let me = ProcessInfo.processInfo.processIdentifier
        // A filter that names apps to leave out keeps the whole display's frame. Naming the apps to include instead
        // would crop the picture to their windows and move every coordinate.
        let leftOut = shareable.applications.filter { app in
            if app.processID == me || matches(app.bundleIdentifier, hidden) { return true }
            guard let only else { return false }
            return !matches(app.bundleIdentifier, only)
        }
        let filter = SCContentFilter(display: screen, excludingApplications: leftOut, exceptingWindows: [])
        let settings = SCStreamConfiguration()
        settings.width = size.width
        settings.height = size.height
        if let region { settings.sourceRect = CGRect(x: region.x, y: region.y, width: region.width, height: region.height) }
        settings.showsCursor = only == nil
        var picture: CGImage?
        SCScreenshotManager.captureImage(contentFilter: filter, configuration: settings) { image, error in
            picture = image
            problem = error
            signal.signal()
        }
        if signal.wait(timeout: .now() + 10) == .timedOut { throw HelperFailure("failed", "ScreenCaptureKit gave no answer.") }
        guard let picture else { throw refusal(problem) }
        return picture
    }

    /// Fallback for macOS 12 and 13: the window server composites the selected windows in screen order.
    private static func withWindowList(hidden: [String], only: [String]?, bounds: CGRect = display) throws -> CGImage {
        guard let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
            throw refusal(nil)
        }
        let me = ProcessInfo.processInfo.processIdentifier
        let ids: [CGWindowID] = info.compactMap { window in
            guard let pid = window[kCGWindowOwnerPID as String] as? Int32, pid != me,
                  let number = window[kCGWindowNumber as String] as? Int else { return nil }
            let bundle = NSRunningApplication(processIdentifier: pid)?.bundleIdentifier ?? ""
            if matches(bundle, hidden) { return nil }
            if let only, !matches(bundle, only) { return nil }
            return CGWindowID(number)
        }
        let list = ids.map { NSNumber(value: $0) } as CFArray
        let resolution: CGWindowImageOption = bounds == display ? .nominalResolution : .bestResolution
        guard let picture = CGImage(windowListFromArrayScreenBounds: bounds, windowArray: list, imageOption: resolution) else {
            throw refusal(nil)
        }
        return picture
    }

    /// `image` redrawn at exactly `size`: coordinates the model passes and approval previews are pixels of it.
    static func resize(_ image: CGImage, to size: (width: Int, height: Int)) throws -> CGImage {
        if image.width == size.width && image.height == size.height { return image }
        guard let context = CGContext(
            data: nil, width: size.width, height: size.height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
        ) else { throw HelperFailure("failed", "The screenshot could not be scaled.") }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: size.width, height: size.height))
        guard let out = context.makeImage() else { throw HelperFailure("failed", "The screenshot could not be scaled.") }
        return out
    }

    private static func refusal(_ error: Error?) -> HelperFailure {
        if !CGPreflightScreenCaptureAccess() {
            return HelperFailure("permission", "Screen Recording has not been granted to the app running Haarness.")
        }
        return HelperFailure("failed", "Capturing the screen failed: \(error?.localizedDescription ?? "no reason given").")
    }

    static func jpeg(_ image: CGImage, quality: Double = 0.72) throws -> Data {
        let data = NSMutableData()
        guard let target = CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil) else {
            throw HelperFailure("failed", "The screenshot could not be encoded.")
        }
        CGImageDestinationAddImage(target, image, [kCGImageDestinationLossyCompressionQuality: quality] as CFDictionary)
        guard CGImageDestinationFinalize(target) else { throw HelperFailure("failed", "The screenshot could not be encoded.") }
        return data as Data
    }

    /// Crops a small JPEG around `point` (in pixels of `image`) and circles the spot, for the approval card.
    static func preview(_ image: CGImage, around point: (x: Double, y: Double), width: Int = 360) throws -> Data {
        let full = (w: Double(image.width), h: Double(image.height))
        let side = min(Double(width), full.w)
        let tall = min(side * 0.6, full.h)
        let left = min(max(point.x - side / 2, 0), full.w - side)
        let top = min(max(point.y - tall / 2, 0), full.h - tall)
        guard let crop = image.cropping(to: CGRect(x: left, y: top, width: side, height: tall)),
              let context = CGContext(
                  data: nil, width: crop.width, height: crop.height, bitsPerComponent: 8, bytesPerRow: 0,
                  space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
              ) else { throw HelperFailure("failed", "The preview could not be made.") }
        context.draw(crop, in: CGRect(x: 0, y: 0, width: crop.width, height: crop.height))
        // Core Graphics has a bottom-left origin; convert the point into crop coordinates.
        let cx = point.x - left
        let cy = Double(crop.height) - (point.y - top)
        context.setStrokeColor(Marker.color.cgColor)
        context.setLineWidth(3)
        context.strokeEllipse(in: CGRect(x: cx - 13, y: cy - 13, width: 26, height: 26))
        guard let marked = context.makeImage() else { throw HelperFailure("failed", "The preview could not be made.") }
        return try jpeg(marked, quality: 0.7)
    }
}

/// The windows on screen, front to back, menus and panels of background apps included.
enum WindowList {
    static func current(viewport: Viewport, hidden: [String]) -> [[String: Any]] {
        guard let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
            return []
        }
        let me = ProcessInfo.processInfo.processIdentifier
        return info.compactMap { window in
            guard let pid = window[kCGWindowOwnerPID as String] as? Int32, pid != me,
                  let bounds = window[kCGWindowBounds as String] as? [String: Double],
                  let alpha = window[kCGWindowAlpha as String] as? Double, alpha > 0 else { return nil }
            let box = Box(x: bounds["X"] ?? 0, y: bounds["Y"] ?? 0, width: bounds["Width"] ?? 0, height: bounds["Height"] ?? 0)
            if box.width < 2 || box.height < 2 { return nil }
            if (window[kCGWindowLayer as String] as? Int ?? 0) >= pointerLayer { return nil }
            let app = NSRunningApplication(processIdentifier: pid)
            let bundle = app?.bundleIdentifier ?? ""
            if matches(bundle, hidden) { return nil }
            var out: [String: Any] = [
                "id": window[kCGWindowNumber as String] as? Int ?? 0,
                "pid": Int(pid),
                "app": app?.localizedName ?? (window[kCGWindowOwnerName as String] as? String) ?? "",
                "bundle": bundle,
                "layer": window[kCGWindowLayer as String] as? Int ?? 0,
                "frame": viewport.pixels(box).json,
            ]
            // The window server only gives titles to apps with Screen Recording.
            if let title = window[kCGWindowName as String] as? String, !title.isEmpty { out["title"] = shorten(title, 120) }
            return out
        }
    }

    /// The app whose window is frontmost at a screen point, skipping the transparent full-screen windows the Dock
    /// and other system agents keep above everything.
    static func owner(_ x: Double, _ y: Double) -> NSRunningApplication? {
        top(x, y).flatMap { NSRunningApplication(processIdentifier: $0.pid) }
    }

    /// The topmost window under a screen point, apart from the helper's own, the pointer's and screen-wide overlays:
    /// its owner and its window level (0 for ordinary windows; menus and pop-ups sit higher).
    static func top(_ x: Double, _ y: Double) -> (pid: pid_t, layer: Int)? {
        let screen = Screenshot.display
        for window in entries() where window.frame.holds(x, y) {
            if window.layer != 0 && window.frame.width >= screen.width && window.frame.height >= screen.height - 40 { continue }
            return (window.pid, window.layer)
        }
        return nil
    }

    /// One window as the window server lists it.
    struct Entry {
        var id: CGWindowID
        var pid: pid_t
        var layer: Int
        /// In screen points, top-left origin.
        var frame: Box
        var app: String
        var title: String?
        var onScreen: Bool
    }

    /// The pointer's own window level: the pointer is drawn in a window that belongs to no app and takes no input.
    static let pointerLayer = Int(CGWindowLevelForKey(.cursorWindow))

    /// The windows on screen (or every window, `onScreenOnly` false), front to back, without the helper's own, the
    /// pointer's, invisible ones and slivers.
    static func entries(onScreenOnly: Bool = true) -> [Entry] {
        let options: CGWindowListOption = onScreenOnly ? [.optionOnScreenOnly, .excludeDesktopElements] : [.optionAll, .excludeDesktopElements]
        guard let info = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] else { return [] }
        let me = ProcessInfo.processInfo.processIdentifier
        return info.compactMap { window in
            guard let pid = window[kCGWindowOwnerPID as String] as? Int32, pid != me,
                  let number = window[kCGWindowNumber as String] as? Int,
                  let bounds = window[kCGWindowBounds as String] as? [String: Double] else { return nil }
            let layer = window[kCGWindowLayer as String] as? Int ?? 0
            let owner = window[kCGWindowOwnerName as String] as? String ?? ""
            let title = window[kCGWindowName as String] as? String
            if layer >= pointerLayer || (owner == "Window Server" && title == "Cursor") { return nil }
            if onScreenOnly && ((window[kCGWindowAlpha as String] as? Double) ?? 0) <= 0 { return nil }
            let frame = Box(x: bounds["X"] ?? 0, y: bounds["Y"] ?? 0, width: bounds["Width"] ?? 0, height: bounds["Height"] ?? 0)
            if frame.width < 2 || frame.height < 2 { return nil }
            return Entry(
                id: CGWindowID(number), pid: pid, layer: layer, frame: frame,
                app: NSRunningApplication(processIdentifier: pid)?.localizedName ?? owner, title: title,
                onScreen: (window[kCGWindowIsOnscreen as String] as? Bool) ?? onScreenOnly
            )
        }
    }

    /// The topmost on-screen window of `pids` under a screen point, whatever other apps' windows lie over it.
    static func topmost(of pids: Set<pid_t>, at x: Double, _ y: Double) -> Entry? {
        entries().first { pids.contains($0.pid) && $0.frame.holds(x, y) }
    }

    /// Whether another app's window lies over any of `pid`'s on-screen windows. Status items, the menu bar, the Dock and
    /// other screen furniture do not count.
    static func covered(_ pid: pid_t, besides helpers: Set<pid_t> = []) -> Bool {
        let all = entries()
        var above: [Entry] = []
        for window in all {
            if window.pid == pid && window.layer == 0 {
                if above.contains(where: { $0.frame.intersection(window.frame) != nil }) { return true }
            } else if window.pid != pid && !helpers.contains(window.pid) && (0..<20).contains(window.layer) {
                above.append(window)
            }
        }
        return false
    }
}
