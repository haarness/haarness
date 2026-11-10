import Foundation

/// One line of a snapshot outline before it is printed: its depth, its text, and the words a search looks at.
public struct OutlineEntry: Equatable {
    public var level: Int
    public var line: String
    /// Role, name, description, value and placeholder; empty for the lines that stand in for hidden elements.
    public var words: String

    public init(level: Int, line: String, words: String) {
        self.level = level
        self.line = line
        self.words = words
    }
}

/// The entries whose words contain `query` (ignoring case and accents), each with the entries it sits inside, so the
/// agent still sees which window, list or group a match belongs to. Order and indentation are kept.
public func filterOutline(_ entries: [OutlineEntry], keeping query: String) -> [OutlineEntry] {
    let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !needle.isEmpty else { return entries }
    var kept = Set<Int>()
    var path: [Int] = []
    for (index, entry) in entries.enumerated() {
        while let last = path.last, entries[last].level >= entry.level { path.removeLast() }
        if !entry.words.isEmpty, entry.words.range(of: needle, options: [.caseInsensitive, .diacriticInsensitive]) != nil {
            kept.formUnion(path)
            kept.insert(index)
        }
        path.append(index)
    }
    return entries.indices.filter(kept.contains).map { entries[$0] }
}

/// The searchable words of an outline item.
public func outlineWords(_ item: OutlineItem) -> String {
    let parts = [roleName(item.role), item.name, item.detail, item.password ? nil : item.value, item.placeholder]
    return parts.compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " ")
}
