import Foundation
import XCTest
@testable import tauri_plugin_mobile_provider

final class ProviderAccessTests: XCTestCase {
  func testBookmarkRecordUsesExactIdentityAndRequiresReselectionWhenMissing() throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try BookmarkStore(directory: directory)
    let upper = "file:///provider/Case%20Document.txt"
    let lower = "file:///provider/case%20Document.txt"
    try store.save(identity: upper, bookmark: Data([1, 2, 3]))
    try store.save(identity: lower, bookmark: Data([4, 5]))
    XCTAssertEqual(try store.load(identity: upper), Data([1, 2, 3]))
    XCTAssertEqual(try store.load(identity: lower), Data([4, 5]))
    XCTAssertThrowsError(try store.load(identity: "file:///provider/missing.txt")) { error in
      XCTAssertTrue(error.localizedDescription.contains("Select the document again"))
    }
  }

  func testReadBoundedPreservesBinaryAndRejectsOversize() throws {
    XCTAssertEqual(try ProviderAccess.readBounded(InputStream(data: Data([97, 0, 98]))), Data([97, 0, 98]))
    let oversized = Data(repeating: 7, count: ProviderAccess.maxBytes + 1)
    XCTAssertThrowsError(try ProviderAccess.readBounded(InputStream(data: oversized))) { error in
      XCTAssertTrue(error.localizedDescription.contains("16 MiB"))
    }
  }

  func testReadRejectsAnUnselectedURL() throws {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: directory) }
    let access = ProviderAccess(bookmarks: try BookmarkStore(directory: directory))
    XCTAssertThrowsError(try access.read("file:///provider/unselected.txt"))
    XCTAssertThrowsError(try access.read("https://example.com/file.txt"))
    let corrupt = "file:///provider/corrupt.txt"
    try BookmarkStore(directory: directory).save(identity: corrupt, bookmark: Data([0, 1, 2]))
    XCTAssertThrowsError(try access.read(corrupt))
  }
}
