import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

struct DocumentCapabilities: Encodable {
  let pdf = true
  let image_formats: [String]
  let max_source_bytes = DocumentRasterizer.maxSourceBytes
  let max_page_pixels = DocumentRasterizer.maxPagePixels
  let max_pages = DocumentRasterizer.maxPages
}

struct RasterizedPage: Encodable {
  let path: String
  let width: Int
  let height: Int
  let width_points: Double
  let height_points: Double
  let warnings: [String]
}

struct RasterizedDocument: Encodable {
  let pages: [RasterizedPage]
  let bytes: Int
  let decoder: String
}

private struct RasterizerError: LocalizedError {
  let message: String
  var errorDescription: String? { message }
}

/// Reads only archived sources and writes one raster at a time into private, empty staging.
struct DocumentRasterizer {
  static let maxSourceBytes = 32 * 1024 * 1024
  static let maxPagePixels = 16_000_000
  static let maxSourcePixels = 100_000_000
  static let maxOutputBytes = 512 * 1024 * 1024
  static let maxPages = 10_000
  static let pdfDpi = 200.0
  let home: URL
  let onProgress: ((Int, Int) -> Void)?

  init(home: URL = URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true), onProgress: ((Int, Int) -> Void)? = nil) {
    self.home = home.resolvingSymlinksInPath().standardizedFileURL
    self.onProgress = onProgress
  }

  static func capabilities() -> DocumentCapabilities {
    let types = Set(CGImageSourceCopyTypeIdentifiers() as! [String])
    let candidates = [("heic", "public.heic"), ("heif", "public.heif"), ("avif", "public.avif")]
    return DocumentCapabilities(image_formats: candidates.compactMap { types.contains($0.1) ? $0.0 : nil })
  }

  static func boundedSize(width: Double, height: Double, preferredScale: Double) throws -> (Int, Int) {
    guard width.isFinite, height.isFinite, width > 0, height > 0 else { throw RasterizerError(message: "Invalid page dimensions.") }
    let scale = min(preferredScale, min(16384 / max(width, height), sqrt(Double(maxPagePixels) / width / height)))
    let w = max(1, Int(floor(width * scale))), h = max(1, Int(floor(height * scale)))
    guard w * h <= maxPagePixels else { throw RasterizerError(message: "Page exceeds the raster pixel limit.") }
    return (w, h)
  }

  private func paths(sourcePath: String, outputDir: String) throws -> (URL, URL) {
    let source = URL(fileURLWithPath: sourcePath).standardizedFileURL
    let output = URL(fileURLWithPath: outputDir, isDirectory: true).standardizedFileURL
    guard sourcePath == source.path, outputDir == output.path,
      source == source.resolvingSymlinksInPath(), output == output.resolvingSymlinksInPath(),
      source.path.hasPrefix(home.path + "/"), output.path.hasPrefix(home.path + "/") else {
      throw RasterizerError(message: "OCR import paths must be canonical private paths.")
    }
    let workspace = source.deletingLastPathComponent().deletingLastPathComponent()
    guard workspace.lastPathComponent == "ocr-workspace", source.deletingLastPathComponent().lastPathComponent == "sources",
      source.lastPathComponent.range(of: "^[0-9a-f]{64}\\.(pdf|heic|heif|avif|png|jpe?g)$", options: .regularExpression) != nil,
      output.deletingLastPathComponent() == workspace.appendingPathComponent("import-staging", isDirectory: true),
      output.lastPathComponent.range(of: "^import-[A-Za-z0-9_-]{6,100}$", options: .regularExpression) != nil else {
      throw RasterizerError(message: "Invalid archived OCR source or import staging directory.")
    }
    let sourceInfo = try source.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
    guard sourceInfo.isRegularFile == true, let size = sourceInfo.fileSize, size > 0, size <= Self.maxSourceBytes else {
      throw RasterizerError(message: "This source exceeds the 32 MiB mobile import limit.")
    }
    let entries = try FileManager.default.contentsOfDirectory(at: output, includingPropertiesForKeys: [.isRegularFileKey])
    guard entries.allSatisfy({ $0.lastPathComponent == "cancel" && $0 == $0.resolvingSymlinksInPath() &&
      (try? $0.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true }) else {
      throw RasterizerError(message: "The import staging directory is already in use.")
    }
    return (source, output)
  }

  private func checkCancelled(_ output: URL) throws {
    if FileManager.default.fileExists(atPath: output.appendingPathComponent("cancel").path) {
      throw RasterizerError(message: "OCR_IMPORT_CANCELLED")
    }
  }

  private func progress(_ output: URL, completed: Int, total: Int) throws {
    try checkCancelled(output)
    let data = try JSONSerialization.data(withJSONObject: ["phase": "rendering", "completed": completed, "total": total])
    try data.write(to: output.appendingPathComponent("progress.json"), options: .atomic)
    onProgress?(completed, total)
  }

  private func context(width: Int, height: Int) throws -> CGContext {
    guard let space = CGColorSpace(name: CGColorSpace.sRGB),
      let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: width * 4,
        space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue) else {
      throw RasterizerError(message: "This page needs more memory than is available. Reduce the photo resolution and retry.")
    }
    context.setFillColor(CGColor(gray: 1, alpha: 1)); context.fill(CGRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height)))
    return context
  }

  private func write(_ image: CGImage, output: URL, index: Int, bytesWritten: Int) throws -> (URL, Int) {
    try checkCancelled(output)
    let target = output.appendingPathComponent(String(format: "page-%06d.png", index + 1))
    let writer = try BoundedRasterWriter(url: target, output: output, limit: Self.maxOutputBytes - bytesWritten)
    defer { try? writer.handle.close() }
    var callbacks = CGDataConsumerCallbacks(putBytes: { pointer, bytes, count in
      guard let pointer else { return 0 }
      return Unmanaged<BoundedRasterWriter>.fromOpaque(pointer).takeUnretainedValue().write(bytes, count: count)
    }, releaseConsumer: nil)
    guard let consumer = CGDataConsumer(info: Unmanaged.passUnretained(writer).toOpaque(), cbks: &callbacks),
      let destination = CGImageDestinationCreateWithDataConsumer(consumer, UTType.png.identifier as CFString, 1, nil) else {
      throw RasterizerError(message: "Could not create the page encoder.")
    }
    CGImageDestinationAddImage(destination, image, nil)
    let success = CGImageDestinationFinalize(destination)
    if let failure = writer.failure { throw failure }
    guard success, writer.count > 0 else { throw RasterizerError(message: "Could not encode the rendered page.") }
    try writer.handle.synchronize(); try checkCancelled(output)
    return (target, writer.count)
  }

  func rasterize(sourcePath: String, outputDir: String, kind: String) throws -> RasterizedDocument {
    let (source, output) = try paths(sourcePath: sourcePath, outputDir: outputDir)
    try checkCancelled(output)
    switch kind {
    case "pdf": return try pdf(source: source, output: output)
    case "image": return try image(source: source, output: output)
    default: throw RasterizerError(message: "Unsupported native document type.")
    }
  }

  private func pdf(source: URL, output: URL) throws -> RasterizedDocument {
    guard let document = CGPDFDocument(source as CFURL) else {
      throw RasterizerError(message: "This PDF could not be opened; it may be damaged or password protected. Import an unlocked, valid PDF.")
    }
    guard !document.isEncrypted || document.isUnlocked else { throw RasterizerError(message: "This PDF is password protected. Import an unlocked copy.") }
    guard (1...Self.maxPages).contains(document.numberOfPages) else { throw RasterizerError(message: "A PDF must contain 1 to \(Self.maxPages) pages.") }
    var pages = [RasterizedPage](), bytes = 0
    try progress(output, completed: 0, total: document.numberOfPages)
    for index in 0..<document.numberOfPages {
      try checkCancelled(output)
      let page: RasterizedPage = try autoreleasepool {
        guard let sourcePage = document.page(at: index + 1) else { throw RasterizerError(message: "PDF page \(index + 1) could not be opened.") }
        let box = sourcePage.getBoxRect(.cropBox).standardized
        let rotation = (sourcePage.rotationAngle % 360 + 360) % 360
        let pointsWidth = Double(rotation == 90 || rotation == 270 ? box.height : box.width)
        let pointsHeight = Double(rotation == 90 || rotation == 270 ? box.width : box.height)
        guard pointsWidth > 0, pointsHeight > 0, pointsWidth <= 14400, pointsHeight <= 14400 else {
          throw RasterizerError(message: "Page \(index + 1) has invalid or oversized physical dimensions.")
        }
        let (width, height) = try Self.boundedSize(width: pointsWidth, height: pointsHeight, preferredScale: Self.pdfDpi / 72)
        let canvas = try context(width: width, height: height)
        canvas.concatenate(sourcePage.getDrawingTransform(.cropBox, rect: CGRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height)), rotate: 0, preserveAspectRatio: true))
        canvas.drawPDFPage(sourcePage)
        guard let raster = canvas.makeImage() else { throw RasterizerError(message: "PDF page \(index + 1) could not be rendered.") }
        let (path, count) = try write(raster, output: output, index: index, bytesWritten: bytes); bytes += count
        let warnings = width < Int(floor(pointsWidth * Self.pdfDpi / 72)) ? ["This page was reduced to the mobile raster limit; check small text before recognition."] : []
        return RasterizedPage(path: path.path, width: width, height: height, width_points: pointsWidth, height_points: pointsHeight, warnings: warnings)
      }
      pages.append(page)
      try progress(output, completed: index + 1, total: document.numberOfPages)
    }
    return RasterizedDocument(pages: pages, bytes: bytes, decoder: "ios-core-graphics")
  }

  /// JPEG must end with the EOI marker (trailing padding tolerated) and PNG with
  /// the IEND chunk. Other formats rely on ImageIO's own status.
  static func hasCompleteTrailer(_ url: URL, type: String?) -> Bool {
    guard let type else { return true }
    let isJPEG = type == "public.jpeg", isPNG = type == "public.png"
    guard isJPEG || isPNG else { return true }
    guard let handle = try? FileHandle(forReadingFrom: url) else { return false }
    defer { try? handle.close() }
    guard let size = try? handle.seekToEnd(), size >= 12 else { return false }
    let window = UInt64(min(size, 4096))
    guard (try? handle.seek(toOffset: size - window)) != nil,
      let tail = try? handle.read(upToCount: Int(window)) else { return false }
    let bytes = [UInt8](tail)
    if isPNG {
      return Array(bytes.suffix(12)) == [0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82]
    }
    var end = bytes.count
    while end > 0 && (bytes[end - 1] == 0 || bytes[end - 1] == 0x0A || bytes[end - 1] == 0x0D || bytes[end - 1] == 0x20) { end -= 1 }
    return end >= 2 && bytes[end - 2] == 0xFF && bytes[end - 1] == 0xD9
  }

  private func image(source: URL, output: URL) throws -> RasterizedDocument {
    try progress(output, completed: 0, total: 1)
    guard let imageSource = CGImageSourceCreateWithURL(source as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary),
      CGImageSourceGetCount(imageSource) > 0 else { throw RasterizerError(message: "This device cannot decode this image. Export a JPEG or PNG copy.") }
    let index = CGImageSourceGetPrimaryImageIndex(imageSource)
    guard let properties = CGImageSourceCopyPropertiesAtIndex(imageSource, index, nil) as? [CFString: Any],
      let encodedWidth = properties[kCGImagePropertyPixelWidth] as? Int, let encodedHeight = properties[kCGImagePropertyPixelHeight] as? Int,
      encodedWidth > 0, encodedHeight > 0, encodedWidth <= Self.maxSourcePixels / encodedHeight else {
      throw RasterizerError(message: "The source image exceeds the 100 megapixel decode limit.")
    }
    let orientation = properties[kCGImagePropertyOrientation] as? Int ?? 1
    let swapAxes = (5...8).contains(orientation)
    let sourceWidth = swapAxes ? encodedHeight : encodedWidth, sourceHeight = swapAxes ? encodedWidth : encodedHeight
    let (targetWidth, targetHeight) = try Self.boundedSize(width: Double(sourceWidth), height: Double(sourceHeight), preferredScale: 1)
    let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true,
      kCGImageSourceCreateThumbnailWithTransform: true, kCGImageSourceThumbnailMaxPixelSize: max(targetWidth, targetHeight),
      kCGImageSourceShouldCacheImmediately: true]
    guard let thumbnail = CGImageSourceCreateThumbnailAtIndex(imageSource, index, options as CFDictionary),
      thumbnail.width > 0, thumbnail.height > 0, thumbnail.width <= Self.maxPagePixels / thumbnail.height else {
      throw RasterizerError(message: "The primary image could not be decoded within the raster limit.")
    }
    // ImageIO reports a fully present but truncated file as complete and
    // returns partially decoded pixels, so also check the container's trailer.
    guard CGImageSourceGetStatusAtIndex(imageSource, index) == .statusComplete,
      Self.hasCompleteTrailer(source, type: CGImageSourceGetType(imageSource) as String?) else {
      throw RasterizerError(message: "This image is incomplete or damaged. Import a complete JPEG, PNG, or supported phone image.")
    }
    try checkCancelled(output)
    let canvas = try context(width: thumbnail.width, height: thumbnail.height)
    canvas.draw(thumbnail, in: CGRect(x: 0, y: 0, width: CGFloat(thumbnail.width), height: CGFloat(thumbnail.height)))
    guard let raster = canvas.makeImage() else { throw RasterizerError(message: "The image could not be prepared for recognition.") }
    let (path, count) = try write(raster, output: output, index: 0, bytesWritten: 0)
    var warnings = ["Imported the primary still image. Auxiliary images and animation frames are not included."]
    if thumbnail.width < sourceWidth || thumbnail.height < sourceHeight { warnings.append("This photo was reduced to the mobile raster limit; the original file is retained.") }
    let page = RasterizedPage(path: path.path, width: raster.width, height: raster.height,
      width_points: Double(sourceWidth) * 72 / 300, height_points: Double(sourceHeight) * 72 / 300, warnings: warnings)
    try progress(output, completed: 1, total: 1)
    return RasterizedDocument(pages: [page], bytes: count, decoder: "ios-image-io")
  }
}

private final class BoundedRasterWriter {
  let handle: FileHandle
  let output: URL
  let limit: Int
  var count = 0
  var failure: Error?

  init(url: URL, output: URL, limit: Int) throws {
    guard !FileManager.default.fileExists(atPath: url.path), FileManager.default.createFile(atPath: url.path, contents: nil) else {
      throw RasterizerError(message: "The page output already exists or cannot be created.")
    }
    self.handle = try FileHandle(forWritingTo: url); self.output = output; self.limit = limit
  }

  func write(_ bytes: UnsafeRawPointer?, count length: Int) -> Int {
    guard failure == nil else { return 0 }
    do {
      guard let bytes else { throw RasterizerError(message: "The page encoder returned an empty buffer.") }
      if FileManager.default.fileExists(atPath: output.appendingPathComponent("cancel").path) { throw RasterizerError(message: "OCR_IMPORT_CANCELLED") }
      guard length >= 0, count <= limit - length else { throw RasterizerError(message: "Rendered document exceeds the 512 MiB import limit.") }
      try handle.write(contentsOf: Data(bytes: bytes, count: length)); count += length
      return length
    } catch { failure = error; return 0 }
  }
}
