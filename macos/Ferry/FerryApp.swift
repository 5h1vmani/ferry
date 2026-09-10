// The app entry point: one window, and the standard Settings scene
// (docs/ia.md, On the Mac: "Settings: the standard Settings window,
// Command comma").
//
// The engine is built and started here, once, and every screen reads it
// through the environment. The delegate stops it on quit. Stopping is not
// optional: the listener holds the model and the model holds the engine, so
// dropping the app would never run the engine's own clean up.

import SwiftUI
import AppKit

@main
struct FerryApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = EngineModel()

    var body: some Scene {
        WindowGroup(S.app.name) {
            ContentView()
                .environmentObject(model)
                .task {
                    appDelegate.model = model
                    model.start()
                }
        }

        Settings {
            SettingsView()
                .environmentObject(model)
        }
    }
}

/// Holds the model so the engine can be stopped when the app quits.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    var model: EngineModel?

    func applicationWillTerminate(_ notification: Notification) {
        model?.stop()
    }
}
