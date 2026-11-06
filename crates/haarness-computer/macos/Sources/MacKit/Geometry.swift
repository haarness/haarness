import Foundation

/// A rectangle in screen points with a top-left origin, the way Accessibility and Quartz events measure the screen.
public struct Box: Equatable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }

    public var middle: (x: Double, y: Double) { (x + width / 2, y + height / 2) }

    public func holds(_ px: Double, _ py: Double) -> Bool {
        px >= x && py >= y && px <= x + width && py <= y + height
    }

    public func grown(by amount: Double) -> Box {
        Box(x: x - amount, y: y - amount, width: width + amount * 2, height: height + amount * 2)
    }

    public var json: [String: Double] { ["x": x.rounded(), "y": y.rounded(), "width": width.rounded(), "height": height.rounded()] }
}

/// How screenshot pixels relate to screen points. The model works on an image no longer than `longest` pixels on
/// its long side; the image keeps the display's proportions, so a single factor converts both axes.
public struct Viewport: Equatable {
    public let pointsWide: Double
    public let pointsHigh: Double
    public let factor: Double

    public init(pointsWide: Double, pointsHigh: Double, longest: Double) {
        self.pointsWide = pointsWide
        self.pointsHigh = pointsHigh
        let side = max(pointsWide, pointsHigh)
        factor = side > 0 ? min(1, longest / side) : 1
    }

    /// The screenshot size in pixels.
    public var size: (width: Int, height: Int) {
        (Int((pointsWide * factor).rounded()), Int((pointsHigh * factor).rounded()))
    }

    public func pixels(_ box: Box) -> Box {
        Box(x: box.x * factor, y: box.y * factor, width: box.width * factor, height: box.height * factor)
    }

    public func points(_ px: Double, _ py: Double) -> (x: Double, y: Double) {
        (px / factor, py / factor)
    }

    /// Whether a pixel coordinate lies on the screenshot.
    public func covers(_ px: Double, _ py: Double) -> Bool {
        let (w, h) = size
        return px >= 0 && py >= 0 && px <= Double(w) && py <= Double(h)
    }
}
