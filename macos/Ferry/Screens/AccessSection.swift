// The Finder mount, in one line (docs/ia.md, Access). L2.
//
// This is the only place in either app that states whether job 1 is
// currently true: the phone's folder can be opened in Finder with no
// action on either device. It is one line because the job is finished
// elsewhere — in Finder — and Ferry's part is to say so and get out of the
// way.
//
// When the mount is not ready the section is absent. Ferry does not state a
// negative about a thing it has not yet done.

import SwiftUI
import AppKit

struct AccessSection: View {
    let mount: MountSnapshot

    var body: some View {
        if let path = mount.path {
            HStack(spacing: FerrySpace.s2) {
                Image(systemName: FerryIcon.folder)
                    .foregroundStyle(FerryColor.accent)
                Text(S.access.mountReady)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)
                Text(path)
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.text)
                    .lineLimit(1)
                    .truncationMode(.head)
                    .textSelection(.enabled)
                Spacer()
                Button(S.access.openInFinder) {
                    NSWorkspace.shared.open(URL(fileURLWithPath: path))
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(S.access.accessibilityMountReady(path: path))
        }
    }
}
