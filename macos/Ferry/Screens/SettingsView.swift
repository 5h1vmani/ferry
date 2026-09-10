// The standard Settings window, Command comma (docs/ia.md, Settings, the
// Mac). Two settings: which folders paired phones may reach, and where
// pulled files land.
//
// Desktop and Downloads are the defaults. They are names a person
// recognises, where the first version's single "Ferry" folder was a name
// Ferry made up.
//
// Changing either setting calls straight into the running engine
// (EngineModel.setRoots, EngineModel.setDownloadPath). Neither restarts it:
// a root change reaches an already-connected phone on its next operation.

import SwiftUI
import AppKit

struct SettingsView: View {
    @EnvironmentObject private var model: EngineModel

    var body: some View {
        Form {
            Section {
                ForEach(model.roots) { root in
                    SharedRootRow(root: root, canRemove: model.roots.count > 1) {
                        remove(root)
                    }
                }

                Button {
                    add()
                } label: {
                    Label(S.settings.addAFolder, systemImage: FerryIcon.pair)
                }
                .buttonStyle(.link)
            } header: {
                Text(S.settings.sharedFolders)
            } footer: {
                // Stated because it is the boundary of what pairing gave
                // away, and a person deciding whether to add a folder is
                // deciding exactly that.
                Text(S.settings.sharedFoldersFooter)
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
            }

            Section(S.settings.whereFilesLand) {
                HStack(spacing: FerrySpace.s3) {
                    Text(model.downloadPath)
                        .font(FerryFont.mono)
                        .foregroundStyle(FerryColor.textSecondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button(S.settings.choose) {
                        if let url = chooseFolder() {
                            model.setDownloadPath(url.path)
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .padding(FerrySpace.s5)
        .frame(width: 480)
    }

    private func add() {
        guard let url = chooseFolder() else { return }
        model.setRoots(model.roots.map(\.path) + [url.path])
    }

    private func remove(_ root: SharedRootSnapshot) {
        model.setRoots(model.roots.map(\.path).filter { $0 != root.path })
    }

    private func chooseFolder() -> URL? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = S.settings.choose
        guard panel.runModal() == .OK else { return nil }
        return panel.url
    }
}

/// One folder this Mac serves: its name as the peer sees it, its path, and
/// a way to stop. A view, not a component: one screen, one platform
/// (docs/components.md, Rejected).
private struct SharedRootRow: View {
    let root: SharedRootSnapshot
    let canRemove: Bool
    let onRemove: () -> Void

    var body: some View {
        HStack(spacing: FerrySpace.s2) {
            Image(systemName: FerryIcon.folder)
                .foregroundStyle(FerryColor.textSecondary)

            VStack(alignment: .leading, spacing: 2) {
                Text(root.name)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)
                Text(root.path)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer()

            Button(S.settings.stopSharing, action: onRemove)
                .buttonStyle(.link)
                // The last root cannot be removed: an engine serving
                // nothing is not a state any screen describes.
                .disabled(!canRemove)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(S.settings.rootAccessibility(name: root.name, path: root.path))
    }
}
