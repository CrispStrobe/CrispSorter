import Foundation

/// UIKit performs the destination copy only after the system picker accepts it.
/// This validates the source separately from security-scoped document imports.
enum ExportAccess {
  static let maxBytes = 256 * 1024 * 1024

  static func source(path: String, name: String, cacheRoot: URL? = nil) throws -> URL {
    guard !name.isEmpty, name.count <= 240, name != ".", name != "..",
          !name.contains("/"), !name.contains("\\"),
          name.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) else {
      throw ProviderError(message: "Invalid export filename.")
    }
    let cache = try cacheRoot ?? FileManager.default.url(for: .cachesDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
    let root = cache.resolvingSymlinksInPath().standardizedFileURL
    let source = URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL
    let directory = source.deletingLastPathComponent()
    let exports = directory.deletingLastPathComponent()
    // Tauri may include the bundle identifier below Caches. Keep that harmless
    // platform difference while requiring an OCR-only UUID staging directory.
    guard source.lastPathComponent == name, UUID(uuidString: directory.lastPathComponent) != nil,
          exports.lastPathComponent == "ocr-exports",
          exports.path.hasPrefix(root.path + "/") else {
      throw ProviderError(message: "Only prepared OCR export files may be saved.")
    }
    let values = try source.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
    guard values.isRegularFile == true, let size = values.fileSize, size <= maxBytes else {
      throw ProviderError(message: "Export file is unavailable or exceeds 256 MiB.")
    }
    return source
  }
}
