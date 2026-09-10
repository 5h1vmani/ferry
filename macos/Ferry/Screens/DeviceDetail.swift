// The area on the right when a device is selected (docs/ia.md, On the
// Mac): Files, Transfers, then Info, stacked as sections of one view.
//
// Files browses the phone's shared folder. The engine's list call blocks
// for a round trip, so the model runs it off the main thread and this view
// awaits it. Browsing starts at the root, which the engine names as the
// empty path.

import SwiftUI

struct DeviceDetail: View {
    @EnvironmentObject private var model: EngineModel
    let device: DeviceInfo

    /// The folders entered since the root, in order.
    @State private var folders: [String] = []
    @State private var entries: [Entry] = []
    @State private var isReading = false
    @State private var listError: ThreePartError?

    /// The path the engine reads. The root is the empty string.
    private var remotePath: String {
        folders.joined(separator: "/")
    }

    private var transfers: [TransferInfo] {
        model.transfers(for: device.keyHex)
    }

    var body: some View {
        Form {
            if let actionError = model.actionError {
                ErrorBlock(error: actionError)
            }

            Section(S.deviceDetail.filesSection) {
                filesHeader
                filesBody
            }

            Section(S.deviceDetail.transfersSection) {
                if transfers.isEmpty {
                    EmptyState(line: S.deviceDetail.noTransfers)
                } else {
                    ForEach(transfers) { transfer in
                        TransferRow(
                            transfer: transfer,
                            speedBytesPerSec: model.speed(forDevice: device.keyHex),
                            onRetry: { model.retry(transferId: transfer.id) }
                        )
                    }
                }
            }

            Section(S.deviceDetail.infoSection) {
                LabeledContent(S.deviceDetail.pairedLabel, value: FerryFormat.longDate(unixSecs: device.pairedUnixSecs))
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
        .formStyle(.grouped)
        .navigationTitle(device.name)
        .task(id: reloadKey) {
            await load()
        }
        .onChange(of: device.keyHex) { _, _ in
            folders = []
            model.actionError = nil
        }
    }

    /// Changes whenever the view must read a different folder.
    private var reloadKey: String {
        device.keyHex + "\u{0000}" + remotePath
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

    /// "/" at the root, then the folders entered, so a person can see where
    /// they are.
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

/// One file or folder in the phone's shared folder. A folder is a control
/// that enters it. A file states its size and offers to copy it.
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

/// A transfer as one row in the Transfers section: file name, a
/// ProgressLine, and the TransportBadge while it is active
/// (docs/ia.md, Transfers, Active).
private struct TransferRow: View {
    let transfer: TransferInfo
    let speedBytesPerSec: UInt64?
    let onRetry: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: FerrySpace.s2) {
            HStack {
                Text(transfer.fileName)
                    .font(FerryFont.label)
                    .foregroundStyle(FerryColor.text)
                Spacer()
                if transfer.state == .active, let transport = transfer.transport {
                    TransportBadge(
                        state: TransportBadgeState(
                            transport: transport,
                            speedBytesPerSec: speedBytesPerSec
                        )
                    )
                }
            }
            ProgressLine(
                transfer: transfer,
                speedBytesPerSec: transfer.state == .active ? speedBytesPerSec : nil,
                onRetry: onRetry
            )
        }
        .padding(.vertical, FerrySpace.s1)
    }
}
