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

    // MARK: Background clicks

    func testBackgroundClickPlanMovesFirstAndCountsPresses() {
        let spot = Point(500, 300)
        let single = backgroundClickPlan(at: spot, origin: Point(400, 200), button: 0, count: 1, outsidePress: false, windowLocal: false)
        XCTAssertEqual(single.map(\.kind), [.move, .down, .up])
        XCTAssertEqual(single[0].clickState, 0)
        XCTAssertEqual(single[1].clickState, 1)
        // Before macOS 27 the window server reads the screen point.
        XCTAssertEqual(single.map(\.stamped), [spot, spot, spot])
        let triple = backgroundClickPlan(at: spot, origin: Point(0, 0), button: 0, count: 3, outsidePress: false, windowLocal: false)
        XCTAssertEqual(triple.filter { $0.kind == .down }.map(\.clickState), [1, 2, 3])
        XCTAssertEqual(triple.filter { $0.kind == .up }.map(\.clickState), [1, 2, 3])
        XCTAssertEqual(triple.last?.pause, 0)
        XCTAssertEqual(backgroundClickPlan(at: spot, origin: spot, button: 0, count: 9, outsidePress: false, windowLocal: false).count, 7)
    }

    func testChromiumGetsOnePressOutsideEveryWindowFirst() {
        let plan = backgroundClickPlan(at: Point(50, 60), origin: Point(0, 0), button: 0, count: 2, outsidePress: true, windowLocal: false)
        XCTAssertEqual(plan.map(\.kind), [.move, .down, .up, .down, .up, .down, .up])
        XCTAssertEqual(plan[1].screen, outsidePoint)
        XCTAssertEqual(plan[2].screen, outsidePoint)
        XCTAssertEqual(plan[3].screen, Point(50, 60))
        XCTAssertEqual(plan.map(\.phase), [2, 1, 2, 3, 3, 3, 3])
        // A right click goes without it: the outside press is a left press.
        let right = backgroundClickPlan(at: Point(50, 60), origin: Point(0, 0), button: 1, count: 1, outsidePress: true, windowLocal: false)
        XCTAssertEqual(right.map(\.kind), [.move, .down, .up])
        XCTAssertEqual(right[1].button, 1)
    }

    func testMacOS27StampsWindowLocalPoints() {
        XCTAssertFalse(windowLocationIsLocal(macOSMajor: 26))
        XCTAssertTrue(windowLocationIsLocal(macOSMajor: 27))
        let plan = backgroundClickPlan(at: Point(500, 300), origin: Point(400, 200), button: 0, count: 1, outsidePress: true, windowLocal: true)
        XCTAssertEqual(plan[0].screen, Point(500, 300))
        XCTAssertEqual(plan[0].stamped, Point(100, 100))
        // The outside press keeps its spot either way.
        XCTAssertEqual(plan[1].stamped, outsidePoint)
        XCTAssertEqual(plan[3].stamped, Point(100, 100))
    }

    // MARK: Shortcuts

    func testMenuShortcutsMatchKeyAndModifiersExactly() throws {
        let save = try KeyCombo.read("Meta+S")
        XCTAssertTrue(save.matchesMenuItem(character: "S", modifiers: 0))
        XCTAssertTrue(save.matchesMenuItem(character: "s", modifiers: 0))
        XCTAssertFalse(save.matchesMenuItem(character: "S", modifiers: MenuModifier.shift), "Save As is Command-Shift-S")
        XCTAssertFalse(save.matchesMenuItem(character: "S", modifiers: MenuModifier.noCommand))
        XCTAssertFalse(save.matchesMenuItem(character: "A", modifiers: 0))
        let saveAs = try KeyCombo.read("Meta+Shift+s")
        XCTAssertTrue(saveAs.matchesMenuItem(character: "S", modifiers: MenuModifier.shift))
        XCTAssertFalse(saveAs.matchesMenuItem(character: "S", modifiers: 0))
        let hideOthers = try KeyCombo.read("Meta+Alt+h")
        XCTAssertTrue(hideOthers.matchesMenuItem(character: "H", modifiers: MenuModifier.option))
        XCTAssertFalse(hideOthers.matchesMenuItem(character: "H", modifiers: MenuModifier.option | MenuModifier.control))
        XCTAssertTrue(try KeyCombo.read("Meta+,").matchesMenuItem(character: ",", modifiers: 0))
        // Without Command, or with a named key, a combination is a key press.
        XCTAssertFalse(try KeyCombo.read("Control+a").mayBeMenuShortcut)
        XCTAssertFalse(try KeyCombo.read("Meta+Enter").mayBeMenuShortcut)
        XCTAssertFalse(try KeyCombo.read("Meta+ArrowUp").matchesMenuItem(character: "", modifiers: 0))
    }

    /// A layout typing Cyrillic on its own and Latin with Command, like the Russian ones.
    static let cyrillic = KeyLayout(
        plain: [0x08: "с", 0x09: "м", 0x00: "ф", 0x2C: ".", 0x12: "1"],
        shifted: [0x08: "С", 0x09: "М", 0x00: "Ф", 0x2C: ",", 0x12: "!"],
        command: [0x08: "c", 0x09: "v", 0x00: "a", 0x2C: "/", 0x12: "1"]
    )

    func testShortcutsUseTheCommandLayerOfTheLayout() throws {
        XCTAssertEqual(Self.cyrillic.resolve(try KeyCombo.read("Meta+c"))?.code, 0x08)
        XCTAssertEqual(Self.cyrillic.resolve(try KeyCombo.read("Meta+v"))?.code, 0x09)
        // Letter case means nothing in a shortcut: Command-S, not Command-Shift-S.
        let save = KeyLayout.us.resolve(try KeyCombo.read("Meta+S"))
        XCTAssertEqual(save?.code, 0x01)
        XCTAssertEqual(save?.held, [.command])
        // A Dvorak layout whose Command layer is QWERTY presses the QWERTY key; one whose Command layer is Dvorak too
        // presses the Dvorak one.
        let dvorakQwerty = KeyLayout(plain: [0x22: "c", 0x08: "j"], shifted: [:], command: [0x08: "c", 0x22: "i"])
        XCTAssertEqual(dvorakQwerty.resolve(try KeyCombo.read("Meta+c"))?.code, 0x08)
        let dvorak = KeyLayout(plain: [0x22: "c", 0x08: "j"], shifted: [:], command: [0x22: "c", 0x08: "j"])
        XCTAssertEqual(dvorak.resolve(try KeyCombo.read("Meta+c"))?.code, 0x22)
        // A shifted symbol adds the Shift it needs.
        let help = KeyLayout.us.resolve(try KeyCombo.read("Meta+?"))
        XCTAssertEqual(help?.code, 0x2C)
        XCTAssertEqual(help?.held, [.command, .shift])
    }

    func testCharactersTheLayoutLacksComeFromAUSKeyboard() throws {
        // No Latin q anywhere on this layout: the US position is used.
        XCTAssertEqual(Self.cyrillic.resolve(try KeyCombo.read("Meta+q"))?.code, 0x0C)
        XCTAssertEqual(Self.cyrillic.resolve(try KeyCombo.read("Control+z"))?.code, 0x06)
        // Plain keys type the character exactly, with Shift where the layout needs it.
        let cyrillicEs = Self.cyrillic.resolve(try KeyCombo.read("С"))
        XCTAssertEqual(cyrillicEs?.code, 0x08)
        XCTAssertEqual(cyrillicEs?.held, [.shift])
        XCTAssertEqual(Self.cyrillic.resolve(try KeyCombo.read("!"))?.held, [.shift])
        XCTAssertEqual(KeyLayout.us.resolve(try KeyCombo.read("A"))?.held, [.shift])
        XCTAssertEqual(KeyLayout.us.resolve(try KeyCombo.read("a"))?.held, [])
        XCTAssertEqual(KeyLayout.us.resolve(try KeyCombo.read("Enter"))?.code, 0x24)
        XCTAssertEqual(KeyLayout.us.resolve(try KeyCombo.read("Shift+Tab"))?.held, [.shift])
        XCTAssertNil(KeyLayout.us.resolve(KeyCombo(held: [], key: "ж")))
    }

    // MARK: Typing

    func testLineBreaksPressReturnAndTabsPressTab() {
        let units = { (text: String) in TypingStep.text(Array(text.utf16)) }
        XCTAssertEqual(typingSteps("ok\r"), [units("ok"), .enter])
        XCTAssertEqual(typingSteps("\r"), [.enter])
        XCTAssertEqual(typingSteps("a\nb\r\nc\td"), [units("a"), .enter, units("b"), .enter, units("c"), .tab, units("d")])
        XCTAssertEqual(typingSteps(String(repeating: "x", count: 20)).count, 2)
        XCTAssertTrue(isOneLine("hello lab"))
        XCTAssertFalse(isOneLine("two\nlines"))
        XCTAssertFalse(isOneLine("enter\r"))
        XCTAssertFalse(isOneLine("a\tb"))
    }

    // MARK: Snapshot search

    func testOutlineSearchKeepsMatchesAndWhatHoldsThem() {
        let entries = [
            OutlineEntry(level: 0, line: "window", words: "window Lab"),
            OutlineEntry(level: 1, line: "group", words: "group Form"),
            OutlineEntry(level: 2, line: "name", words: "text-field Name"),
            OutlineEntry(level: 2, line: "save", words: "button Save"),
            OutlineEntry(level: 1, line: "list", words: "list Rows"),
            OutlineEntry(level: 2, line: "row", words: "static-text Row 1"),
            OutlineEntry(level: 2, line: "(hidden)", words: ""),
            OutlineEntry(level: 0, line: "menu", words: "menu Édition"),
        ]
        XCTAssertEqual(filterOutline(entries, keeping: "save").map(\.line), ["window", "group", "save"])
        XCTAssertEqual(filterOutline(entries, keeping: "ROW").map(\.line), ["window", "list", "row"])
        XCTAssertEqual(filterOutline(entries, keeping: "edition").map(\.line), ["menu"])
        XCTAssertEqual(filterOutline(entries, keeping: "absent"), [])
        XCTAssertEqual(filterOutline(entries, keeping: " ").count, entries.count)
        // A password's value is never searched.
        let secret = OutlineItem(role: "AXTextField", value: "hunter2", password: true)
        XCTAssertFalse(outlineWords(secret).contains("hunter2"))
    }

    // MARK: Windows an action opened or closed

    func testWindowChangesNameOtherAppsWithoutTitles() {
        let before = [WindowFacts(id: 1, pid: 10, app: "Lab", layer: 0, title: "Haarness Lab"),
                      WindowFacts(id: 2, pid: 20, app: "Mail", layer: 0, title: "Re: salary")]
        let after = [WindowFacts(id: 1, pid: 10, app: "Lab", layer: 0, title: "Haarness Lab"),
                     WindowFacts(id: 3, pid: 10, app: "Lab", layer: 0, title: "Save", role: "AXSheet"),
                     WindowFacts(id: 4, pid: 20, app: "Mail", layer: 0, title: "New mail from Sam", subrole: "AXDialog"),
                     WindowFacts(id: 5, pid: 10, app: "Lab", layer: 101)]
        let changes = windowChanges(before: before, after: after, target: 10)
        XCTAssertEqual(changes.opened, [["app": "Lab", "role": "sheet", "title": "Save"],
                                        ["app": "Mail", "role": "dialog"],
                                        ["app": "Lab", "role": "menu"]])
        XCTAssertEqual(changes.closed, [["app": "Mail", "role": "window"]])
    }
}
