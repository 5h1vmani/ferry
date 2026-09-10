// The area on the right when a device is selected (docs/ia.md, On the
// Mac). Six sections, in the order a person needs them:
//
//   Access      the Finder mount, one line, absent until it is ready   L2
//   Files       this Mac's view of the phone's roots                  L2
//   Automatic   copy new photos, one switch                           L3
//   Transfers   what is moving, newest first                          L3
//   Access log  what this pair actually did                           L5
//   Info        four facts and Forget, as a footer                    L1
//
// Info was an equal section in the first version of the IA. It is four
// facts read twice a year, stacked against things that change every second,
// so it is demoted to a footer and stops competing.
//
// Six sections is the most this shape will carry. A seventh turns the pane
// into a list of destinations, which is a bigger change than adding a
// section — so a seventh has to earn it.

import SwiftUI

struct DeviceDetail: View {
    @EnvironmentObject private var model: EngineModel
    let device: DeviceSnapshot

    /// The folders entered since the roots, in order.
    @State private var folders: [String] = []
    @State private var entries: [Entry] = []
    @State private var isReading = false
    @State private var listError: ThreePartError?

    /// The path the engine reads. The roots themselves are the empty path.
    private var remotePath: String {
        folders.joined(separator: "/")
    }

    private var groups: [TransferGroupSnapshot] {
        model.groups(forDevice: device.keyHex)
    }

    var body: some View {
        Form {
            if let actionError = model.actionError {
                ErrorBlock(error: actionError)
            }

            AccessSection(mount: model.mount(forDevice: device.keyHex))

            Section(S.deviceDetail.filesSection) {
                filesHeader
                filesBody
            }

            AutomaticSection(
                autoCopy: model.autoCopy(forDevice: device.keyHex),
                deviceName: device.name
            ) { enabled in
                model.setAutoCopy(forDevice: device.keyHex, enabled: enabled)
            }

            Section(S.deviceDetail.transfersSection) {
                if groups.isEmpty {
                    EmptyState(line: S.deviceDetail.noTransfers)
                } else {
                    ForEach(groups) { group in
                        TransferRow(group: group) {
                            model.retry(transferId: group.id)
                        }
                    }
                }
            }

            AccessLogSection(
                days: model.accessLog(forDevice: device.keyHex),
                peerName: device.name
            )

            infoFooter
        }
        .formStyle(.grouped)
        .navigationTitle(device.name)
        .navigationSubtitle(subtitle)
        .task(id: reloadKey) {
            await load()
        }
        .onChange(of: device.keyHex) { _, _ in
            folders = []
            model.actionError = nil
        }
    }

    /// The active transport, and the spare stated once beside it, so a
    /// pulled cable is not a surprise (docs/ia.md, Devices).
    private var subtitle: String {
        var parts = [TransportBadge.accessibilityText(for: device.badge)]
        if let spare = device.spareTransport {
            parts.append(S.devices.spareTransport(spare))
        }
        return parts.joined(separator: S.common.dotSeparator)
    }

    /// Changes whenever the view must read a different folder.
    private var reloadKey: String {
        device.keyHex + "\u{0000}" + remotePath
    }

    /// Four facts and one destructive control, in a footer rather than a
    /// section of its own.
    private var infoFooter: some View {
        Section {
            LabeledContent(S.deviceDetail.pairedLabel, value: device.pairedDate)
            LabeledContent(S.deviceDetail.keyFingerprintLabel) {
                Text(device.keyHex)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.textSecondary)
                    .textSelection(.enabled)
            }
            Button(S.deviceDetail.forgetThisPhone, role: .destructive) {
                model.forget(keyHex: device.keyHex)
            }
        }
    }

    private var filesHeader: some View {
        HStack(spacing: FerrySpace.s3) {
            Button(S.files.goUp) {
                if !folders.isEmpty {
                    folders.removeLast()
                }
            }
            .disabled(folders.isEmpty)

            Text(pathText)
                .font(FerryFont.mono)
                .foregroundStyle(FerryColor.textSecondary)
                .lineLimit(1)
                .truncationMode(.head)
        }
    }

    @ViewBuilder
    private var filesBody: some View {
        if isReading {
            ProgressView()
                .controlSize(.small)
                .accessibilityLabel(S.files.reading)
        } else if let listError {
            ErrorBlock(error: listError) {
                Task { await load() }
            }
        } else if entries.isEmpty {
            Text(S.files.emptyFolder)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.textSecondary)
        } else {
            ForEach(sortedEntries) { entry in
                EntryRow(
                    entry: entry,
                    onOpen: { folders.append(entry.name) },
                    onCopy: {
                        model.pull(
                            deviceKeyHex: device.keyHex,
                            remotePath: path(for: entry),
                            localName: entry.name
                        )
                    }
                )
            }
        }
    }

    /// "/" at the roots, then the folders entered, so a person can see
    /// where they are. The first segment is a root's name, which is what
    /// the peer serves it as (docs/engine-contract.md, item 15).
    private var pathText: String {
        folders.isEmpty ? S.files.root : S.files.root + folders.joined(separator: S.files.root)
    }

    /// Folders first, then files, each by name. The engine returns entries
    /// in the order the phone sent them, which is the filesystem's order.
    private var sortedEntries: [Entry] {
        entries.sorted { left, right in
            if left.kind != right.kind {
                return left.kind == .directory
            }
            return left.name.localizedStandardCompare(right.name) == .orderedAscending
        }
    }

    private func path(for entry: Entry) -> String {
        folders.isEmpty ? entry.name : remotePath + "/" + entry.name
    }

    private func load() async {
        isReading = true
        listError = nil
        do {
            entries = try await model.list(deviceKeyHex: device.keyHex, remotePath: remotePath)
        } catch {
            entries = []
            listError = ThreePartError.from(error, canRetry: true)
        }
        isReading = false
    }
}

/// One file or folder inside the phone's roots. A folder is a control that
/// enters it. A file states its size and offers to copy it.
private struct EntryRow: View {
    let entry: Entry
    let onOpen: () -> Void
    let onCopy: () -> Void

    var body: some View {
        switch entry.kind {
        case .directory:
            Button(action: onOpen) {
                Text(entry.name)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .accessibilityLabel(S.files.folderAccessibility(name: entry.name))

        case .file:
            HStack(spacing: FerrySpace.s3) {
                Text(entry.name)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)
                Spacer()
                Text(FerryFormat.bytes(entry.size))
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.textSecondary)
                Button(S.files.copyToMac, action: onCopy)
            }
            .accessibilityElement(children: .contain)
            .accessibilityLabel(
                S.files.fileAccessibility(name: entry.name, size: FerryFormat.bytes(entry.size))
            )
        }
    }
}
