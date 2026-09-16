// The one Finder Services entry, "Send with Ferry" (docs/ux-fix-plan.md,
// item 3). `project.yml`'s `NSServices` block names this class's method as
// `NSMessage`, and `AppDelegate` sets `NSApp.servicesProvider` to one of
// these once the model exists, so a file or folder selected in Finder can
// reach the engine without opening Ferry's window first.

import AppKit

final class FerryServicesProvider: NSObject {
    private let model: EngineModel

    init(model: EngineModel) {
        self.model = model
    }

    /// Reads the file URLs Finder passed on the pasteboard, and routes
    /// each one through `EngineModel.sendFromFinder`. Called on whatever
    /// thread AppKit delivers the service on, so the model is reached
    /// through the main actor.
    @objc func sendWithFerry(
        _ pasteboard: NSPasteboard,
        userData: String,
        error: AutoreleasingUnsafeMutablePointer<NSString>
    ) {
        // Reads file URLs only. Without this, a web URL on the pasteboard
        // reaches `send`. `docs/audits/ux-gestures.md`, finding 4.
        let options: [NSPasteboard.ReadingOptionKey: Any] = [.urlReadingFileURLsOnly: true]
        guard
            let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: options) as? [URL],
            !urls.isEmpty
        else {
            return
        }
        let model = self.model
        Task { @MainActor in
            model.sendFromFinder(urls: urls)
        }
    }
}
