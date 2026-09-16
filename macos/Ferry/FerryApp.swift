// The app entry point: one window, presence in the menu bar, and the
// standard Settings scene (docs/ia.md, On the Mac).
//
// The engine is built and started here, once, and every screen reads it
// through the environment. The delegate stops it on quit. Stopping is not
// optional: the listener holds the model and the model holds the engine, so
// dropping the app would never run the engine's own clean up.
//
// The menu bar scene reads the same model as the window. Two surfaces, one
// value: presence is ambient, so it appears wherever a person already is,
// and it can only do that honestly if neither surface owns the state.

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
                    appDelegate.registerServicesProvider()
                    await model.start()
                }
        }
        .commands {
            CommandGroup(after: .newItem) {
                Button(S.devices.sendFiles) {
                    guard let device = model.targetDevice else { return }
                    SendFilesPanel.present(forDevice: device.keyHex, model: model)
                }
                .keyboardShortcut("o", modifiers: .command)
                .disabled(model.targetDevice == nil)

                Button(S.devices.retryFailedTransfers) {
                    guard let device = model.targetDevice else { return }
                    model.retryAllFailed(forDevice: device.keyHex)
                }
                .keyboardShortcut("r", modifiers: .command)
                .disabled(model.targetDevice == nil)
            }
        }

        MenuBarExtra {
            MenuBarPresence()
                .environmentObject(model)
        } label: {
            MenuBarLabel(presence: model.presence)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environmentObject(model)
        }
    }
}

/// Holds the model so the engine can be stopped when the app quits, and
/// registers this app's one Finder Services entry.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    var model: EngineModel?
    private var servicesProvider: FerryServicesProvider?

    /// Sets `NSApp.servicesProvider` once the model exists. Called from
    /// `FerryApp`'s own `.task`, alongside `model.start()`, because the
    /// delegate has no model yet at `applicationDidFinishLaunching`.
    /// `docs/ux-fix-plan.md`, item 3.
    func registerServicesProvider() {
        guard let model, servicesProvider == nil else { return }
        let provider = FerryServicesProvider(model: model)
        servicesProvider = provider
        NSApp.servicesProvider = provider
    }

    func applicationWillTerminate(_ notification: Notification) {
        model?.stop()
    }

    /// Files dropped on the Dock icon. `project.yml`'s
    /// `CFBundleDocumentTypes` declares `public.item` with handler rank
    /// `None`, so Ferry never becomes a default opener; this only fires
    /// when a person drops files on the icon on purpose. Pushes to
    /// `targetDevice`'s landing folder, the same as a window drop.
    /// `docs/ux-fix-plan.md`, item 3.
    func application(_ application: NSApplication, open urls: [URL]) {
        guard let model else { return }
        guard let device = model.targetDevice else {
            model.actionError = DropError.noDevice.threePart(canRetry: false)
            return
        }
        model.send(urls: urls, toDevice: device.keyHex)
    }
}
