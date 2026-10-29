import CryptoKit
import Foundation

struct ProviderError: LocalizedError {
  let message: String
  var errorDescription: String? { message }
}

struct ProviderFile: Codable {
  let uri: String
  let name: String
  let size: Int
}

private struct BookmarkRecord: Codable {
  let identity: String
  let bookmark: Data
}

/// The identity is the original URL; reopening always uses bookmark data,
/// never a URL reconstructed from that string.
final class BookmarkStore {
  private let directory: URL

  init(directory: URL? = nil) throws {
    if let directory = directory {
      self.directory = directory
    } else {
      let support = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
      self.directory = support.appendingPathComponent("provider-bookmarks", isDirectory: true)
    }
    try FileManager.default.createDirectory(at: self.directory, withIntermediateDirectories: true)
  }

  private func recordURL(_ identity: String) -> URL {
    let digest = SHA256.hash(data: Data(identity.utf8)).map { String(format: "%02x", $0) }.joined()
    return directory.appendingPathComponent(digest + ".json")
  }

  func save(identity: String, bookmark: Data) throws {
    let record = BookmarkRecord(identity: identity, bookmark: bookmark)
    try JSONEncoder().encode(record).write(to: recordURL(identity), options: .atomic)
  }

  func load(identity: String) throws -> Data {
    let data: Data
    do { data = try Data(contentsOf: recordURL(identity)) }
    catch { throw ProviderError(message: "The provider permission is unavailable. Select the document again.") }
    let record: BookmarkRecord
    do { record = try JSONDecoder().decode(BookmarkRecord.self, from: data) }
    catch { throw ProviderError(message: "The provider permission is invalid. Select the document again.") }
    guard record.identity == identity else { throw ProviderError(message: "Provider bookmark identity mismatch.") }
    return record.bookmark
  }
}

final class ProviderAccess {
  static let maxBytes = 16 * 1024 * 1024
  static let maxEntries = 5000
  static let maxDepth = 32
  private let bookmarks: BookmarkStore

  init(bookmarks: BookmarkStore) { self.bookmarks = bookmarks }

  private func scoped<T>(_ url: URL, _ body: () throws -> T) throws -> T {
    guard url.startAccessingSecurityScopedResource() else {
      throw ProviderError(message: "Provider access was denied or revoked. Select the document again.")
    }
    defer { url.stopAccessingSecurityScopedResource() }
    return try body()
  }

  private func coordinated<T>(_ url: URL, _ body: (URL) throws -> T) throws -> T {
    var coordinationError: NSError?
    var outcome: Result<T, Error>?
    NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &coordinationError) { coordinatedURL in
      outcome = Result { try body(coordinatedURL) }
    }
    if let error = coordinationError { throw error }
    guard let result = outcome else { throw ProviderError(message: "The provider could not coordinate access.") }
    return try result.get()
  }

  /// Called while the picker's original security-scoped URL is still alive.
  private func remember(_ url: URL, alreadyCoordinated: Bool = false) throws -> ProviderFile {
    try scoped(url) {
      let keys: Set<URLResourceKey> = [.nameKey, .fileSizeKey, .isDirectoryKey, .isSymbolicLinkKey]
      let values: URLResourceValues
      if alreadyCoordinated {
        values = try url.resourceValues(forKeys: keys)
      } else {
        values = try coordinated(url) { try $0.resourceValues(forKeys: keys) }
      }
      guard values.isDirectory == false, values.isSymbolicLink != true else {
        throw ProviderError(message: "Only regular documents can be imported.")
      }
      let bookmark = try url.bookmarkData(options: .minimalBookmark, includingResourceValuesForKeys: nil, relativeTo: nil)
      let identity = url.absoluteString
      try bookmarks.save(identity: identity, bookmark: bookmark)
      return ProviderFile(uri: identity, name: values.name ?? url.lastPathComponent, size: values.fileSize ?? -1)
    }
  }

  func files(_ urls: [URL]) throws -> [ProviderFile] {
    guard !urls.isEmpty, urls.count <= Self.maxEntries else {
      throw ProviderError(message: "Select between 1 and \(Self.maxEntries) documents.")
    }
    return try urls.map { try remember($0) }
  }

  func folder(_ root: URL) throws -> [ProviderFile] {
    try scoped(root) {
      try coordinated(root) { directory in
        var enumerationError: Error?
        let keys: [URLResourceKey] = [.nameKey, .fileSizeKey, .isDirectoryKey, .isSymbolicLinkKey]
        guard let enumerator = FileManager.default.enumerator(at: directory, includingPropertiesForKeys: keys, options: [], errorHandler: { _, error in
          enumerationError = error
          return false
        }) else { throw ProviderError(message: "The provider could not list the folder.") }
        var result: [ProviderFile] = []
        var count = 0
        for case let item as URL in enumerator {
          count += 1
          if count > Self.maxEntries { throw ProviderError(message: "Folder exceeds the \(Self.maxEntries) entry mobile import limit.") }
          if enumerator.level > Self.maxDepth { throw ProviderError(message: "Folder exceeds the \(Self.maxDepth) level mobile import limit.") }
          let values = try item.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
          if values.isSymbolicLink == true { throw ProviderError(message: "The selected folder contains a symbolic link.") }
          if values.isDirectory == true { continue }
          result.append(try remember(item, alreadyCoordinated: true))
        }
        if let error = enumerationError { throw error }
        return result
      }
    }
  }

  func read(_ identity: String) throws -> Data {
    guard let identityURL = URL(string: identity), identityURL.isFileURL else {
      throw ProviderError(message: "Expected a document provider URL.")
    }
    let data = try bookmarks.load(identity: identity)
    var stale = false
    let url: URL
    do { url = try URL(resolvingBookmarkData: data, bookmarkDataIsStale: &stale) }
    catch { throw ProviderError(message: "Provider permission could not be restored. Select the document again.") }
    guard !stale else { throw ProviderError(message: "Provider permission is stale. Select the document again.") }
    return try scoped(url) {
      try coordinated(url) { coordinatedURL in
        guard let stream = InputStream(url: coordinatedURL) else { throw ProviderError(message: "The provider could not open the document.") }
        return try Self.readBounded(stream)
      }
    }
  }

  static func readBounded(_ stream: InputStream) throws -> Data {
    stream.open()
    defer { stream.close() }
    var output = Data()
    var buffer = [UInt8](repeating: 0, count: 32 * 1024)
    while true {
      let count = stream.read(&buffer, maxLength: buffer.count)
      if count < 0 { throw stream.streamError ?? ProviderError(message: "The provider read failed.") }
      if count == 0 { break }
      if output.count + count > maxBytes { throw ProviderError(message: "Document exceeds the 16 MiB mobile import limit.") }
      output.append(contentsOf: buffer.prefix(count))
    }
    return output
  }
}
