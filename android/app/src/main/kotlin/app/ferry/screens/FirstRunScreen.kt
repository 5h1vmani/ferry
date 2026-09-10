package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R

// The first screen a person sees, docs/ia.md phone first run step 1. It
// says what Ferry needs and why, and has one control.
//
// Continue opens the system screen for all files access. The notification
// prompt follows on Android 13 and later. Both of those belong to the
// activity, so this screen only reports the tap.
@Composable
fun FirstRunScreen(
    onContinue: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier,
        containerColor = FerryColor.background(),
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(FerrySpace.s5)
                .fillMaxSize(),
        ) {
            Text(
                text = stringResource(R.string.first_run_title),
                style = FerryFont.title(),
                color = FerryColor.text(),
            )
            Spacer(Modifier.height(FerrySpace.s4))
            Text(
                text = stringResource(R.string.first_run_all_files),
                style = FerryFont.body(),
                color = FerryColor.textSecondary(),
            )
            Spacer(Modifier.height(FerrySpace.s3))
            Text(
                text = stringResource(R.string.first_run_notification),
                style = FerryFont.body(),
                color = FerryColor.textSecondary(),
            )
            Spacer(Modifier.height(FerrySpace.s6))
            Button(onClick = onContinue) {
                Text(stringResource(R.string.action_continue))
            }
        }
    }
}
