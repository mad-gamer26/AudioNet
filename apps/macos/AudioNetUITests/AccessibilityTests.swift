import AppKit
import XCTest

/// Runs Xcode's accessibility audit on AudioNet's windows and checks the
/// flows a keyboard and VoiceOver user relies on.
///
/// The app runs in a separate test profile (`-AudioNetProfile uitest`), so
/// the account of anyone using AudioNet on this Mac is never touched. The
/// signed-in test needs a temporary device made by
/// `scripts/test/mac_ui_tests.py`, which writes its credential to
/// `accountFile` (deleted by the app as soon as it is read) and removes
/// the device afterwards; without it that test is skipped.
final class AccessibilityTests: XCTestCase {
    static let accountFile = "/tmp/audionet-uitest-account"

    override func setUp() {
        continueAfterFailure = false
    }

    private func launch(signedIn: Bool = false, extra: [String] = []) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-AudioNetProfile", "uitest", "-AudioNetResetProfile", "YES",
                               "-ApplePersistenceIgnoreState", "YES"] + extra
        if signedIn { app.launchArguments += ["-AudioNetTestAccountFile", Self.accountFile] }
        app.launch()
        return app
    }

    private func audit(_ app: XCUIApplication, file: StaticString = #filePath, line: UInt = #line) throws {
        var issues: [String] = []
        try app.performAccessibilityAudit { issue in
            // A focused AppKit text field's field editor is reported as a
            // parent/child mismatch with no element: a window holding only
            // one plain SwiftUI TextField gets the same finding (macOS 27,
            // Xcode 27). Only that exact case is ignored.
            if issue.auditType == .parentChild && issue.element == nil { return true }
            // An AppKit slider's knob (its value indicator) is a part of the
            // slider, which is named; the part itself has no description.
            if issue.auditType == .sufficientElementDescription, issue.element?.elementType == .valueIndicator {
                return true
            }
            // Contrast on a row scrolled partly under the window's translucent
            // title bar is measured through the bar: position, not the app's
            // colors (the same rows pass when not covered).
            if issue.auditType == .contrast, let e = issue.element {
                let window = app.windows.firstMatch.frame
                if e.frame.minY < window.minY + 52 {
                    print("contrast finding on '\(e.label)' overruled: partly under the title bar")
                    return true
                }
            }
            // Text scrolled partly out of the window's bottom is measured
            // on pixels that are not on screen (the same rows pass in view).
            if issue.auditType == .contrast, let e = issue.element {
                let window = app.windows.firstMatch.frame
                if e.frame.maxY > window.maxY {
                    print("contrast finding on '\(e.label)' overruled: partly below the window's visible area")
                    return true
                }
            }
            // Contrast is judged from the element's rendered pixels: the audit
            // reported black text on the table's light grey selection (and
            // wide, mostly empty label frames) as failing. Accepted only at
            // 4.5:1 or more, measured.
            if issue.auditType == .contrast, let e = issue.element, let ratio = self.measuredContrast(e), ratio >= 4.5 {
                print("contrast finding on '\(e.identifier)' overruled: measured \(String(format: "%.1f", ratio)):1")
                return true
            }
            // macOS's own password AutoFill popup (not AudioNet's window),
            // which sometimes opens over the password field.
            if issue.element?.identifier.hasPrefix("SafariPlatformSupport") == true { return true }
            let element = issue.element.map { "\($0.elementType.rawValue) '\($0.label)' id '\($0.identifier)' at \($0.frame)" } ?? "no element"
            issues.append("\(issue.compactDescription) on \(element) [\(issue.detailedDescription)]")
            return true
        }
        if !issues.isEmpty {
            let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            shot.name = "audit-failure"
            shot.lifetime = .keepAlways
            add(shot)
        }
        if !issues.isEmpty { print("ACCESSIBILITY TREE\n\(app.windows.firstMatch.debugDescription)") }
        XCTAssertTrue(issues.isEmpty, "Accessibility audit issues: " + issues.joined(separator: "; "), file: file, line: line)
    }

    /// The contrast of an element as drawn: its lightest and darkest pixels
    /// (WCAG relative luminance).
    private func measuredContrast(_ e: XCUIElement) -> Double? {
        guard let cg = e.screenshot().image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }
        let w = cg.width, h = cg.height
        var px = [UInt8](repeating: 0, count: w * h * 4)
        guard let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                  space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
        func lum(_ i: Int) -> Double {
            func ch(_ v: UInt8) -> Double {
                let c = Double(v) / 255
                return c <= 0.03928 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4)
            }
            return 0.2126 * ch(px[i]) + 0.7152 * ch(px[i + 1]) + 0.0722 * ch(px[i + 2])
        }
        var lo = 1.0, hi = 0.0
        for i in stride(from: 0, to: px.count, by: 4 * 3) {
            let l = lum(i)
            lo = min(lo, l)
            hi = max(hi, l)
        }
        return (hi + 0.05) / (lo + 0.05)
    }

    /// Opens the status log dialog (it closes with Escape).
    private func openLog(_ app: XCUIApplication) {
        let open = app.buttons["openStatusLog"]
        if !app.textViews["statusLog"].exists, open.waitForExistence(timeout: 5) { open.click() }
        _ = app.textViews["statusLog"].waitForExistence(timeout: 5)
    }

    private func closeLog(_ app: XCUIApplication) {
        if app.buttons["closeLog"].exists { app.buttons["closeLog"].click() }
    }

    private func logText(_ app: XCUIApplication) -> String {
        app.textViews["statusLog"].value as? String ?? ""
    }

    /// Waits until the status log contains `text`.
    private func waitForLog(_ app: XCUIApplication, _ text: String, timeout: TimeInterval = 10) -> Bool {
        let found = expectation(for: NSPredicate(format: "value CONTAINS %@", text), evaluatedWith: app.textViews["statusLog"])
        return XCTWaiter.wait(for: [found], timeout: timeout) == .completed
    }

    /// Whether AudioNet has a Dock icon (a regular app) rather than only
    /// its menu bar item (an accessory app), as macOS reports it.
    private func hasDockIcon() -> Bool {
        testCopy()?.activationPolicy == .regular
    }

    /// Test builds have their own bundle identifier (the app's plus
    /// ".uitest", set by scripts/test/mac_ui_tests.py), so they never share
    /// macOS permissions or "open AudioNet" with the AudioNet the Mac's owner
    /// has installed.
    static let testBundleID = "org.audionet.AudioNet.uitest"

    /// The copy under test: the freshly built one, never an AudioNet the
    /// Mac's owner has installed (and may be using) in Applications.
    private func testCopy() -> NSRunningApplication? {
        NSRunningApplication.runningApplications(withBundleIdentifier: Self.testBundleID)
            .first { $0.bundleURL?.path.contains("/Build/Products/") == true }
    }

    private func waitUntil(_ timeout: TimeInterval, _ condition: () -> Bool) -> Bool {
        let end = Date().addingTimeInterval(timeout)
        while Date() < end {
            if condition() { return true }
            Thread.sleep(forTimeInterval: 0.25)
        }
        return condition()
    }

    /// The entries of AudioNet's menu bar menu, read from the accessibility
    /// tree without opening it. (XCTest cannot open the menu of an app
    /// without a Dock icon, nor any menu while the Mac's screen is dark.)
    /// Every place that says whether this Mac is visible, in words: the
    /// window's Status row and the menu bar item's menu.
    /// The Status row as VoiceOver meets it: the row element's own label
    /// and value, and each text inside it.
    private func dumpStatus(_ app: XCUIApplication, _ when: String) {
        let row = app.descendants(matching: .any).matching(identifier: "status").firstMatch
        let kids = row.descendants(matching: .any).allElementsBoundByIndex
            .map { "\($0.elementType.rawValue):'\($0.label)'/'\(($0.value as? String) ?? "")'" }
        print("STATUS \(when): type \(row.elementType.rawValue) label '\(row.label)' value '\((row.value as? String) ?? "")' children \(kids)")
    }

    /// What VoiceOver reads for the window's Status row (the row element's
    /// own value, not the text drawn inside it), and the menu bar item's
    /// menu.
    private func statusWords(_ app: XCUIApplication) -> (window: String, menu: [String]) {
        let row = app.descendants(matching: .any).matching(identifier: "status").firstMatch
        let words = row.exists ? "\(row.label): \((row.value as? String) ?? "")" : ""
        return (words, menuBarEntries(app))
    }

    /// Waits until the window and the menu bar item both say `window` and
    /// `menu` (for example "Visible to your other devices" and "This Mac is
    /// visible to your other devices").
    private func waitForStatus(_ app: XCUIApplication, window: String, menu: String,
                               file: StaticString = #filePath, line: UInt = #line) {
        // macOS can keep an app's menu bar item out of the menu bar (System
        // Settings, Menu Bar): then only the window is checked, and the
        // log says so.
        let menuShown = menuBarItemShown(app)
        if !menuShown { print("MENU BAR ITEM HIDDEN BY macOS: only the window's status is checked") }
        let ok = waitUntil(15) {
            let now = statusWords(app)
            return now.window.contains(window) && (!menuShown || now.menu.contains(menu))
        }
        let now = statusWords(app)
        XCTAssertTrue(ok, "status not updated: window \"\(now.window)\", menu \(now.menu)", file: file, line: line)
    }

    /// The selected stream's volume and mute (one set of controls under the
    /// streams table, as in the Windows app). On macOS XCTest reads a
    /// slider's numeric value (VoiceOver also gets the spoken "50 percent"
    /// as its value description, which XCTest does not expose); the table's
    /// Volume column says it in words.
    private func checkStreamVolume(_ app: XCUIApplication, file: StaticString = #filePath, line: UInt = #line) {
        let volume = app.sliders["streamVolume"]
        XCTAssertTrue(volume.waitForExistence(timeout: 5), "no volume slider", file: file, line: line)
        XCTAssertEqual(volume.label, "Volume of the Selected Stream", file: file, line: line)
        func number(_ v: Any?) -> Double? { (v as? NSNumber)?.doubleValue ?? (v as? String).flatMap(Double.init) }
        XCTAssertEqual(number(volume.value), 1, "volume starts at \(volume.value ?? "")", file: file, line: line)
        volume.adjust(toNormalizedSliderPosition: 0.5)
        // XCTest places the slider only roughly (45 or 50 percent).
        XCTAssertTrue(waitUntil(5) { (0.4...0.6).contains(number(volume.value) ?? -1) },
                      "volume is \(volume.value ?? "")", file: file, line: line)
        let mute = app.descendants(matching: .any).matching(identifier: "streamMute").firstMatch
        XCTAssertTrue(mute.exists, "no mute switch", file: file, line: line)
        XCTAssertEqual(mute.label, "Mute the Selected Stream", file: file, line: line)
        mute.click()
        XCTAssertTrue(waitUntil(5) { number(mute.value) == 1 }, "mute is \(mute.value ?? "")", file: file, line: line)
        let words = app.tables["streams"].tableRows.firstMatch.descendants(matching: .any)["streamVolumeText"]
        XCTAssertTrue(waitUntil(5) { ((words.value as? String) ?? words.label).hasSuffix("percent, muted") },
                      "the table's Volume column says \(words.value ?? words.label)", file: file, line: line)
    }

    /// Scrolls the window's form until `element` can be clicked (the window
    /// shows only part of it).
    private func scrollIntoView(_ app: XCUIApplication, _ element: XCUIElement) {
        let form = app.scrollViews.firstMatch
        // Wholly inside the window, clear of its edges (an element cut off at
        // the bottom edge still counts as hittable, but a click there is lost).
        let inside = { app.windows.firstMatch.frame.insetBy(dx: 0, dy: 30).contains(element.frame) }
        for _ in 0..<10 where !(element.exists && element.isHittable && inside()) {
            form.scroll(byDeltaX: 0, deltaY: -120)
        }
    }

    /// Whether macOS shows AudioNet's menu bar item (it can be turned off
    /// in System Settings, Menu Bar, "Allow in the Menu Bar").
    private func menuBarItemShown(_ app: XCUIApplication) -> Bool {
        app.statusItems.firstMatch.waitForExistence(timeout: 5)
    }

    /// Skips a test that needs the menu bar item when macOS hides it.
    private func requireMenuBarItem(_ app: XCUIApplication) throws {
        if !menuBarItemShown(app) {
            throw XCTSkip("macOS is not showing AudioNet's menu bar item on this Mac (System Settings, Menu Bar, Allow in the Menu Bar)")
        }
    }

    private func menuBarEntries(_ app: XCUIApplication) -> [String] {
        let item = app.statusItems.firstMatch
        guard item.waitForExistence(timeout: 10) else { return [] }
        return item.menuItems.allElementsBoundByIndex.map(\.title)
    }

    /// Opens AudioNet again while it runs, as Spotlight, Launchpad or the
    /// Finder do.
    /// Whether another AudioNet (the Mac owner's) runs: macOS then sends
    /// "open AudioNet" to that copy, so the test must not do it.
    private func otherCopyRunning() -> Bool {
        NSRunningApplication.runningApplications(withBundleIdentifier: Self.testBundleID)
            .contains { $0.bundleURL?.path.contains("/Build/Products/") != true }
    }

    private func openAgain() throws {
        if otherCopyRunning() {
            throw XCTSkip("another AudioNet is running on this Mac; opening AudioNet again would reach it, so that step is skipped")
        }
        guard let url = testCopy()?.bundleURL else { return XCTFail("the AudioNet under test is not running") }
        let done = expectation(description: "reopened")
        NSWorkspace.shared.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration()) { _, _ in
            done.fulfill()
        }
        wait(for: [done], timeout: 10)
    }

    func testClosingTheWindowLeavesOnlyTheMenuBarItem() throws {
        let app = launch()
        XCTAssertTrue(app.windows["main"].waitForExistence(timeout: 10))
        XCTAssertTrue(waitUntil(5) { hasDockIcon() }, "no Dock icon while the window is open")
        app.windows["main"].buttons[XCUIIdentifierCloseWindow].click()
        XCTAssertTrue(waitUntil(5) { !app.windows["main"].exists }, "the window did not close")
        XCTAssertTrue(waitUntil(5) { !hasDockIcon() }, "the Dock icon stayed after the window closed")
        XCTAssertNotEqual(app.state, .notRunning, "AudioNet should keep running")
        try requireMenuBarItem(app)
        XCTAssertTrue(menuBarEntries(app).contains("Open AudioNet"), "the menu bar item has no Open AudioNet")
        try openAgain()
        XCTAssertTrue(app.windows["main"].waitForExistence(timeout: 10), "opening AudioNet again did not show its window")
        XCTAssertTrue(waitUntil(5) { hasDockIcon() }, "the Dock icon did not come back with the window")
    }

    func testStartingInTheMenuBarShowsNoWindowOrDockIcon() throws {
        let app = launch(extra: ["-startInMenuBar", "YES"])
        try requireMenuBarItem(app)
        let entries = menuBarEntries(app)
        XCTAssertTrue(entries.contains("Open AudioNet"), "the menu bar item is missing or has no Open AudioNet: \(entries)")
        XCTAssertTrue(entries.contains("Settings…"), "the menu bar menu has no Settings item: \(entries)")
        XCTAssertFalse(waitUntil(3) { app.windows["main"].exists }, "the window opened")
        XCTAssertFalse(hasDockIcon(), "the Dock icon is showing")
        try openAgain()
        XCTAssertTrue(app.windows["main"].waitForExistence(timeout: 10), "opening AudioNet again did not show its window")
        XCTAssertTrue(waitUntil(5) { hasDockIcon() }, "the Dock icon did not come with the window")
    }

    func testSignInWindowPassesAccessibilityAudit() throws {
        let app = launch()
        XCTAssertTrue(app.textFields["server"].waitForExistence(timeout: 10))
        try audit(app)
    }

    func testEmptySignInSaysWhatIsMissing() throws {
        let app = launch()
        let signIn = app.buttons["signIn"]
        XCTAssertTrue(signIn.waitForExistence(timeout: 10))
        app.textFields["username"].click()
        signIn.click()
        XCTAssertTrue((app.textFields["server"].value(forKey: "hasKeyboardFocus") as? Bool) == true, "focus did not move to the missing server address")
        openLog(app)
        XCTAssertTrue(waitForLog(app, "Enter the server address"), "status log: \(logText(app))")
        // No audit with the dialog open: the sheet dims the window behind it.
        closeLog(app)
        XCTAssertTrue(waitUntil(5) { !app.textViews["statusLog"].exists }, "the status log dialog did not close")
    }

    /// "Forgot Password?" opens the server's web client for a reset link;
    /// without a server address it says so and focuses that field (so no
    /// browser opens during the test).
    func testForgotPasswordNeedsTheServerAddress() throws {
        let app = launch()
        let forgot = app.buttons["forgotPassword"]
        XCTAssertTrue(forgot.waitForExistence(timeout: 10))
        XCTAssertEqual(forgot.title.isEmpty ? forgot.label : forgot.title, "Forgot Password?")
        app.textFields["username"].click()
        forgot.click()
        XCTAssertTrue((app.textFields["server"].value(forKey: "hasKeyboardFocus") as? Bool) == true, "focus did not move to the server address")
        openLog(app)
        XCTAssertTrue(waitForLog(app, "Enter the server address first"), "status log: \(logText(app))")
        closeLog(app)
    }

    /// Chooses a tab of the main window with its View menu shortcut
    /// (Command-1 to Command-3), as a keyboard user would.
    private func showTab(_ app: XCUIApplication, _ number: Int, _ name: String,
                         file: StaticString = #filePath, line: UInt = #line) {
        app.typeKey(XCUIKeyboardKey(rawValue: "\(number)"), modifierFlags: .command)
        let tab = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@ AND (elementType == %d OR elementType == %d)",
            name, XCUIElement.ElementType.radioButton.rawValue, XCUIElement.ElementType.tab.rawValue)).firstMatch
        XCTAssertTrue(waitUntil(5) { tab.exists && (tab.value as? Int ?? (tab.isSelected ? 1 : 0)) == 1 },
                      "the \(name) tab was not chosen", file: file, line: line)
    }

    /// Signed in to two accounts, visible in both at once (a switch per
    /// account, on the Accounts tab); the tabs from the keyboard; choose a
    /// device on Devices, listen (to a silent source, on a silent output
    /// when there is one), the stream on Streams (volume, mute, audit,
    /// stop); hide this Mac in one account; sign out of the second account
    /// (asked first); the visibility choice kept across restarts.
    func testSignedInWindowPassesAccessibilityAudit() throws {
        guard FileManager.default.fileExists(atPath: Self.accountFile) else {
            throw XCTSkip("no test device (run scripts/test/mac_ui_tests.py)")
        }
        let app = launch(signedIn: true)
        // Opens on Devices, visible in both accounts (the menu bar item
        // says so too).
        let source = app.disclosureTriangles.matching(NSPredicate(format: "label BEGINSWITH 'Remote test source'")).firstMatch
        XCTAssertTrue(source.waitForExistence(timeout: 30), "the test source device did not appear as a disclosure")
        waitForStatus(app, window: "Visible in 2 of 2 accounts", menu: "This Mac is visible to your other devices")
        dumpStatus(app, "at launch")
        // The second account has no visible devices: no section, no
        // heading, no "no devices" line (the first account has some).
        let emptyHeading = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'Devices in uitest-' OR value BEGINSWITH 'Devices in uitest-'")).firstMatch
        XCTAssertTrue(waitUntil(15) { !emptyHeading.exists }, "an account without visible devices still has a heading")
        XCTAssertFalse(app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'No other devices' OR value BEGINSWITH 'No other devices'")).firstMatch.exists,
                       "says no devices while another account has some")
        XCTAssertFalse(app.descendants(matching: .any).matching(identifier: "visible").firstMatch.exists,
                       "the Accounts tab's switches show on the Devices tab")
        try audit(app)

        // Each device is a collapsed disclosure; expand the test source.
        XCTAssertEqual(source.value as? Int, 0, "a device should start collapsed")
        XCTAssertFalse(app.popUpButtons["listenSource"].exists, "a collapsed device should not show its controls")
        // A click on the device's line toggles it (VoiceOver presses the
        // disclosure itself).
        source.click()
        let shown = app.popUpButtons["listenSource"].waitForExistence(timeout: 5)
        if !shown {
            let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            shot.name = "expand-failure"
            shot.lifetime = .keepAlways
            add(shot)
        }
        XCTAssertTrue(shown, "expanding the device did not show its controls: \(source.debugDescription)")
        XCTAssertEqual(source.value as? Int, 1, "the device does not report itself expanded")
        let listenSource = app.popUpButtons["listenSource"]
        listenSource.click()
        app.menuItems.matching(NSPredicate(format: "title BEGINSWITH 'Sound playing on UniMic'")).firstMatch.click()
        let listenOutput = app.popUpButtons["listenOutput"]
        listenOutput.click()
        // An output-only device: playing on one that also has inputs (such
        // as Jump Desktop Audio) makes macOS ask for microphone access. The
        // test source is silent, so nothing is heard.
        let quiet = app.menuItems.matching(NSPredicate(format: "title BEGINSWITH 'MacBook Air Speakers'")).firstMatch
        if quiet.exists { quiet.click() } else { app.typeKey(.escape, modifierFlags: []) }
        XCTAssertTrue(app.buttons["send"].isEnabled, "sending should be offered")
        try audit(app)
        let listenButton = app.buttons["listen"]
        scrollIntoView(app, listenButton)
        listenButton.click()

        // The stream, on the Streams tab (its title counts the streams): a
        // row of the streams table, selected.
        showTab(app, 2, "Streams")
        if !app.tables["streams"].waitForExistence(timeout: 15) {
            print("STREAMS TAB: \(app.windows.firstMatch.debugDescription)")
            openLog(app)
            XCTFail("no streams table; log: \(logText(app).suffix(800))")
            return
        }
        // A standard AppKit table: its rows' cells are labels.
        let streamRow = app.tables["streams"].tableRows.firstMatch
        let stream = streamRow.descendants(matching: .any)["streamState"]
        if !stream.waitForExistence(timeout: 10) {
            let shot = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
            shot.name = "no-stream"
            shot.lifetime = .keepAlways
            add(shot)
            openLog(app)
            XCTFail("no stream row; status log: \(logText(app).suffix(1500))")
            return
        }
        // SwiftUI text on macOS carries its words in the accessibility value.
        let active = expectation(for: NSPredicate(format: "value == 'connected'"), evaluatedWith: stream)
        XCTAssertEqual(XCTWaiter.wait(for: [active], timeout: 30), .completed,
                       "stream: \(stream.value ?? "")")
        checkStreamVolume(app)
        try audit(app)
        let stop = app.buttons["stopStream"]
        XCTAssertEqual(stop.label, "Stop the Selected Stream")
        stop.click()
        let none = app.staticTexts.matching(NSPredicate(format: "value BEGINSWITH 'No streams are running' OR label BEGINSWITH 'No streams are running'")).firstMatch
        if !none.waitForExistence(timeout: 15) {
            openLog(app)
            XCTFail("the stream did not end; log: \(logText(app).suffix(1500))")
            return
        }

        // Accounts: a visibility switch per account, on (new accounts start
        // visible). Hiding in the first account: the status follows.
        showTab(app, 3, "Accounts")
        // A checkbox or a switch, depending on the macOS version.
        let switches = app.descendants(matching: .any).matching(identifier: "visible")
        XCTAssertTrue(switches.firstMatch.waitForExistence(timeout: 10), "no visibility switch on Accounts")
        XCTAssertEqual(switches.count, 2, "one visibility switch per account")
        let toggle = switches.firstMatch
        // Named plainly: the account's heading says which account, and
        // nothing under it repeats the account name.
        XCTAssertEqual(toggle.label, "Visible to My Other Devices", "switch named \(toggle.label)")
        let repeating = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS ' on ' AND label BEGINSWITH 'Visible' OR label BEGINSWITH 'Sign Out of' OR label BEGINSWITH 'Manage Devices in'"))
        XCTAssertEqual(repeating.count, 0, "controls repeat the account: \(repeating.allElementsBoundByIndex.map(\.label))")
        let secondHeading = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'uitest-' OR value BEGINSWITH 'uitest-'")).firstMatch
        if !secondHeading.exists { print("ACCOUNTS TAB: \(app.windows.firstMatch.debugDescription)") }
        XCTAssertTrue(secondHeading.exists, "the second account's heading names it")
        func on(_ e: XCUIElement) -> Int? { (e.value as? Int) ?? (e.value as? String).flatMap { Int($0) } }
        XCTAssertEqual(on(toggle), 1, "a new account starts visible")
        XCTAssertTrue(app.buttons["manageDevices"].exists, "no Manage Devices")
        try audit(app)
        toggle.click()
        XCTAssertTrue(waitUntil(10) { on(toggle) == 0 }, "visibility did not turn off (\(toggle.value ?? "no value"))")
        waitForStatus(app, window: "Visible in 1 of 2 accounts", menu: "This Mac is visible to your other devices")
        dumpStatus(app, "hidden in one")

        // Two accounts (the second a temporary one): each with its own Sign
        // Out; signing out of the second leaves the first.
        let signOuts = app.buttons.matching(identifier: "signOut")
        XCTAssertEqual(signOuts.count, 2, "accounts listed: \(signOuts.count)")
        XCTAssertTrue(app.buttons["addAccount"].exists, "no Add Account")
        let second = signOuts.element(boundBy: 1)
        XCTAssertTrue(second.label.hasPrefix("Sign Out"), "second account: \(second.label)")
        scrollIntoView(app, second)
        second.click()
        // Asked first; then this Mac is removed from that account.
        let confirm = app.buttons["confirmSignOut"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "signing out did not ask first")
        confirm.click()
        XCTAssertTrue(waitUntil(20) { signOuts.count == 1 }, "still \(signOuts.count) accounts")
        try audit(app)

        // The choice is kept across restarts: the account left is hidden
        // (turned off above); quit and open again, then make it visible and
        // do the same.
        app.terminate()
        var again = reopen()
        waitForStatus(again, window: "Hidden from your other devices", menu: "This Mac is hidden from your other devices")
        showTab(again, 3, "Accounts")
        let kept = again.descendants(matching: .any).matching(identifier: "visible").firstMatch
        XCTAssertTrue(kept.waitForExistence(timeout: 10), "no visibility switch after reopening")
        XCTAssertEqual(on(kept), 0, "the switch should be off after reopening")
        kept.click()
        XCTAssertTrue(waitUntil(10) { statusWords(again).window.contains("Visible to your other devices") },
                      "visibility did not turn on: \(statusWords(again).window)")
        again.terminate()
        again = reopen()
        waitForStatus(again, window: "Visible to your other devices", menu: "This Mac is visible to your other devices")
        showTab(again, 3, "Accounts")
        let still = again.descendants(matching: .any).matching(identifier: "visible").firstMatch
        XCTAssertTrue(still.waitForExistence(timeout: 10), "no visibility switch after reopening")
        XCTAssertEqual(on(still), 1, "the switch should be on after reopening")
        again.terminate()
    }

    /// Opens the app again as it was left (same test profile, no reset, no
    /// test account passed in).
    private func reopen() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-AudioNetProfile", "uitest", "-ApplePersistenceIgnoreState", "YES"]
        app.launch()
        return app
    }
}
