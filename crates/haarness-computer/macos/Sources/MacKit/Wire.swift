import Foundation

/// Version of the line protocol between the Haarness server and this helper. The server refuses a helper that reports
/// a different one, so an old helper left on disk never runs against a newer server.
public let protocolVersion = 1

/// The Haarness version this helper was built for. Release builds overwrite `Build.swift` with the real version.
public var helperBuild: String { build }

/// A refusal or failure the server can branch on: `code` is stable, `message` is written for the agent.
public struct HelperFailure: Error, Equatable {
    public let code: String
    public let message: String

    public init(_ code: String, _ message: String) {
        self.code = code
        self.message = message
    }
}

/// Reads a JSON number whether it arrived as an integer or a double.
public func double(_ value: Any?) -> Double? {
    switch value {
    case let number as Double: return number
    case let number as Int: return Double(number)
    case let number as NSNumber: return number.doubleValue
    default: return nil
    }
}

/// Every event this helper posts carries this value in `eventSourceUserData`, so the watcher of the person's own
/// mouse and keyboard can skip the helper's events. ASCII "HMAC".
public let syntheticTag: Int64 = 0x484D_4143

/// An element reference as agents write it: `e12` or `@e12`. Returns the number, or nil when it is not a reference.
public func refNumber(_ text: String) -> Int? {
    var body = Substring(text.trimmingCharacters(in: .whitespaces))
    if body.hasPrefix("@") { body = body.dropFirst() }
    guard body.hasPrefix("e") else { return nil }
    let digits = body.dropFirst()
    guard !digits.isEmpty, digits.first != "0", digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
    return Int(digits)
}
