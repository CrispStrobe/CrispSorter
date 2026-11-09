import Foundation
import XCTest
@testable import tauri_plugin_mobile_provider

final class ExportAccessTests: XCTestCase {
  private let original = Data([97, 0, 98, 255, 10, 42])

  private func isolated(_ action: (URL, URL) throws -> Void) throws {
    let owned = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    let cache = owned.appendingPathComponent("Caches", isDirectory: true)
    try FileManager.default.createDirectory(at: cache, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: owned) }
    try action(cache, owned)
  }

  private func staged(_ cache: URL, bundle: String? = nil, name: String = "Reviewed.pdf") throws -> URL {
    let base = bundle.map { cache.appendingPathComponent($0, isDirectory: true) } ?? cache
    let directory = base.appendingPathComponent("ocr-exports", isDirectory: true)
      .appendingPathComponent(UUID().uuidString.lowercased(), isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let source = directory.appendingPathComponent(name)
    try original.write(to: source)
    return source
  }

  func testPreparedFilesInBothTauriCacheLayoutsPreserveTheSource() throws {
    try isolated { cache, _ in
      for bundle in [nil, "com.crispstrobe.crispsorter"] as [String?] {
        let source = try staged(cache, bundle: bundle)
        let validated = try ExportAccess.source(path: source.path, name: source.lastPathComponent, cacheRoot: cache)
        XCTAssertEqual(validated.path, source.resolvingSymlinksInPath().standardizedFileURL.path)
        XCTAssertEqual(try Data(contentsOf: source), original)
      }
    }
  }

  func testRejectsOrdinaryFilesMismatchedNamesAndUnsafeNames() throws {
    try isolated { cache, _ in
      let source = try staged(cache)
      for name in ["", ".", "..", "../Reviewed.pdf", "bad\\name.pdf", "bad\nname.pdf", String(repeating: "x", count: 241), "Different.pdf"] {
        XCTAssertThrowsError(try ExportAccess.source(path: source.path, name: name, cacheRoot: cache))
      }
      let ordinary = cache.appendingPathComponent("ordinary.pdf")
      try original.write(to: ordinary)
      XCTAssertThrowsError(try ExportAccess.source(path: ordinary.path, name: ordinary.lastPathComponent, cacheRoot: cache))
      let missing = source.deletingLastPathComponent().appendingPathComponent("missing.pdf")
      XCTAssertThrowsError(try ExportAccess.source(path: missing.path, name: missing.lastPathComponent, cacheRoot: cache))
      let directory = source.deletingLastPathComponent().appendingPathComponent("directory.pdf", isDirectory: true)
      try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
      XCTAssertThrowsError(try ExportAccess.source(path: directory.path, name: directory.lastPathComponent, cacheRoot: cache))
      XCTAssertEqual(try Data(contentsOf: source), original)
    }
  }

  func testRejectsOutsideCacheAndPrefixLookalikePaths() throws {
    try isolated { cache, owned in
      for location in ["Elsewhere", "Caches-lookalike"] {
        let outside = try staged(owned.appendingPathComponent(location, isDirectory: true))
        XCTAssertThrowsError(try ExportAccess.source(path: outside.path, name: outside.lastPathComponent, cacheRoot: cache))
        XCTAssertEqual(try Data(contentsOf: outside), original)
      }
      let invalid = cache.appendingPathComponent("ocr-exports/not-a-uuid/Reviewed.pdf")
      try FileManager.default.createDirectory(at: invalid.deletingLastPathComponent(), withIntermediateDirectories: true)
      try original.write(to: invalid)
      XCTAssertThrowsError(try ExportAccess.source(path: invalid.path, name: invalid.lastPathComponent, cacheRoot: cache))
    }
  }

  func testRejectsSourceAndStagingRootSymlinkEscapes() throws {
    try isolated { cache, owned in
      let source = try staged(cache)
      let outside = try staged(owned.appendingPathComponent("Outside", isDirectory: true))
      try FileManager.default.removeItem(at: source)
      try FileManager.default.createSymbolicLink(at: source, withDestinationURL: outside)
      XCTAssertThrowsError(try ExportAccess.source(path: source.path, name: source.lastPathComponent, cacheRoot: cache))
      try FileManager.default.removeItem(at: cache.appendingPathComponent("ocr-exports"))
      let rootLink = cache.appendingPathComponent("ocr-exports", isDirectory: true)
      let outsideRoot = outside.deletingLastPathComponent().deletingLastPathComponent()
      try FileManager.default.createSymbolicLink(at: rootLink, withDestinationURL: outsideRoot)
      let linked = rootLink.appendingPathComponent(outside.deletingLastPathComponent().lastPathComponent).appendingPathComponent(outside.lastPathComponent)
      XCTAssertThrowsError(try ExportAccess.source(path: linked.path, name: linked.lastPathComponent, cacheRoot: cache))
      XCTAssertEqual(try Data(contentsOf: outside), original)
    }
  }

  func testRejectsOversizedFileUsingMetadataWithoutReadingItsContents() throws {
    try isolated { cache, _ in
      let source = try staged(cache)
      let handle = try FileHandle(forWritingTo: source)
      try handle.truncate(atOffset: UInt64(ExportAccess.maxBytes + 1))
      try handle.close()
      XCTAssertThrowsError(try ExportAccess.source(path: source.path, name: source.lastPathComponent, cacheRoot: cache))
      let size = try source.resourceValues(forKeys: [.fileSizeKey]).fileSize
      XCTAssertEqual(size, ExportAccess.maxBytes + 1)
    }
  }
}
