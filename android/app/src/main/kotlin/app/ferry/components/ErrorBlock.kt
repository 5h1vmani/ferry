package app.ferry.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerryIcon
import app.ferry.FerryRadius
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.model.ThreePartError

// The three-part rule from docs/voice.md, as a view: what stopped, why,
// what to do. A part that is null is left out, never guessed.
//
// Never red: a paused transfer is not dangerous, and neither is anything
// else Ferry shows here. The failed icon and surface_raised carry the
// weight instead.
//
// The action button is a real button, so TalkBack appends ", button" to its
// own label on its own; there is no need to add the word here.
//
// docs/components.md names that button Retry, which is what most errors
// offer. One does not: a missing all files access grant is fixed on a
// system screen, so its block carries "Open settings" instead. The label is
// a parameter for that reason and defaults to Retry.
@Composable
fun ErrorBlock(
    error: ThreePartError,
    onAction: (() -> Unit)? = null,
    actionLabel: String? = null,
    modifier: Modifier = Modifier,
) {
    val description = listOfNotNull(error.stopped, error.why, error.todo).joinToString(" ")

    Row(
        modifier = modifier
            .background(color = FerryColor.surfaceRaised(), shape = RoundedCornerShape(FerryRadius.medium))
            .padding(FerrySpace.s3)
            .semantics(mergeDescendants = true) { contentDescription = description },
    ) {
        Icon(
            imageVector = ferryIconFor(FerryIcon.failed),
            contentDescription = null,
            tint = FerryColor.text(),
        )
        Spacer(Modifier.width(FerrySpace.s2))
        Column {
            Text(
                text = error.stopped,
                style = FerryFont.body(),
                fontWeight = FontWeight.SemiBold,
                color = FerryColor.text(),
            )
            if (error.why != null) {
                Text(text = error.why, style = FerryFont.body(), color = FerryColor.textSecondary())
            }
            if (error.todo != null) {
                Text(text = error.todo, style = FerryFont.body(), color = FerryColor.text())
            }
            if (onAction != null) {
                Spacer(Modifier.height(FerrySpace.s2))
                OutlinedButton(
                    onClick = onAction,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = FerryColor.accent()),
                ) {
                    Text(actionLabel ?: stringResource(R.string.action_retry))
                }
            }
        }
    }
}
