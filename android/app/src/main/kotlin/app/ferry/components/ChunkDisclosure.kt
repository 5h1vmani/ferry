package app.ferry.components

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.model.ChunkFacts

// The bottom of the depth axis. docs/ia.md, The chunk disclosure.
//
// Part of the Transfers row, not a component of its own. It appears only
// where the engine holds a chunk-level fact, and collapsed by default,
// always: a person reading an error is not reading a chunk list yet.
//
// Depth on demand means the precise thing is one tap away and never in the
// way. Verified chunks are summarised and never listed — ninety rows
// saying "verified" is not depth, it is noise.
@Composable
fun ChunkDisclosure(
    chunks: ChunkFacts,
    modifier: Modifier = Modifier,
) {
    var expanded by remember { mutableStateOf(false) }
    val summary = stringResource(R.string.chunks_verified, chunks.verified, chunks.total)
    val show = stringResource(R.string.chunks_show)

    Column(modifier = modifier) {
        Row(
            modifier = Modifier
                .clickable { expanded = !expanded }
                .padding(vertical = FerrySpace.s2)
                .semantics { contentDescription = "$show. $summary" },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = ferryIconFor(FerryIcon.expand),
                contentDescription = null,
                tint = FerryColor.textSecondary(),
            )
            Spacer(Modifier.width(FerrySpace.s2))
            Text(text = show, style = FerryFont.body(), color = FerryColor.accentText())
            Spacer(Modifier.width(FerrySpace.s2))
            Text(
                text = summary,
                style = FerryFont.mono(),
                color = FerryColor.textSecondary(),
            )
        }

        AnimatedVisibility(visible = expanded) {
            Column {
                val failed = chunks.failedIndex
                if (failed != null) {
                    Text(
                        text = stringResource(R.string.chunks_failed_chunk, failed),
                        style = FerryFont.mono(),
                        color = FerryColor.textSecondary(),
                    )
                    Spacer(Modifier.height(FerrySpace.s1))
                }
                Text(
                    text = stringResource(
                        R.string.chunks_verified_summary,
                        chunks.verified,
                        chunks.total,
                    ),
                    style = FerryFont.mono(),
                    color = FerryColor.textSecondary(),
                )
            }
        }
    }
}
