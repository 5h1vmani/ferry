package app.ferry

import android.app.Notification
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.graphics.drawable.Icon
import app.ferry.model.Direction
import app.ferry.model.TransferGroup
import app.ferry.model.TransferState
import app.ferry.model.toUi
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import uniffi.ferry_runtime.BatchInfo
import uniffi.ferry_runtime.TransferInfo

private const val PROGRESS_MAX = 100

// Builds and updates ReachableService's "transfers" notifications: one per
// batch, and one per transfer that belongs to no batch. Split from
// ReachableService.kt, docs/audits/principles.md row P10 and P7.
// docs/ux-fix-plan.md item 2.
//
// One instance per ReachableService, so lastNotified starts empty exactly
// when the service does: audit finding 16, a fresh instance must not
// assume a notification some earlier, now-dead instance posted is still
// tracked.
class TransferNotifier(private val context: Context) {
    private val lastNotified = mutableMapOf<Int, NotifiedState>()

    private data class NotifiedState(val state: TransferState, val percent: Int)

    // One row of the Transfers section, wherever it is: every batch, and
    // every transfer that belongs to no batch. Mirrors
    // model/Mapping.kt's transferGroupsFor, without filtering by device,
    // because a notification is not filed under a device row.
    fun allGroups(batches: List<BatchInfo>, transfers: List<TransferInfo>): List<TransferGroup> {
        val batchGroups = batches.map { it.toUi() }
        val loneGroups = transfers.filter { it.batchId == null }.map { it.toUi() }
        return batchGroups + loneGroups
    }

    // Suspend so it can read isActive on the collector's own coroutine
    // context. ReachableService.onDestroy cancels that coroutine's job but
    // does not join it, so a pass already inside this loop keeps running
    // on Dispatchers.Default after the cancel call returns: docs/audits/
    // principles-fixes.md row 5. The check below stops it from posting
    // once that has happened.
    suspend fun updateTransferNotifications(groups: List<TransferGroup>) {
        val manager = context.getSystemService(NotificationManager::class.java) ?: return
        val currentIds = mutableSetOf<Int>()
        for (group in groups) {
            val id = notificationIdFor(group.id)
            currentIds += id
            val running = group.state == TransferState.Queued ||
                group.state == TransferState.Active ||
                group.state == TransferState.Paused
            val last = lastNotified[id]
            // A running row keeps updating as its percent moves. A row
            // already shown Done or Failed is left alone, so it is not
            // re-posted every time some other transfer changes.
            val changed = last == null || last.state != group.state || (running && last.percent != group.percent)
            if (changed) {
                // The service can have been destroyed since this pass
                // started. Posting an ongoing row after that would leave
                // it with no instance left to cancel it, so this group is
                // skipped instead: docs/audits/principles-fixes.md row 5.
                if (!currentCoroutineContext().isActive) {
                    continue
                }
                manager.notify(id, buildTransferNotification(group))
                lastNotified[id] = NotifiedState(group.state, group.percent)
            }
        }
        val stale = lastNotified.keys - currentIds
        for (id in stale) {
            manager.cancel(id)
            lastNotified.remove(id)
        }
    }

    // A stable id per group, clear of the "reachable" notification's own
    // id. Not private: ReachableService's postRetryFailed uses it too, to
    // post a Retry failure on the same notification a transfer's own row
    // already used.
    fun notificationIdFor(groupId: String): Int {
        val hash = groupId.hashCode() and 0x7fffffff
        return if (hash == ReachableService.NOTIFICATION_ID) hash + 1 else hash
    }

    private fun buildTransferNotification(group: TransferGroup): Notification {
        val open = openAppIntent(context)
        val builder = Notification.Builder(context, ReachableService.TRANSFERS_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentTitle(group.label)
            .setSubText(directionLineFor(group))
            .setContentIntent(open)
            .setOnlyAlertOnce(true)

        when (group.state) {
            TransferState.Queued -> {
                builder.setContentText(context.getString(R.string.progress_queued))
                builder.setProgress(0, 0, true)
                builder.setOngoing(true)
            }

            TransferState.Active -> {
                builder.setContentText(activeLineFor(group))
                builder.setProgress(PROGRESS_MAX, group.percent, false)
                builder.setOngoing(true)
            }

            TransferState.Paused -> {
                builder.setContentText(pausedLineFor(group))
                builder.setProgress(PROGRESS_MAX, group.percent, false)
                builder.setOngoing(true)
            }

            TransferState.Done -> {
                builder.setContentText(doneLineFor(group))
                builder.setOngoing(false)
                builder.setAutoCancel(true)
            }

            TransferState.Failed -> {
                val code = group.errorCode
                val stopped = if (code != null) {
                    errorWordsFor(context, code, group.errorDetail).stopped
                } else {
                    context.getString(R.string.error_unknown_stopped)
                }
                builder.setContentText(stopped)
                builder.setOngoing(false)
                val retry = PendingIntent.getService(
                    context,
                    notificationIdFor(group.id),
                    Intent(context, ReachableService::class.java)
                        .setAction(ReachableService.ACTION_RETRY_TRANSFER)
                        .putExtra(ReachableService.EXTRA_GROUP_ID, group.id)
                        .putExtra(ReachableService.EXTRA_IS_BATCH, !group.isSingleFile),
                    PendingIntent.FLAG_IMMUTABLE,
                )
                builder.addAction(
                    Notification.Action.Builder(
                        Icon.createWithResource(context, android.R.drawable.ic_menu_close_clear_cancel),
                        context.getString(R.string.action_retry),
                        retry,
                    ).build(),
                )
            }
        }
        return builder.build()
    }

    // "Automatic · Phone to Mac", or just the direction: the same words
    // TransferRow's own originAndDirection composes, from strings.xml.
    // buildDirectionLine in TransferLines.kt holds the shared logic.
    private fun directionLineFor(group: TransferGroup): String {
        val direction = when (group.direction) {
            Direction.Pull -> context.getString(R.string.transfers_direction_mac_to_phone)
            Direction.Push -> context.getString(R.string.transfers_direction_phone_to_mac)
        }
        return buildDirectionLine(
            origin = group.origin,
            direction = direction,
            automaticLabel = context.getString(R.string.transfers_origin_automatic),
            dotSeparator = context.getString(R.string.dot_separator),
        )
    }

    // "43 of 120 files · 2.1 GB remaining · 38 MB/s": TransferRow's own
    // activeLine, built the same way without a Compose context.
    private fun activeLineFor(group: TransferGroup): String {
        val filesProgress = if (!group.isSingleFile) {
            context.getString(R.string.transfers_files_progress, group.filesDone, group.filesTotal)
        } else {
            null
        }
        val remaining = (group.bytesTotal - group.bytesDone).coerceAtLeast(0L)
        val remainingText = context.getString(R.string.progress_remaining, formatSize(context, remaining))
        val speed = group.speedMBps
        val speedText = if (speed != null && speed > 0) {
            context.getString(R.string.transport_speed_value, speed)
        } else {
            null
        }
        return buildActiveLine(filesProgress, remainingText, speedText, context.getString(R.string.dot_separator))
    }

    // TransferRow's own pausedLine.
    private fun pausedLineFor(group: TransferGroup): String {
        val code = group.errorCode
        val words = code?.let { errorWordsFor(context, it, group.errorDetail) }
        return buildPausedLine(
            code = code,
            why = words?.why,
            todo = words?.todo,
            pausedPlain = context.getString(R.string.progress_paused_plain),
            pausedTemplate = context.getString(R.string.progress_paused),
        )
    }

    // "12 files · 4.8 GB · 3 min": TransferRow's own doneLine.
    private fun doneLineFor(group: TransferGroup): String {
        val fileCount = if (!group.isSingleFile) {
            context.getString(R.string.transfers_file_count, group.filesTotal)
        } else {
            null
        }
        val size = formatSize(context, group.bytesTotal)
        val duration = group.durationSecs?.let { formatDuration(context, it) }
        return buildDoneLine(fileCount, size, duration, context.getString(R.string.dot_separator))
    }
}
