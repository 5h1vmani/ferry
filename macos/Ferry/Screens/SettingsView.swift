// The standard Settings window, Command comma (docs/ia.md, On the Mac).
// Phase 1 has one setting on the Mac: the shared folder (docs/design.md).
// Nothing is persisted yet; choosing a folder only updates this view.

import SwiftUI
import AppKit

struct SettingsView: View {
    @State private var sharedFolderPath = NSHomeDirectory() + "/Ferry"

    var body: some View {
        Form {
            LabeledContent(S.settings.sharedFolderLabel) {
                HStack(spacing: FerrySpace.s3) {
                    Text(sharedFolderPath)
                        .font(FerryFont.mono)
                        .foregroundStyle(FerryColor.textSecondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Button(S.settings.choose, action: chooseFolder)
                }
            }
        }
        .formStyle(.grouped)
        .padding(FerrySpace.s5)
        .frame(width: 440)
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = S.settings.choose
        if panel.runModal() == .OK, let url = panel.url {
            sharedFolderPath = url.path
        }
    }
}

#Preview {
    SettingsView()
}
