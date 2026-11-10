import Foundation

/// One step of typing a text: a run of characters sent as Unicode key events, or Return or Tab pressed as a key.
public enum TypingStep: Equatable {
    case text([UInt16])
    case enter
    case tab
}

/// The steps that type `text`: line breaks (`\n`, `\r` or `\r\n`) press Return and `\t` presses Tab, since a raw control
/// character in a key event does nothing useful in most fields; other characters go in runs of at most `chunk` UTF-16
/// units, never splitting a surrogate pair.
public func typingSteps(_ text: String, chunk: Int = 16) -> [TypingStep] {
    var steps: [TypingStep] = []
    var pending: [UInt16] = []
    func flush() {
        steps += pieces(pending, size: chunk).map { TypingStep.text($0) }
        pending.removeAll()
    }
    for char in text {
        switch char {
        case "\n", "\r", "\r\n":
            flush()
            steps.append(.enter)
        case "\t":
            flush()
            steps.append(.tab)
        default:
            pending.append(contentsOf: Array(String(char).utf16))
        }
    }
    flush()
    return steps
}

/// Whether `text` fits a one-line field as it is: no line break and no tab, which would move focus or press a button.
public func isOneLine(_ text: String) -> Bool {
    !text.contains { $0.isNewline || $0 == "\t" }
}
