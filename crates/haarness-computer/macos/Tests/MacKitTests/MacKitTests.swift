import XCTest
@testable import MacKit

final class MacKitTests: XCTestCase {
    func testKeyCombosReadBrowserAndMacSpellings() throws {
        XCTAssertEqual(try KeyCombo.read("Meta+S"), KeyCombo(held: [.command], key: "S"))
        XCTAssertEqual(try KeyCombo.read("cmd+shift+n"), KeyCombo(held: [.command, .shift], key: "n"))
        XCTAssertEqual(try KeyCombo.read("Enter"), KeyCombo(held: [], key: "enter"))
        XCTAssertEqual(try KeyCombo.read("ArrowDown"), KeyCombo(held: [], key: "arrowdown"))
        XCTAssertEqual(try KeyCombo.read("Meta++"), KeyCombo(held: [.command], key: "+"))
        XCTAssertEqual(try KeyCombo.read("Control+Plus"), KeyCombo(held: [.control], key: "+"))
        XCTAssertThrowsError(try KeyCombo.read("Hyper+S"))
        XCTAssertThrowsError(try KeyCombo.read("Meta+"))
        XCTAssertThrowsError(try KeyCombo.read("Meta+\u{1B}"))
    }

    func testSessionEndingCombosAreRefusedInEverySpelling() throws {
        for keys in ["Meta+Shift+Q", "cmd+shift+q", "Meta+Q+Shift".replacingOccurrences(of: "+Q+Shift", with: "+Shift+Q"),
                     "Control+Meta+Q", "Meta+Alt+Escape", "cmd+opt+esc", "Meta+Alt+Shift+Q", "Control+Alt+Meta+Q"] {
            let combo = try KeyCombo.read(keys)
            var held = combo.held
            if combo.key.count == 1, combo.key != combo.key.lowercased() { held.append(.shift) }
            XCTAssertTrue(KeyCombo.endsSession(held: held, key: combo.key), keys)
        }
        // Typing a capital Q involves Shift.
        let upper = try KeyCombo.read("Meta+Q")
        XCTAssertTrue(KeyCombo.endsSession(held: upper.held + [.shift], key: upper.key))
        XCTAssertFalse(KeyCombo.endsSession(held: [.command], key: "q"))
        XCTAssertFalse(KeyCombo.endsSession(held: [.command], key: "s"))
        XCTAssertTrue(KeyCombo.endsSession(held: [.command, .shift], code: 0x0C))
        XCTAssertFalse(KeyCombo.endsSession(held: [.command], code: 0x0C))
    }

    func testTypingCombos() throws {
        XCTAssertTrue(try KeyCombo.read("a").types)
        XCTAssertTrue(try KeyCombo.read("Shift+A").types)
        XCTAssertFalse(try KeyCombo.read("Meta+A").types)
        XCTAssertFalse(try KeyCombo.read("Enter").types)
    }

    func testPiecesNeverSplitASurrogatePair() {
        let units = Array("abcdefghijklmno😀xyz".utf16)
        let split = pieces(units, size: 16)
        XCTAssertEqual(split.flatMap { $0 }, units)
        for piece in split { XCTAssertFalse(UTF16.isLeadSurrogate(piece.last!)) }
    }

    func testViewportScalesBothAxesByOneFactor() {
        let view = Viewport(pointsWide: 2912, pointsHigh: 1632, longest: 1456)
        XCTAssertEqual(view.factor, 0.5)
        XCTAssertEqual(view.size.width, 1456)
        XCTAssertEqual(view.size.height, 816)
        XCTAssertEqual(view.points(100, 50).x, 200)
        XCTAssertTrue(view.covers(1456, 816))
        XCTAssertFalse(view.covers(1457, 10))
        XCTAssertEqual(Viewport(pointsWide: 1000, pointsHigh: 600, longest: 1456).factor, 1)
    }

    func testOutlineLinesMatchTheBrowserSnapshotShape() {
        let line = outlineLine(OutlineItem(ref: 12, role: "AXButton", name: "Save", box: Box(x: 410.2, y: 220, width: 64, height: 22), enabled: false, level: 1))
        XCTAssertEqual(line, "  @e12 [button] \"Save\" at=410,220 size=64x22 disabled")
        let secret = outlineLine(OutlineItem(ref: 3, role: "AXTextField", subrole: "AXSecureTextField", value: "hunter2", password: true))
        XCTAssertFalse(secret.contains("hunter2"))
        XCTAssertTrue(secret.contains("value=[redacted]"))
        XCTAssertEqual(roleName("AXTextField"), "text-field")
        XCTAssertEqual(outlineLine(OutlineItem(role: "AXStaticText", name: "Say \"hi\"\nnow")), "[static-text] \"Say \\\"hi\\\" now\"")
    }

    func testContainersAndTextGetNoRef() {
        XCTAssertFalse(worthShowing(OutlineItem(role: "AXGroup")))
        XCTAssertTrue(worthShowing(OutlineItem(role: "AXGroup", name: "Sidebar")))
        XCTAssertTrue(worthShowing(OutlineItem(role: "AXButton")))
        XCTAssertTrue(takesRef(OutlineItem(role: "AXStaticText", name: "Hello")))
        XCTAssertFalse(takesRef(OutlineItem(role: "AXGroup")))
        XCTAssertTrue(takesRef(OutlineItem(role: "AXButton")))
    }

    func testBudgetStopsAndSaysSo() {
        var budget = OutlineBudget(limit: 20)
        XCTAssertTrue(budget.take("0123456789"))
        XCTAssertFalse(budget.take("0123456789"))
        XCTAssertTrue(budget.exhausted)
        XCTAssertTrue(budget.text.contains("left out"))
    }

    func testBundlePatterns() {
        XCTAssertTrue(matches("com.jetbrains.intellij", ["com.jetbrains.*"]))
        XCTAssertTrue(matches("dev.haarness.desktop", ["dev.haarness.desktop"]))
        XCTAssertFalse(matches("com.jetbrainsx", ["com.jetbrains.*"]))
        XCTAssertFalse(matches("", ["*"]))
    }

    func testRefs() {
        XCTAssertEqual(refNumber("e12"), 12)
        XCTAssertEqual(refNumber("@e3"), 3)
        XCTAssertNil(refNumber("e0"))
        XCTAssertNil(refNumber("12"))
        XCTAssertNil(refNumber("e1x"))
    }

    func testBarTextKeepsEnglishForMissingOrUnsafeWords() {
        let text = BarText(["usingApp": "Haarness verwendet %@", "pause": "Pausieren", "stop": ""])
        XCTAssertEqual(text.label(for: "Notizen"), "Haarness verwendet Notizen")
        XCTAssertEqual(text.pause, "Pausieren")
        XCTAssertEqual(text.stop, "Stop")
        XCTAssertEqual(BarText(["usingApp": "%@ %@"]).usingApp, BarText.english.usingApp)
        XCTAssertEqual(BarText(["usingApp": "no placeholder"]).usingApp, BarText.english.usingApp)
        XCTAssertEqual(BarText(nil).label(for: nil), "Haarness is working on this Mac")
        // An app name with a percent sign is printed as it is.
        XCTAssertEqual(BarText.english.label(for: "100%@ App"), "Haarness is working in 100%@ App")
    }

    func testPositionsAreTheVisiblePartOfAnElement() {
        let list = Box(x: 100, y: 300, width: 200, height: 120)
        // A row inside the list keeps its frame; one half under the list's edge keeps its visible half.
        XCTAssertEqual(sight(of: Box(x: 100, y: 320, width: 50, height: 20), within: list), .seen(Box(x: 100, y: 320, width: 50, height: 20)))
        XCTAssertEqual(sight(of: Box(x: 100, y: 410, width: 50, height: 20), within: list), .seen(Box(x: 100, y: 410, width: 50, height: 10)))
        // A row scrolled above the list reports a frame over whatever sits there; it counts as hidden.
        XCTAssertEqual(sight(of: Box(x: 100, y: 230, width: 50, height: 20), within: list), .hidden)
        XCTAssertEqual(sight(of: Box(x: 100, y: 420, width: 50, height: 20), within: list), .hidden)
        // Nothing to clip against, no frame, or a frame without area: nothing can be told, so nothing is hidden.
        XCTAssertEqual(sight(of: Box(x: 0, y: 0, width: 5, height: 5), within: nil), .seen(Box(x: 0, y: 0, width: 5, height: 5)))
        XCTAssertEqual(sight(of: nil, within: list), .seen(nil))
        XCTAssertEqual(sight(of: Box(x: 0, y: 0, width: 0, height: 0), within: list), .seen(Box(x: 0, y: 0, width: 0, height: 0)))
        XCTAssertNil(Box(x: 0, y: 0, width: 10, height: 10).intersection(Box(x: 10, y: 0, width: 10, height: 10)))
        XCTAssertEqual(hiddenLine(1, level: 2), "    (1 more element is scrolled out of view here; scroll this area to reach it.)")
        XCTAssertEqual(hiddenLine(54, level: 0), "(54 more elements are scrolled out of view here; scroll this area to reach them.)")
    }
}
