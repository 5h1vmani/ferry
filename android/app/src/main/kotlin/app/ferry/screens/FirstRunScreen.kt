package app.ferry.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R

// First run. docs-v2/ia.md, First run, phone.
//
// Two things, each with the reason it is needed. Ferry states what it wants
// and why, and then gets out of the way.
//
// The control is "Grant access", not "Continue": rule 9 of docs/voice.md
// bans that word by name, and this button does one nameable thing.
//
// Skip exists because all files access can be refused, and docs-v2/ia.md
// already says Devices must still work when it is. A screen with one
// forward control and no way past it contradicts that. Skipping lands on
// Devices with "Not granted" in Settings, and every transfer then fails
// with words the error table already holds — which is better than refusing
// to open.
//
// The camera is not asked for here. It is asked for when a person taps to
// scan, so first run still asks for two things and a person who pairs by
// code is never asked for a camera at all.
@Composable
fun FirstRunScreen(
    onGrantAccess: () -> Unit,
    onSkip: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(FerrySpace.s5),
    ) {
        Spacer(Modifier.height(FerrySpace.s7))
        Text(
            text = stringResource(R.string.first_run_title),
            style = FerryFont.title(),
            color = FerryColor.text(),
        )
        Spacer(Modifier.height(FerrySpace.s5))

        Requirement(
            icon = Icons.Outlined.Folder,
            title = stringResource(R.string.first_run_all_files_title),
            why = stringResource(R.string.first_run_all_files_why),
        )
        Spacer(Modifier.height(FerrySpace.s5))
        Requirement(
            icon = Icons.Outlined.Notifications,
            title = stringResource(R.string.first_run_notifications_title),
            why = stringResource(R.string.first_run_notifications_why),
        )

        Spacer(Modifier.weight(1f))

        Button(
            onClick = onGrantAccess,
            modifier = Modifier
                .fillMaxWidth()
                .height(MIN_TARGET),
            colors = ButtonDefaults.buttonColors(containerColor = FerryColor.accent()),
        ) {
            Text(stringResource(R.string.first_run_grant))
        }
        Spacer(Modifier.height(FerrySpace.s2))
        TextButton(
            onClick = onSkip,
            modifier = Modifier
                .fillMaxWidth()
                .height(MIN_TARGET),
            colors = ButtonDefaults.textButtonColors(contentColor = FerryColor.accentText()),
        ) {
            Text(stringResource(R.string.first_run_skip))
        }
    }
}

// One thing Ferry needs, and the consequence of not having it. The reason
// is stated rather than implied: a permission prompt with no stated cost is
// a request a person cannot weigh.
@Composable
private fun Requirement(icon: ImageVector, title: String, why: String) {
    Row(verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.Start) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            tint = FerryColor.textSecondary(),
        )
        Spacer(Modifier.width(FerrySpace.s3))
        Column {
            Text(text = title, style = FerryFont.body(), color = FerryColor.text())
            Text(text = why, style = FerryFont.caption(), color = FerryColor.textSecondary())
        }
    }
}

// Material's minimum touch target.
private val MIN_TARGET = 48.dp
