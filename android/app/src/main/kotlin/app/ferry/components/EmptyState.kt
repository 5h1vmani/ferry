package app.ferry.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace

// One line and one action, centred. Used by Devices with no devices and by
// Transfers with no transfers. docs/components.md.
//
// Sizing is the caller's choice: DevicesScreen fills the whole screen with
// it, DevicesScreen fits it into the Transfers section of a longer list.
@Composable
fun EmptyState(
    line: String,
    actionLabel: String? = null,
    onAction: (() -> Unit)? = null,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxWidth()
            .padding(FerrySpace.s5),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(text = line, style = FerryFont.body(), color = FerryColor.textSecondary())
        if (actionLabel != null && onAction != null) {
            Spacer(Modifier.height(FerrySpace.s3))
            Button(onClick = onAction) {
                Text(actionLabel)
            }
        }
    }
}
