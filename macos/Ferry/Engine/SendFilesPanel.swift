// Opens the file panel for "Send files…" and starts pushing what a person
// chose (docs/ux-fix-plan.md, items 3 and 5). The device row's context
// menu, the app menu, and its Cmd+O shortcut all open this same panel.

import AppKit

@MainActor
enum SendFilesPanel {
    static func present(forDevice keyHex: String, model: EngineModel) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.prompt = S.devices.sendFiles
        guard panel.runModal() == .OK else { return }
        model.send(urls: panel.urls, toDevice: keyHex)
    }
}
