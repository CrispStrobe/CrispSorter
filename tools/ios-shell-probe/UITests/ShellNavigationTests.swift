import XCTest
import Foundation

final class ShellNavigationTests: XCTestCase {
    private let bundleID = "com.crispstrobe.crispsorter"
    private var evidence: [String: Any] = [
        "schema_version": 1,
        "kind": "ios-ocr-shell-navigation",
        "status": "failed",
        "scope": "Accessibility frames, Tools menu, and OCR workspace reachability in the retained simulator application. No recognition or physical-device qualification.",
        "snapshots": [] as [[String: Any]],
    ]

    private var directory: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("ocr-shell-probe", isDirectory: true)
    }

    private func rect(_ value: CGRect) -> [String: Double] {
        ["x": value.origin.x, "y": value.origin.y,
         "width": value.width, "height": value.height].mapValues { Double($0) }
    }

    private func state(_ element: XCUIElement, screen: CGRect) -> [String: Any] {
        guard element.exists else { return ["exists": false] }
        let frame = element.frame
        let inside = !frame.isEmpty && screen.insetBy(dx: -0.5, dy: -0.5).contains(frame)
        var result: [String: Any] = [
            "exists": true, "label": element.label, "identifier": element.identifier,
            "enabled": element.isEnabled,
            "frame": rect(frame),
            "inside_application_frame": inside,
            "hittability_checked": inside,
        ]
        // XCTest can fail the test itself when asked for the activation point
        // of an offscreen WebKit element. Its frame is already a strict failure.
        if inside { result["hittable"] = element.isHittable }
        return result
    }

    private func record(_ app: XCUIApplication, _ name: String) {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var snapshots = evidence["snapshots"] as? [[String: Any]] ?? []
        snapshots.append(["name": name, "application_frame": rect(app.frame)])
        evidence["snapshots"] = snapshots
        writeReport()
        let screenshot = XCUIScreen.main.screenshot()
        try? screenshot.pngRepresentation.write(to: directory.appendingPathComponent(name + ".png"), options: .atomic)
        let screenAttachment = XCTAttachment(screenshot: screenshot)
        screenAttachment.name = name
        screenAttachment.lifetime = .keepAlways
        add(screenAttachment)
        let hierarchy = app.debugDescription
        try? hierarchy.write(to: directory.appendingPathComponent(name + "-accessibility.txt"), atomically: true, encoding: .utf8)
        let treeAttachment = XCTAttachment(string: hierarchy)
        treeAttachment.name = name + "-accessibility"
        treeAttachment.lifetime = .keepAlways
        add(treeAttachment)
        // The raw tree preserves all frames without independently hit-testing
        // unrelated, offscreen controls such as the Inbox's resize handles.
    }

    private func writeReport() {
        evidence["bundle_identifier"] = bundleID
        if let data = try? JSONSerialization.data(withJSONObject: evidence, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: directory.appendingPathComponent("navigation.json"), options: .atomic)
            let attachment = XCTAttachment(data: data, uniformTypeIdentifier: "public.json")
            attachment.name = "navigation.json"
            attachment.lifetime = .keepAlways
            add(attachment)
        }
    }

    private func fail(_ app: XCUIApplication, _ message: String) {
        evidence["failure"] = message
        record(app, "failure")
        writeReport() // Preserve diagnostics before XCTest interrupts a failed test.
        XCTFail(message)
    }

    func testToolsOpensOcrWorkspace() throws {
        continueAfterFailure = false
        executionTimeAllowance = 180
        XCUIDevice.shared.orientation = .portrait
        let app = XCUIApplication(bundleIdentifier: bundleID)
        defer { record(app, "final"); writeReport() }
        app.launch()
        guard app.wait(for: .runningForeground, timeout: 30) else {
            fail(app, "The retained application did not reach the foreground.")
            return
        }
        let tools = app.buttons.matching(NSPredicate(format: "label == %@", "Tools")).firstMatch
        let toolsExists = tools.waitForExistence(timeout: 30)
        evidence["tools"] = state(tools, screen: app.frame)
        record(app, "inbox")
        guard toolsExists else {
            fail(app, "Tools is absent from the accessibility tree after application startup.")
            return
        }
        guard app.frame.insetBy(dx: -0.5, dy: -0.5).contains(tools.frame) && tools.isHittable && tools.isEnabled else {
            fail(app, "Tools exists but is not both onscreen and hittable. See its retained frame and hierarchy.")
            return
        }
        tools.tap()
        let ocr = app.buttons.matching(NSPredicate(format: "label == %@", "OCR")).firstMatch
        let ocrExists = ocr.waitForExistence(timeout: 15)
        evidence["ocr_menu_item"] = state(ocr, screen: app.frame)
        record(app, "tools-menu")
        guard ocrExists && app.frame.insetBy(dx: -0.5, dy: -0.5).contains(ocr.frame) && ocr.isHittable && ocr.isEnabled else {
            fail(app, "The Tools menu did not expose a hittable OCR command.")
            return
        }
        ocr.tap()
        let project = app.textFields.matching(NSPredicate(format: "label == %@", "Project name")).firstMatch
        let addDocuments = app.buttons.matching(NSPredicate(format: "label == %@", "Add documents")).firstMatch
        let projectExists = project.waitForExistence(timeout: 30)
        let addExists = addDocuments.waitForExistence(timeout: 15)
        if addExists {
            let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in addDocuments.exists && addDocuments.isEnabled }, object: nil)
            _ = XCTWaiter.wait(for: [ready], timeout: 15)
        }
        evidence["project_name"] = state(project, screen: app.frame)
        evidence["add_documents"] = state(addDocuments, screen: app.frame)
        record(app, "ocr-workspace")
        guard projectExists && addExists && app.frame.insetBy(dx: -0.5, dy: -0.5).contains(addDocuments.frame) && addDocuments.isHittable && addDocuments.isEnabled else {
            fail(app, "OCR navigation did not reveal the project name and reachable Add documents control.")
            return
        }
        evidence["status"] = "passed"
    }
}
