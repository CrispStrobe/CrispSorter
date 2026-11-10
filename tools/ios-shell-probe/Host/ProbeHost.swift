import SwiftUI

// Xcode's UI-test runner host. The application under test is separately
// installed from its immutable build artifact and launched by bundle ID.
@main
struct ProbeHost: App {
    var body: some Scene {
        WindowGroup { Text("OCR navigation probe") }
    }
}
