// The standard Settings window, Command comma (docs/ia.md, Settings, the
// Mac). Three settings: which folders paired phones may reach, where
// pulled files land, and which Wi-Fi networks this Mac stays reachable
// on.
//
// Desktop and Downloads are the defaults. They are names a person
// recognises, where the first version's single "Ferry" folder was a name
// Ferry made up.
//
// Changing a setting calls straight into the running engine
// (EngineModel.setRoots, EngineModel.setDownloadPath, .trustNetwork,
// .forgetNetwork). None restarts it: a root change reaches an
// already-connected phone on its next operation, and a network change
// takes effect through the engine's own presence rule
// (docs/engine-contract.md, item 18).

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

            Section {
                if let networkName = model.presence.networkName, !model.trustedNetworks.contains(networkName) {
                    CurrentNetworkRow(name: networkName) {
                        model.trustNetwork(networkName)
                    }
                }

                ForEach(model.trustedNetworks, id: \.self) { name in
                    TrustedNetworkRow(name: name) {
                        model.forgetNetwork(name)
                    }
                }
            } header: {
                Text(S.settings.networks)
            } footer: {
                // docs/engine-contract.md, item 18: stated for the same
                // reason presence.consequence is, in PresenceControl. The
                // words are the same ones shown there.
                if model.presence.isQuietOnThisNetwork {
                    VStack(alignment: .leading, spacing: FerrySpace.s1) {
                        Text(quietWhy)
                            .font(FerryFont.caption)
                            .foregroundStyle(FerryColor.textSecondary)
                        if model.presence.networkName == nil {
                            Button(S.settings.openLocationSettings, action: openLocationSettings)
                                .buttonStyle(.link)
                        }
                    }
                }
            }
        }
        .formStyle(.grouped)
        .padding(FerrySpace.s5)
        .frame(width: 480)
    }

    private var quietWhy: String {
        model.presence.networkName == nil ? S.presence.quietUnknownNetwork : S.presence.quietKnownNetwork
    }

    private func openLocationSettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices") else {
            return
        }
        NSWorkspace.shared.open(url)
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
        VStack(alignment: .leading, spacing: FerrySpace.s1) {
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
            // Stated because a disabled control with no reason is a grey
            // button and a guess.
            if !canRemove {
                Text(S.settings.lastRootCaption)
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(S.settings.rootAccessibility(name: root.name, path: root.path))
    }
}

/// The Wi-Fi network this Mac is on right now, shown only while it is
/// known and not yet trusted. `docs/engine-contract.md`, item 18.
private struct CurrentNetworkRow: View {
    let name: String
    let onTrust: () -> Void

    var body: some View {
        HStack(spacing: FerrySpace.s2) {
            Image(systemName: FerryIcon.wifi)
                .foregroundStyle(FerryColor.textSecondary)

            Text(name)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.text)

            Spacer()

            Button(S.settings.trustThisNetwork, action: onTrust)
                .buttonStyle(.link)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(S.settings.currentNetworkAccessibility(name: name))
    }
}

/// One Wi-Fi network on the trusted list, with a way to remove it.
/// `docs/engine-contract.md`, item 18.
private struct TrustedNetworkRow: View {
    let name: String
    let onRemove: () -> Void

    var body: some View {
        HStack(spacing: FerrySpace.s2) {
            Image(systemName: FerryIcon.wifi)
                .foregroundStyle(FerryColor.textSecondary)

            Text(name)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.text)

            Spacer()

            Button(S.settings.remove, action: onRemove)
                .buttonStyle(.link)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(S.settings.trustedNetworkAccessibility(name: name))
    }
}
