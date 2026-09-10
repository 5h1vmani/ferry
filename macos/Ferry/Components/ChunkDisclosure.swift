// The bottom of the depth axis (docs/ia.md, Depth): the chunks of one
// transfer, one interaction away and never in the way.
//
// Part of the Transfers row, not a component of its own
// (docs/components.md, ProgressLine). It appears once a transfer's size is
// known, which is every transfer past its first `stat`. Collapsed by
// default, always: a person reading an error is not reading a chunk list
// yet.

import SwiftUI

struct ChunkDisclosure: View {
    let chunks: ChunkFacts

    @State private var isExpanded = false

    var body: some View {
        DisclosureGroup(isExpanded: $isExpanded) {
            VStack(alignment: .leading, spacing: FerrySpace.s1) {
                if let index = chunks.failedIndex {
                    row(S.chunks.failedChunk(index: Int(index)))
                }
                // Verified chunks are summarised and never listed: ninety
                // rows saying "verified" is not depth, it is noise.
                row(S.chunks.verifiedSummary(
                    verified: Int(chunks.verified),
                    total: Int(chunks.total)
                ))
            }
            .padding(.top, FerrySpace.s1)
        } label: {
            HStack(spacing: FerrySpace.s2) {
                Text(S.chunks.show)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.accentText)
                Text(S.chunks.verified(
                    verified: Int(chunks.verified),
                    total: Int(chunks.total)
                ))
                .font(FerryFont.mono)
                .foregroundStyle(FerryColor.textSecondary)
            }
        }
        .accessibilityHint(S.chunks.accessibilityHint)
    }

    private func row(_ text: String) -> some View {
        Text(text)
            .font(FerryFont.mono)
            .foregroundStyle(FerryColor.textSecondary)
    }
}
