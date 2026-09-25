package app.ferry.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import app.ferry.FerryColor

// Shown between first run and the engine's first snapshot. docs/audits/
// oss-looks.md M6: until that snapshot, devices, transfers, and presence
// are all at their empty starting values. Drawing Devices from those values
// would say "No Mac paired" and "Not reachable over Wi-Fi" even on a launch
// where a Mac is already paired and reachable. This screen states neither,
// so it never shows a fact before the engine has confirmed it.
@Composable
fun StartingScreen(modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .fillMaxSize()
            .background(FerryColor.background()),
        contentAlignment = Alignment.Center,
    ) {
        CircularProgressIndicator(color = FerryColor.accent())
    }
}
