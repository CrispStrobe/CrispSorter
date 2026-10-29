import Foundation
import SwiftRs
import Tauri
import UIKit
import UniformTypeIdentifiers

private struct PickerResponse: Encodable {
  let cancelled: Bool
  let files: [ProviderFile]
}

private struct ReadArgs: Decodable { let uri: String }
private struct ReadResponse: Encodable { let data: String }

class MobileProviderPlugin: Plugin, UIDocumentPickerDelegate {
  private var pending: (invoke: Invoke, folder: Bool)?

  @objc public func pickFiles(_ invoke: Invoke) {
    presentPicker(invoke, folder: false)
  }

  @objc public func pickFolder(_ invoke: Invoke) {
    presentPicker(invoke, folder: true)
  }

  private func presentPicker(_ invoke: Invoke, folder: Bool) {
    DispatchQueue.main.async {
      guard self.pending == nil else { invoke.reject("A document picker is already open."); return }
      guard let controller = self.manager.viewController, controller.presentedViewController == nil else {
        invoke.reject("The document picker cannot be presented right now.")
        return
      }
      self.pending = (invoke, folder)
      let picker = UIDocumentPickerViewController(forOpeningContentTypes: folder ? [.folder] : [.item], asCopy: false)
      picker.allowsMultipleSelection = !folder
      picker.delegate = self
      controller.present(picker, animated: true)
    }
  }

  func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
    let invocation = pending?.invoke
    pending = nil
    invocation?.resolve(PickerResponse(cancelled: true, files: []))
  }

  func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
    guard let request = pending else { return }
    pending = nil
    DispatchQueue.global(qos: .userInitiated).async {
      do {
        let access = ProviderAccess(bookmarks: try BookmarkStore())
        let files: [ProviderFile]
        if request.folder {
          guard urls.count == 1, let folder = urls.first else { throw ProviderError(message: "The picker did not return a folder.") }
          files = try access.folder(folder)
        } else {
          files = try access.files(urls)
        }
        request.invoke.resolve(PickerResponse(cancelled: false, files: files))
      } catch {
        request.invoke.reject("Document provider access failed: \(error.localizedDescription)")
      }
    }
  }

  @objc public func readFile(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(ReadArgs.self)
    DispatchQueue.global(qos: .userInitiated).async {
      do {
        let access = ProviderAccess(bookmarks: try BookmarkStore())
        let bytes = try access.read(args.uri)
        invoke.resolve(ReadResponse(data: bytes.base64EncodedString()))
      } catch {
        invoke.reject("Document provider access failed: \(error.localizedDescription)")
      }
    }
  }
}

@_cdecl("init_plugin_mobile_provider")
func initPlugin() -> Plugin { MobileProviderPlugin() }
