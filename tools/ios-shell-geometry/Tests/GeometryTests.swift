import XCTest
import Foundation

final class GeometryTests: XCTestCase {
    private let app = XCUIApplication()
    private var data: [String: Any] = [:]
    private var root: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("geometry-tests", isDirectory: true)
    }
    override func setUp() { continueAfterFailure = false; executionTimeAllowance = 150 }
    private func number(_ d: [String: Any], _ key: String) -> CGFloat { CGFloat((d[key] as? NSNumber)?.doubleValue ?? -10000) }
    private func rect(_ d: [String: Any]) -> CGRect { CGRect(x: number(d,"x"), y: number(d,"y"), width: number(d,"width"), height: number(d,"height")) }
    private func object(_ d: [String: Any], _ key: String) -> [String: Any] { d[key] as? [String: Any] ?? [:] }

    private func refresh(_ condition: @escaping ([String: Any]) -> Bool) {
        let label = app.staticTexts["Geometry metrics"]
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            guard label.exists, let text = label.value as? String,
                  let bytes = text.data(using: .utf8), let parsed = try? JSONSerialization.jsonObject(with: bytes) as? [String: Any], condition(parsed) else { return false }
            let window = self.object(self.object(parsed,"native"),"window"), dom = self.object(parsed,"dom")
            guard abs(self.number(window,"width") - self.number(dom,"innerWidth")) < 1 else { return false }
            self.data = parsed
            return true
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
    }

    private func portrait(_ arguments: [String]) {
        XCUIDevice.shared.orientation = .portrait
        app.launchArguments = arguments
        app.launch()
        refresh { value in
            let window = self.object(self.object(value,"native"),"window")
            return self.number(window,"height") > self.number(window,"width")
        }
    }

    private func capture(_ name: String) {
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        if let bytes = try? JSONSerialization.data(withJSONObject: data, options: [.prettyPrinted, .sortedKeys]) {
            try? bytes.write(to: root.appendingPathComponent(name + ".json"), options: .atomic)
            let attachment = XCTAttachment(data: bytes, uniformTypeIdentifier: "public.json")
            attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
        }
        let screenshot = XCUIScreen.main.screenshot()
        try? screenshot.pngRepresentation.write(to: root.appendingPathComponent(name + ".png"), options: .atomic)
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = name; attachment.lifetime = .keepAlways; add(attachment)
        try? app.debugDescription.write(to: root.appendingPathComponent(name + "-accessibility.txt"), atomically: true, encoding: .utf8)
    }

    private func safeFrame() -> CGRect {
        let native = object(data,"native"), window = object(native,"window"), i = object(native,"safe_insets")
        return CGRect(x: number(i,"left"), y: number(i,"top"),
                      width: number(window,"width") - number(i,"left") - number(i,"right"),
                      height: number(window,"height") - number(i,"top") - number(i,"bottom"))
    }

    private func assertChrome() {
        let native = object(data,"native"), insets = object(native,"safe_insets"), adjusted = object(native,"adjusted_content_insets")
        let dom = object(data,"dom"), padding = object(dom,"padding")
        for edge in ["top","right","bottom","left"] {
            XCTAssertEqual(number(adjusted,edge), 0, accuracy: 0.75, "Automatic WebKit inset remained at \(edge)")
            XCTAssertEqual(number(padding,edge), number(insets,edge), accuracy: 0.75, "Source CSS did not apply the native \(edge) inset exactly once")
        }
        let shell = rect(object(dom,"shell")), window = rect(object(native,"window"))
        XCTAssertEqual(shell.width, window.width, accuracy: 0.75)
        XCTAssertEqual(shell.height, window.height, accuracy: 0.75)
    }

    private func openMenu(_ name: String) {
        let tools = app.buttons["Tools"]
        XCTAssertTrue(tools.waitForExistence(timeout: 10))
        XCTAssertTrue(safeFrame().insetBy(dx: -0.75, dy: -0.75).contains(tools.frame))
        XCTAssertGreaterThanOrEqual(tools.frame.width, 44)
        XCTAssertGreaterThanOrEqual(tools.frame.height, 44)
        XCTAssertTrue(tools.isHittable)
        tools.tap()
        refresh { self.object(self.object($0,"dom"),"menu")["visible"] as? Bool == true }
        capture(name)
        let menu = object(object(data,"dom"),"menu")
        XCTAssertTrue(safeFrame().insetBy(dx: -0.75, dy: -0.75).contains(rect(menu)), "The popup must fit all four safe edges")
        let navigation = rect(object(object(data,"dom"),"navigation"))
        XCTAssertLessThanOrEqual(rect(menu).maxY, navigation.minY - 1, "The popup must stay above the actual navigation height, including larger text")
        XCTAssertLessThanOrEqual(number(menu,"scrollWidth"), number(menu,"clientWidth") + 1, "Popup content must not require horizontal scrolling")
        for label in ["Translate", "History"] {
            let action = app.buttons[label]
            XCTAssertTrue(rect(menu).insetBy(dx: -0.75, dy: -0.75).contains(action.frame))
            XCTAssertGreaterThanOrEqual(action.frame.height, 44)
            XCTAssertTrue(action.isHittable)
        }
    }

    func testHistoricalAutomaticInsetReproducesOffscreenNavigation() {
        portrait(["--baseline"])
        capture("baseline-portrait")
        let tools = app.buttons["Tools"]
        XCTAssertTrue(tools.exists)
        XCTAssertGreaterThanOrEqual(tools.frame.minY, app.frame.height)
        XCTAssertGreaterThan(number(object(object(data,"native"),"adjusted_content_insets"),"top"), 0)
        // The offscreen control is intentional in this historical negative control.
        // Do not ask XCTest for its invalid activation point.
    }

    func testCurrentPortraitAndLandscapeInsets() {
        portrait([])
        assertChrome()
        openMenu("current-portrait-menu")
        XCUIDevice.shared.orientation = .landscapeLeft
        refresh { value in
            let window = self.object(self.object(value,"native"),"window")
            return self.number(window,"width") > self.number(window,"height")
        }
        capture("current-landscape")
        assertChrome()
        let dom = object(data,"dom"), sidebar = rect(object(dom,"desktopNavigation"))
        XCTAssertTrue(safeFrame().insetBy(dx: -0.75, dy: -0.75).contains(sidebar))
        XCTAssertEqual(sidebar.width, 64, accuracy: 0.75)
        XCTAssertEqual(number(object(dom,"navigation"),"width"), 0, accuracy: 0.75)
        XCTAssertFalse(object(dom,"menu")["visible"] as? Bool ?? true)
    }

    func testCurrentMenuWithNativeMinimumFontSize() {
        portrait(["--large-text"])
        XCTAssertEqual(number(data,"minimum_font_points"), 28)
        assertChrome()
        openMenu("current-large-text-menu")
    }
}
