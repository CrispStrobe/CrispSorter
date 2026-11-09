import Foundation
import XCTest
import CoreGraphics
import CryptoKit
import ImageIO
import UniformTypeIdentifiers
@testable import tauri_plugin_mobile_provider

final class DocumentRasterizerTests: XCTestCase {
  private func fixture(_ name: String) throws -> Data {
    let path = try XCTUnwrap(Bundle.module.url(forResource: name, withExtension: nil, subdirectory: "Fixtures"))
    let data = try Data(contentsOf: path)
    return name.hasSuffix(".b64") ? try XCTUnwrap(Data(base64Encoded: data, options: .ignoreUnknownCharacters)) : data
  }
  private func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
  private func isolated(_ action: (URL) throws -> Void) throws {
    let home = FileManager.default.temporaryDirectory.appendingPathComponent("document-check-" + UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: home) }
    try action(home.resolvingSymlinksInPath().standardizedFileURL)
  }
  private func archive(_ home: URL, bytes: Data, fileExtension suffix: String) throws -> URL {
    let parent = home.appendingPathComponent("ocr-workspace/sources", isDirectory: true)
    try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true)
    let source = parent.appendingPathComponent(digest(bytes) + "." + suffix)
    try bytes.write(to: source); return source
  }
  private func staging(_ home: URL) throws -> URL {
    let output = home.appendingPathComponent("ocr-workspace/import-staging/import-" + UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true); return output
  }
  private func raster(_ path: String) throws -> CGImage {
    let source = try XCTUnwrap(CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil))
    return try XCTUnwrap(CGImageSourceCreateImageAtIndex(source, 0, nil))
  }
  private func pixels(_ image: CGImage) throws -> [UInt8] {
    var bytes = [UInt8](repeating: 0, count: image.width * image.height * 4)
    try bytes.withUnsafeMutableBytes { pointer in
      let context = try XCTUnwrap(CGContext(data: pointer.baseAddress, width: image.width, height: image.height, bitsPerComponent: 8,
        bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue))
      context.draw(image, in: CGRect(x: 0, y: 0, width: CGFloat(image.width), height: CGFloat(image.height)))
    }
    return bytes
  }

  func testPinnedFixturesMatchTheirDeclaredSources() throws {
    let metadata = try XCTUnwrap(JSONSerialization.jsonObject(with: fixture("manifest.json")) as? [String: Any])
    let pdf = try fixture("two-pages.pdf")
    XCTAssertEqual(metadata["sha256"] as? String, digest(pdf)); XCTAssertEqual(metadata["bytes"] as? Int, pdf.count)
    for group in ["images", "pdf_variants"] {
      for entry in try XCTUnwrap(metadata[group] as? [[String: Any]]) {
        let bytes = try fixture(try XCTUnwrap(entry["fixture"] as? String))
        XCTAssertEqual(entry["sha256"] as? String, digest(bytes)); XCTAssertEqual(entry["bytes"] as? Int, bytes.count)
      }
    }
  }

  func testRendersAllPagesWithInkPhysicalDimensionsAndUnchangedOriginal() throws {
    try isolated { home in
      let original = try fixture("two-pages.pdf"), source = try archive(home, bytes: original, fileExtension: "pdf"), output = try staging(home)
      let result = try DocumentRasterizer(home: home).rasterize(sourcePath: source.path, outputDir: output.path, kind: "pdf")
      XCTAssertEqual(result.pages.count, 2)
      let sizes = [(1700, 2200), (1200, 1800)], physical = [(612.0, 792.0), (432.0, 648.0)]
      var bytes = 0
      for (index, page) in result.pages.enumerated() {
        XCTAssertEqual(URL(fileURLWithPath: page.path).deletingLastPathComponent().path, output.path)
        XCTAssertEqual(page.width_points, physical[index].0); XCTAssertEqual(page.height_points, physical[index].1)
        let image = try raster(page.path); XCTAssertEqual(image.width, sizes[index].0); XCTAssertEqual(image.height, sizes[index].1)
        let rgba = try pixels(image)
        var ink = 0
        // Count all pixels: a sparse sampling grid can miss thin glyph strokes.
        for offset in stride(from: 0, to: rgba.count, by: 4) { if rgba[offset] < 120 { ink += 1 } }
        XCTAssertGreaterThan(ink, 500, "A blank page must not pass as rendered text.")
        bytes += try Data(contentsOf: URL(fileURLWithPath: page.path)).count
      }
      XCTAssertEqual(result.bytes, bytes); XCTAssertEqual(try Data(contentsOf: source), original)
      let progress = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: output.appendingPathComponent("progress.json"))) as? [String: Any])
      XCTAssertEqual(progress["completed"] as? Int, 2)
    }
  }

  func testPreservesPdfRotationAndRejectsLockedOrDamagedInput() throws {
    try isolated { home in
      let decoder = DocumentRasterizer(home: home)
      let source = try archive(home, bytes: fixture("rotated.pdf.b64"), fileExtension: "pdf")
      let result = try decoder.rasterize(sourcePath: source.path, outputDir: staging(home).path, kind: "pdf")
      XCTAssertEqual(result.pages.first?.width_points, 792); XCTAssertEqual(result.pages.first?.height_points, 612)
      let locked = try archive(home, bytes: fixture("locked.pdf.b64"), fileExtension: "pdf")
      XCTAssertThrowsError(try decoder.rasterize(sourcePath: locked.path, outputDir: staging(home).path, kind: "pdf")) {
        XCTAssertTrue($0.localizedDescription.contains("password protected"), $0.localizedDescription)
      }
      let damaged = try archive(home, bytes: Data("%PDF-1.7 broken input".utf8), fileExtension: "pdf")
      XCTAssertThrowsError(try decoder.rasterize(sourcePath: damaged.path, outputDir: staging(home).path, kind: "pdf"))
    }
  }

  func testBakesExifOrientationIntoThePixelsBeforeRecognition() throws {
    try isolated { home in
      let original = try fixture("oriented.jpg.b64"), source = try archive(home, bytes: original, fileExtension: "jpg")
      let result = try DocumentRasterizer(home: home).rasterize(sourcePath: source.path, outputDir: staging(home).path, kind: "image")
      let page = try XCTUnwrap(result.pages.first)
      XCTAssertEqual(page.width, 80); XCTAssertEqual(page.height, 120)
      let image = try raster(page.path), rgba = try pixels(image), offset = (10 * image.width + 10) * 4
      XCTAssertGreaterThan(rgba[offset + 2], 180); XCTAssertLessThan(rgba[offset], 70)
      XCTAssertEqual(try Data(contentsOf: source), original)
    }
  }

  func testDecodesPinnedHeicWhenImageIOAdvertisesIt() throws {
    try XCTSkipUnless(DocumentRasterizer.capabilities().image_formats.contains("heic"), "This runtime does not advertise a HEIC decoder.")
    try isolated { home in
      let original = try fixture("colors.heic.b64"), source = try archive(home, bytes: original, fileExtension: "heic")
      let page = try XCTUnwrap(DocumentRasterizer(home: home).rasterize(sourcePath: source.path, outputDir: staging(home).path, kind: "image").pages.first)
      XCTAssertEqual(page.width, 120); XCTAssertEqual(page.height, 80)
      let image = try raster(page.path), rgba = try pixels(image)
      let red = (10 * image.width + 10) * 4, blue = (70 * image.width + 10) * 4
      XCTAssertGreaterThan(rgba[red], 180); XCTAssertLessThan(rgba[red + 2], 70)
      XCTAssertGreaterThan(rgba[blue + 2], 180); XCTAssertLessThan(rgba[blue], 70)
      XCTAssertEqual(try Data(contentsOf: source), original)
    }
  }

  func testDecodesAvifOnlyWhenImageIOActuallyAdvertisesIt() throws {
    try XCTSkipUnless(DocumentRasterizer.capabilities().image_formats.contains("avif"), "This runtime does not advertise an AVIF decoder.")
    try isolated { home in
      let original = try fixture("colors.avif.b64"), source = try archive(home, bytes: original, fileExtension: "avif")
      let result = try DocumentRasterizer(home: home).rasterize(sourcePath: source.path, outputDir: staging(home).path, kind: "image")
      let page = try XCTUnwrap(result.pages.first)
      XCTAssertEqual(page.width, 120); XCTAssertEqual(page.height, 80)
      let image = try raster(page.path), rgba = try pixels(image), offset = (10 * image.width + 10) * 4
      XCTAssertGreaterThan(rgba[offset], 180); XCTAssertLessThan(rgba[offset + 2], 70)
      XCTAssertEqual(try Data(contentsOf: source), original)
    }
  }

  func testNativeHeicRoundTripWhenEncoderAndDecoderAreAvailable() throws {
    let outputTypes = CGImageDestinationCopyTypeIdentifiers() as! [String]
    try XCTSkipUnless(DocumentRasterizer.capabilities().image_formats.contains("heic") && outputTypes.contains(UTType.heic.identifier), "This runtime cannot produce and decode HEIC fixtures.")
    try isolated { home in
      let jpeg = try fixture("oriented.jpg.b64")
      let source = try XCTUnwrap(CGImageSourceCreateWithData(jpeg as CFData, nil))
      let image = try XCTUnwrap(CGImageSourceCreateImageAtIndex(source, 0, nil))
      let encoded = NSMutableData()
      let destination = try XCTUnwrap(CGImageDestinationCreateWithData(encoded as CFMutableData, UTType.heic.identifier as CFString, 1, nil))
      CGImageDestinationAddImage(destination, image, [kCGImagePropertyOrientation: 6] as CFDictionary)
      try XCTSkipUnless(CGImageDestinationFinalize(destination), "HEIC encoding is unavailable in this simulator.")
      let original = encoded as Data, archived = try archive(home, bytes: original, fileExtension: "heic")
      let result = try DocumentRasterizer(home: home).rasterize(sourcePath: archived.path, outputDir: staging(home).path, kind: "image")
      let page = try XCTUnwrap(result.pages.first)
      XCTAssertEqual(page.width, 80); XCTAssertEqual(page.height, 120)
      XCTAssertEqual(try Data(contentsOf: archived), original)
    }
  }

  func testRejectsIncompleteImagesInsteadOfImportingPartialPixels() throws {
    try isolated { home in
      let complete = try fixture("oriented.jpg.b64")
      let source = try archive(home, bytes: Data(complete.dropLast(100)), fileExtension: "jpg")
      XCTAssertThrowsError(try DocumentRasterizer(home: home).rasterize(sourcePath: source.path, outputDir: staging(home).path, kind: "image"))
      // An apparent end marker in an APP segment must not conceal a truncated scan.
      let falseEnd = complete.prefix(2) + Data([0xff, 0xef, 0x00, 0x04, 0xff, 0xd9]) + complete.dropFirst(2).dropLast(100)
      let metadataEnd = try archive(home, bytes: Data(falseEnd), fileExtension: "jpg")
      XCTAssertThrowsError(try DocumentRasterizer(home: home).rasterize(sourcePath: metadataEnd.path, outputDir: staging(home).path, kind: "image"))
      // A complete primary JPEG remains valid with bytes following its end marker.
      let trailing = try archive(home, bytes: complete + Data("source trailer".utf8), fileExtension: "jpg")
      XCTAssertEqual(try DocumentRasterizer(home: home).rasterize(sourcePath: trailing.path, outputDir: staging(home).path, kind: "image").pages.count, 1)
      let avif = try fixture("colors.avif.b64")
      let truncatedBox = try archive(home, bytes: Data(avif.dropLast()), fileExtension: "avif")
      XCTAssertThrowsError(try DocumentRasterizer(home: home).rasterize(sourcePath: truncatedBox.path, outputDir: staging(home).path, kind: "image"))
    }
  }

  func testCancellationBeforeOpeningAndBetweenPagesNeverReturnsAPartialDocument() throws {
    try isolated { home in
      let original = try fixture("two-pages.pdf"), source = try archive(home, bytes: original, fileExtension: "pdf"), stopped = try staging(home)
      try Data().write(to: stopped.appendingPathComponent("cancel"))
      XCTAssertThrowsError(try DocumentRasterizer(home: home).rasterize(sourcePath: source.path, outputDir: stopped.path, kind: "pdf")) {
        XCTAssertTrue($0.localizedDescription.contains("OCR_IMPORT_CANCELLED"))
      }
      let output = try staging(home)
      let decoder = DocumentRasterizer(home: home) { completed, _ in
        if completed == 1 { try! Data().write(to: output.appendingPathComponent("cancel")) }
      }
      XCTAssertThrowsError(try decoder.rasterize(sourcePath: source.path, outputDir: output.path, kind: "pdf")) {
        XCTAssertTrue($0.localizedDescription.contains("OCR_IMPORT_CANCELLED"))
      }
      let rendered = try FileManager.default.contentsOfDirectory(at: output, includingPropertiesForKeys: nil).filter { $0.pathExtension == "png" }
      XCTAssertEqual(rendered.count, 1); XCTAssertEqual(try Data(contentsOf: source), original)
    }
  }

  func testRejectsPathsOutsideStagingSymlinksReusedDirectoriesAndOversizedSources() throws {
    try isolated { home in
      let original = try fixture("two-pages.pdf"), source = try archive(home, bytes: original, fileExtension: "pdf"), decoder = DocumentRasterizer(home: home)
      let outside = home.appendingPathComponent("outside.pdf"); try original.write(to: outside)
      XCTAssertThrowsError(try decoder.rasterize(sourcePath: outside.path, outputDir: staging(home).path, kind: "pdf"))
      let alias = source.deletingLastPathComponent().appendingPathComponent(String(repeating: "f", count: 64) + ".pdf")
      try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: outside)
      XCTAssertThrowsError(try decoder.rasterize(sourcePath: alias.path, outputDir: staging(home).path, kind: "pdf"))
      let used = try staging(home); try Data([1]).write(to: used.appendingPathComponent("keep-me.txt"))
      XCTAssertThrowsError(try decoder.rasterize(sourcePath: source.path, outputDir: used.path, kind: "pdf"))
      XCTAssertTrue(FileManager.default.fileExists(atPath: used.appendingPathComponent("keep-me.txt").path))
      let file = try FileHandle(forWritingTo: source); try file.truncate(atOffset: UInt64(DocumentRasterizer.maxSourceBytes + 1)); try file.close()
      XCTAssertThrowsError(try decoder.rasterize(sourcePath: source.path, outputDir: staging(home).path, kind: "pdf")) {
        XCTAssertTrue($0.localizedDescription.contains("32 MiB"))
      }
    }
  }

  func testCapsRasterMemoryAndRejectsDegenerateDimensions() throws {
    for (width, height) in [(14400.0, 14400.0), (100000.0, 1.0), (10000.0, 10000.0)] {
      let (w, h) = try DocumentRasterizer.boundedSize(width: width, height: height, preferredScale: 200 / 72)
      XCTAssertGreaterThan(w, 0); XCTAssertGreaterThan(h, 0); XCTAssertLessThanOrEqual(w * h, DocumentRasterizer.maxPagePixels)
    }
    XCTAssertThrowsError(try DocumentRasterizer.boundedSize(width: .nan, height: 42, preferredScale: 1))
  }
}
