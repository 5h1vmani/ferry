// The bottom of the depth axis (docs/ia.md, Depth): the chunks of one
// transfer, one interaction away and never in the way.
//
// Part of the Transfers row, not a component of its own
// (docs/components.md, ProgressLine). It appears only where the engine
// holds a chunk-level fact, which today means only after a verify failure.
// Collapsed by default, always: a person reading an error is not reading a
// chunk list yet.

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
                if chunks.isCounted {
                    row(S.chunks.verifiedSummary(
                        verified: Int(chunks.verified),
                        total: Int(chunks.total)
                    ))
                }
            }
            .padding(.top, FerrySpace.s1)
        } label: {
            HStack(spacing: FerrySpace.s2) {
                Text(S.chunks.show)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.accentText)
                // The count is left out while the engine does not publish
                // it (docs/engine-contract.md, item 7). An unknown part is left
                // out, not guessed.
                if chunks.isCounted {
                    Text(S.chunks.verified(
                        verified: Int(chunks.verified),
                        total: Int(chunks.total)
                    ))
                    .font(FerryFont.mono)
                    .foregroundStyle(FerryColor.textSecondary)
                }
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
