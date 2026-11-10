import Foundation

/// The words on the bar the helper shows while an agent uses the screen. The server sends them in the person's
/// Haarness language with each request; English is the fallback until it does.
public struct BarText: Equatable {
    /// "Haarness is working in %@", with the app's name in place of %@.
    public var usingApp: String
    /// Shown before the agent has picked an app.
    public var usingMac: String
    public var paused: String
    public var pause: String
    public var resume: String
    public var stop: String
    /// Tooltip on Stop.
    public var stopHint: String

    public static let english = BarText(
        usingApp: "Haarness is working in %@",
        usingMac: "Haarness is working on this Mac",
        paused: "Haarness is waiting for you",
        pause: "Pause",
        resume: "Resume",
        stop: "Stop",
        stopHint: "Esc stops it too, while it acts in the frontmost app"
    )

    public init(usingApp: String, usingMac: String, paused: String, pause: String, resume: String, stop: String, stopHint: String) {
        self.usingApp = usingApp
        self.usingMac = usingMac
        self.paused = paused
        self.pause = pause
        self.resume = resume
        self.stop = stop
        self.stopHint = stopHint
    }

    /// The words from a request's `bar` object; anything missing or empty keeps its English text.
    public init(_ json: [String: Any]?) {
        let english = BarText.english
        func word(_ key: String, _ fallback: String) -> String {
            guard let text = json?[key] as? String, !text.trimmingCharacters(in: .whitespaces).isEmpty else { return fallback }
            return shorten(text, 80)
        }
        var usingApp = word("usingApp", english.usingApp)
        // The app name goes where %@ is; text without exactly one placeholder would print garbage or crash.
        if usingApp.components(separatedBy: "%@").count != 2 || usingApp.contains("%%") { usingApp = english.usingApp }
        self.init(
            usingApp: usingApp,
            usingMac: word("usingMac", english.usingMac),
            paused: word("paused", english.paused),
            pause: word("pause", english.pause),
            resume: word("resume", english.resume),
            stop: word("stop", english.stop),
            stopHint: word("stopHint", english.stopHint)
        )
    }

    /// The bar's label for an app name, without format strings: the name is put in place of %@ as plain text.
    public func label(for app: String?) -> String {
        guard let app, !app.isEmpty else { return usingMac }
        return usingApp.replacingOccurrences(of: "%@", with: app)
    }
}
