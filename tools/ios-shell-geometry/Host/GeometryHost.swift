import UIKit
import WebKit

@main
final class GeometryApplication: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        window.rootViewController = GeometryController()
        window.makeKeyAndVisible()
        self.window = window
        return true
    }
}

final class GeometryController: UIViewController, WKScriptMessageHandler {
    private var web: WKWebView!
    private let report = UILabel()
    private let variant = ProcessInfo.processInfo.arguments.contains("--baseline") ? "baseline" : "current"
    private let largeText = ProcessInfo.processInfo.arguments.contains("--large-text")

    override func viewDidLoad() {
        super.viewDidLoad()
        let configuration = WKWebViewConfiguration()
        configuration.preferences.minimumFontSize = largeText ? 28 : 0
        configuration.userContentController.add(self, name: "geometry")
        web = WKWebView(frame: view.bounds, configuration: configuration)
        web.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.addSubview(web)
        // A native diagnostic value, outside the HTML flow, exposes measured
        // UIKit insets to XCTest without changing the source shell's geometry.
        report.text = "Waiting for geometry"
        report.font = .systemFont(ofSize: 12)
        report.textColor = .systemTeal
        report.isAccessibilityElement = true
        report.accessibilityLabel = "Geometry metrics"
        view.addSubview(report)
        guard let url = Bundle.main.url(forResource: variant, withExtension: "html") else { fatalError("Missing source-matched fixture") }
        web.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        report.frame = CGRect(x: view.safeAreaInsets.left + 12, y: view.bounds.height / 2, width: 260, height: 20)
        web?.evaluateJavaScript("window.measureGeometry?.()", completionHandler: nil)
    }

    private func rect(_ r: CGRect) -> [String: CGFloat] { ["x": r.origin.x, "y": r.origin.y, "width": r.width, "height": r.height] }
    private func insets(_ i: UIEdgeInsets) -> [String: CGFloat] { ["top": i.top, "right": i.right, "bottom": i.bottom, "left": i.left] }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let dom = message.body as? [String: Any] else { return }
        let value: [String: Any] = [
            "variant": variant, "minimum_font_points": largeText ? 28 : 0,
            "native": ["window": rect(view.bounds), "safe_insets": insets(view.safeAreaInsets),
                       "webview": rect(web.frame), "adjusted_content_insets": insets(web.scrollView.adjustedContentInset)],
            "dom": dom,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), let text = String(data: data, encoding: .utf8) else { return }
        report.text = "Shell geometry ready"
        report.accessibilityValue = text
        let root = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("geometry", isDirectory: true)
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let orientation = view.bounds.width > view.bounds.height ? "landscape" : "portrait"
        try? data.write(to: root.appendingPathComponent("\(variant)-\(orientation)-\(largeText ? "large" : "normal").json"), options: .atomic)
    }
}
