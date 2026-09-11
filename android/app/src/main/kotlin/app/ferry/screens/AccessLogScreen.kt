package app.ferry.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.ferry.FerryColor
import app.ferry.FerryFont
import app.ferry.FerrySpace
import app.ferry.R
import app.ferry.components.AccessLogRow
import app.ferry.components.EmptyState
import app.ferry.formatDate
import app.ferry.model.AccessDay
import app.ferry.model.DayKind

// The access log. docs-v2/ia.md, The access log. L5, job 9.
//
// What this phone served to a paired device, and what it read from one.
// Ferry is symmetric — either device serves files to the other — so both
// directions are ordinary and neither is filtered out. The direction is a
// word in the sentence, not a tab: a person reading a log is asking what
// happened, not what happened in one direction.
//
// This is the only screen in Ferry a person deliberately goes to. Every
// other one is glanced at or passed through, which is why the log is a
// destination and presence is not.
//
// Nothing here notifies, badges, or judges. It records what happened and
// does not decide that something was wrong. docs-v2/jobs.md, what job 9 is
// not.
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccessLogScreen(
    days: List<AccessDay>,
    // How to name the device behind a key. The log holds keys; only the
    // device list holds names, and a forgotten device's entries outlive it.
    peerNameFor: (String) -> String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Scaffold(
        modifier = modifier,
        containerColor = FerryColor.background(),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.access_log_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Outlined.ArrowBack,
                            contentDescription = stringResource(R.string.cd_back),
                        )
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = FerryColor.surface(),
                ),
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize(),
        ) {
            if (days.isEmpty()) {
                EmptyState(
                    line = stringResource(R.string.access_log_empty),
                    modifier = Modifier.weight(1f),
                )
            } else {
                LazyColumn(modifier = Modifier.weight(1f)) {
                    days.forEach { day ->
                        item(key = day.id) {
                            Text(
                                text = dayTitle(day),
                                style = FerryFont.label(),
                                color = FerryColor.accentText(),
                                modifier = Modifier.padding(
                                    start = FerrySpace.s4,
                                    end = FerrySpace.s4,
                                    top = FerrySpace.s4,
                                    bottom = FerrySpace.s1,
                                ),
                            )
                        }
                        items(day.entries, key = { it.id }) { entry ->
                            AccessLogRow(
                                entry = entry,
                                peerName = peerNameFor(entry.deviceId),
                            )
                            HorizontalDivider(color = FerryColor.border())
                        }
                    }
                }
            }

            // Stated because a log that quietly forgets is worse than no
            // log. The engine prunes; this says so.
            Text(
                text = stringResource(R.string.access_log_retention),
                style = FerryFont.caption(),
                color = FerryColor.textSecondary(),
                modifier = Modifier.padding(FerrySpace.s4),
            )
        }
    }
}

// Which day this group is. Mapping.kt decided it; this only reads the
// words, so no screen computes where midnight falls.
@Composable
private fun dayTitle(day: AccessDay): String = when (day.kind) {
    DayKind.Today -> stringResource(R.string.access_log_today)
    DayKind.Yesterday -> stringResource(R.string.access_log_yesterday)
    DayKind.Earlier -> formatDate(day.atUnixSecs)
}
