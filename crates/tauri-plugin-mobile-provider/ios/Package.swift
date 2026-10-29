// swift-tools-version:5.7
import PackageDescription

let package = Package(
  name: "tauri-plugin-mobile-provider",
  platforms: [.iOS(.v16)],
  products: [.library(name: "tauri-plugin-mobile-provider", type: .static, targets: ["tauri-plugin-mobile-provider"])],
  dependencies: [.package(name: "Tauri", path: "../.tauri/tauri-api")],
  targets: [
    .target(name: "tauri-plugin-mobile-provider", dependencies: [.byName(name: "Tauri")], path: "Sources"),
    .testTarget(name: "MobileProviderTests", dependencies: ["tauri-plugin-mobile-provider"], path: "Tests")
  ]
)
