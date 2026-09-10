// The app entry point: one window, and the standard Settings scene
// (docs/ia.md, On the Mac: "Settings: the standard Settings window,
// Command comma").

import SwiftUI

@main
struct FerryApp: App {
    var body: some Scene {
        WindowGroup(S.app.name) {
            ContentView()
        }

        Settings {
            SettingsView()
        }
    }
}
