package app.ferry.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringArrayResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.sp
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R

// The six digits, on both devices at once. docs/components.md.
//
// This is the "Showing" state only. docs/components.md's other two rows for
// this component, Confirmed and Mismatched, both replace this view rather
// than restyle it ("the view closes", "an ErrorBlock takes its place"), so
// PairingScreen renders those itself instead of asking PairingCode to.
//
// The code is never in a text field. It is compared by eye, not typed.
@Composable
fun PairingCode(
    code: String,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val digitWords = stringArrayResource(R.array.digit_words)
    val groups = code.split(" ")
    val spokenGroups = groups.joinToString(", ") { group ->
        group.mapNotNull { digit -> digit.digitToIntOrNull()?.let { digitWords[it] } }
            .joinToString(" ")
    }
    val codeDescription = stringResource(R.string.cd_pairing_code, spokenGroups)
    val instruction = stringResource(R.string.pairing_code_instruction)
    val codeStyle = FerryFont.mono().copy(fontSize = 40.sp)

    Column(modifier = modifier) {
        // Two groups, not one string with a space in it: the gap between
        // them is space.3, the token, not whatever a space character
        // happens to measure.
        Row(
            horizontalArrangement = Arrangement.spacedBy(FerrySpace.s3),
            modifier = Modifier.semantics { contentDescription = codeDescription },
        ) {
            for (group in groups) {
                Text(text = group, style = codeStyle, color = FerryColor.text())
            }
        }
        Spacer(Modifier.height(FerrySpace.s3))
        Text(text = instruction, style = FerryFont.body(), color = FerryColor.text())
        Spacer(Modifier.height(FerrySpace.s3))
        Row(horizontalArrangement = Arrangement.spacedBy(FerrySpace.s3)) {
            OutlinedButton(onClick = onCancel) {
                Text(stringResource(R.string.action_cancel))
            }
            Button(onClick = onConfirm) {
                Text(stringResource(R.string.action_confirm))
            }
        }
    }
}
