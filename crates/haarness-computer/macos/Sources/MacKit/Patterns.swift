import Foundation

/// Whether a bundle identifier matches one of `patterns`: the exact identifier, or a prefix ending in `.*`
/// (`com.jetbrains.*` matches every JetBrains IDE). The server sends the apps agents never see this way.
public func matches(_ bundle: String, _ patterns: [String]) -> Bool {
    guard !bundle.isEmpty else { return false }
    for pattern in patterns {
        if pattern.hasSuffix(".*") {
            let stem = String(pattern.dropLast(1))
            if bundle.hasPrefix(stem) || bundle == String(pattern.dropLast(2)) { return true }
        } else if pattern.hasSuffix("*") {
            if bundle.hasPrefix(String(pattern.dropLast())) { return true }
        } else if bundle == pattern {
            return true
        }
    }
    return false
}
