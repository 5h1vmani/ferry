package app.ferry

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.os.Build
import android.os.IBinder
import app.ferry.engine.FerryEngine
import app.ferry.model.Direction
import app.ferry.model.TransferGroup
import app.ferry.model.TransferState
import app.ferry.model.toUi
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import uniffi.ferry_runtime.FerryException
import uniffi.ferry_runtime.BatchInfo
import uniffi.ferry_runtime.TransferInfo

// The foreground service that keeps the phone reachable, and the
// notification that says so.
//
// That notification is what Android needs from a foreground service, and it
// is also presence — the honest answer to "is Ferry running", available
// without opening anything. It is one of the two surfaces PresenceControl's
// value reaches on this phone; the other is the row on Devices. Both say
// the same words about the same fact, which is why those words live in
// strings.xml and not in either of them.
//
// The off state is a state of this service, not its absence. docs/ia.md,
// Presence, both platforms, promises a notification with "Not advertising"
// and a "Start advertising" action. Tapping Stop from the shade, or
// turning the switch off inside the app, both leave a way back without
// opening Ferry. The service ends only when the engine cannot start, or
// the process itself dies. Toggling advertising off never stops it.
//
// The notification never shows the quiet-on-this-network line. Both its
// lines show only in Settings, Networks: docs/ia.md, Presence, both
// platforms.
//
// The service type is dataSync. Android 15 and later limit a dataSync
// foreground service to six hours in a day, after which the system stops
// it. The Pixel 3 XL this build targets runs Android 12, which has no such
// limit, so nothing here works around it yet.
class ReachableService : Service() {
    // True while the phone advertises. False in the off state, which this
    // service holds rather than ending.
    private var advertising = false

    // Watches every transfer and batch while this service is alive, and
    // posts the "transfers" notifications: docs/ux-fix-plan.md item 2. A
    // plain scope, not one scoped to this service's own lifecycle, so
    // cancelling it in onDestroy is this object's own choice, the same
    // reasoning FerryEngine.scope documents.
    private val notifyScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var notifyJob: Job? = null

    // The state last posted for each notification id, so a row already
    // shown Done or Failed is not re-posted on every later change to some
    // other transfer, while a running row keeps updating as its percent
    // moves.
    private val lastNotified = mutableMapOf<Int, NotifiedState>()

    private data class NotifiedState(val state: TransferState, val percent: Int)

    override fun onCreate() {
        super.onCreate()
        createTransfersChannel()
        cancelStaleTransferNotifications()
        notifyJob = notifyScope.launch {
            combine(FerryEngine.batches, FerryEngine.transfers) { batches, transfers ->
                allGroups(batches, transfers)
            }.collect { groups -> updateTransferNotifications(groups) }
        }
    }

    // A running transfer's notification is ongoing, but this service
    // instance is gone if the process died mid-transfer, with no one left
    // to cancel it: audit finding 16. lastNotified starts empty on a fresh
    // instance too, so without this a stale notification would sit in the
    // shade forever, never matching a state this instance thinks it holds.
    private fun cancelStaleTransferNotifications() {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        for (posted in manager.activeNotifications) {
            if (posted.notification.channelId == TRANSFERS_CHANNEL_ID) {
                manager.cancel(posted.id)
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_RETRY_TRANSFER) {
            handleRetryAction(intent)
            // Audit finding 9. Android keeps whichever value onStartCommand
            // last returned, so this used to turn off the sticky restart
            // for the whole service on one Retry tap.
            return START_STICKY
        }
        if (intent?.action == ACTION_STOP_ADVERTISING) {
            advertising = false
            FerryEngine.setReachable(false)
            createChannel()
            startForeground(
                NOTIFICATION_ID,
                buildNotification(),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
            running = true
            return START_STICKY
        }
        // Every other action, including none (an ordinary start) and
        // ACTION_START_ADVERTISING (the off notification's own action),
        // starts or resumes advertising. A restarted service has no
        // activity to call start() for it. If the engine cannot start,
        // most likely because all files access is not granted, this must
        // stop rather than post a notification that says the phone
        // advertises when it does not. Once started, a repeat call is a
        // no-op that returns true.
        if (!FerryEngine.start()) {
            // A service started with startForegroundService must call
            // startForeground before it may stop, or Android kills the
            // whole process. Show the off notification for the instant
            // it takes to stop, then remove it.
            createChannel()
            advertising = false
            startForeground(
                NOTIFICATION_ID,
                buildNotification(),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return START_NOT_STICKY
        }
        createChannel()
        advertising = true
        startForeground(
            NOTIFICATION_ID,
            buildNotification(),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
        running = true
        FerryEngine.setReachable(true)
        return START_STICKY
    }

    override fun onDestroy() {
        notifyJob?.cancel()
        FerryEngine.setReachable(false)
        running = false
        // The activity can have finished while this service kept running.
        // Stopping here is the only chance left to release the engine in
        // that case, so the lock file does not stay held with nothing on
        // screen and no service either.
        //
        // stop() joins every worker and can take several seconds against a
        // peer that stopped answering. This runs on the main thread same as
        // MainActivity.onDestroy, and for the same reason: a plain thread,
        // not a coroutine, because a coroutine scoped to this service would
        // be cancelled by this same onDestroy.
        val app = application as? FerryApplication
        if (app == null || !app.hasActivity()) {
            Thread { FerryEngine.stop() }.start()
        }
        super.onDestroy()
    }

    private fun createChannel() {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        val channel = NotificationChannel(
            CHANNEL_ID,
            getString(R.string.notification_channel_reachable),
            NotificationManager.IMPORTANCE_LOW,
        )
        manager.createNotificationChannel(channel)
    }

    // docs/ux-fix-plan.md item 2. A transfer starting or finishing is worth
    // a heads-up the first time; setOnlyAlertOnce on every notification
    // built from it keeps a moving progress bar from alerting again.
    private fun createTransfersChannel() {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        val channel = NotificationChannel(
            TRANSFERS_CHANNEL_ID,
            getString(R.string.notification_channel_transfers),
            NotificationManager.IMPORTANCE_DEFAULT,
        )
        manager.createNotificationChannel(channel)
    }

    // Opens MainActivity from a tap on any notification this service posts.
    // Every notification opens the same screen, so this is the one place
    // the intent is built.
    private fun openAppIntent(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java),
        PendingIntent.FLAG_IMMUTABLE,
    )

    // While this service runs the phone is advertising, so the notification
    // states that and offers the one action that changes it. The off state
    // states that instead, with the action that reverses it, so both ends
    // of the switch are reachable from the shade. docs/ia.md, Presence,
    // both platforms.
    //
    // The device is named rather than called "your phone", because a person
    // reading this may have two.
    private fun buildNotification(): Notification {
        val open = openAppIntent()
        if (!advertising) {
            val start = PendingIntent.getService(
                this,
                2,
                Intent(this, ReachableService::class.java).setAction(ACTION_START_ADVERTISING),
                PendingIntent.FLAG_IMMUTABLE,
            )
            return Notification.Builder(this, CHANNEL_ID)
                .setContentTitle(getString(R.string.notification_not_advertising, Build.MODEL))
                .setSmallIcon(android.R.drawable.stat_sys_upload)
                .setOngoing(true)
                .setContentIntent(open)
                .addAction(
                    Notification.Action.Builder(
                        Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                        getString(R.string.presence_start),
                        start,
                    ).build(),
                )
                .build()
        }
        val stop = PendingIntent.getService(
            this,
            1,
            Intent(this, ReachableService::class.java).setAction(ACTION_STOP_ADVERTISING),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.notification_advertising, Build.MODEL))
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setOngoing(true)
            .setContentIntent(open)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                    getString(R.string.presence_stop),
                    stop,
                ).build(),
            )
            .build()
    }

    // ---- Transfer notifications: docs/ux-fix-plan.md item 2 ----
    //
    // One row of the Transfers section, wherever it is: every batch, and
    // every transfer that belongs to no batch. Mirrors
    // model/Mapping.kt's transferGroupsFor, without filtering by device,
    // because a notification is not filed under a device row.
    private fun allGroups(batches: List<BatchInfo>, transfers: List<TransferInfo>): List<TransferGroup> {
        val batchGroups = batches.map { it.toUi() }
        val loneGroups = transfers.filter { it.batchId == null }.map { it.toUi() }
        return batchGroups + loneGroups
    }

    private fun updateTransferNotifications(groups: List<TransferGroup>) {
        val manager = getSystemService(NotificationManager::class.java) ?: return
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

    // A stable id per group, clear of the "reachable" notification's own id.
    private fun notificationIdFor(groupId: String): Int {
        val hash = groupId.hashCode() and 0x7fffffff
        return if (hash == NOTIFICATION_ID) hash + 1 else hash
    }

    private fun buildTransferNotification(group: TransferGroup): Notification {
        val open = openAppIntent()
        val builder = Notification.Builder(this, TRANSFERS_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentTitle(group.label)
            .setSubText(directionLineFor(group))
            .setContentIntent(open)
            .setOnlyAlertOnce(true)

        when (group.state) {
            TransferState.Queued -> {
                builder.setContentText(getString(R.string.progress_queued))
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
                    errorWordsFor(code, group.errorDetail).stopped
                } else {
                    getString(R.string.error_unknown_stopped)
                }
                builder.setContentText(stopped)
                builder.setOngoing(false)
                val retry = PendingIntent.getService(
                    this,
                    notificationIdFor(group.id),
                    Intent(this, ReachableService::class.java)
                        .setAction(ACTION_RETRY_TRANSFER)
                        .putExtra(EXTRA_GROUP_ID, group.id)
                        .putExtra(EXTRA_IS_BATCH, !group.isSingleFile),
                    PendingIntent.FLAG_IMMUTABLE,
                )
                builder.addAction(
                    Notification.Action.Builder(
                        Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                        getString(R.string.action_retry),
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
            Direction.Pull -> getString(R.string.transfers_direction_mac_to_phone)
            Direction.Push -> getString(R.string.transfers_direction_phone_to_mac)
        }
        return buildDirectionLine(
            origin = group.origin,
            direction = direction,
            automaticLabel = getString(R.string.transfers_origin_automatic),
            dotSeparator = getString(R.string.dot_separator),
        )
    }

    // "43 of 120 files · 2.1 GB remaining · 38 MB/s": TransferRow's own
    // activeLine, built the same way without a Compose context.
    private fun activeLineFor(group: TransferGroup): String {
        val filesProgress = if (!group.isSingleFile) {
            getString(R.string.transfers_files_progress, group.filesDone, group.filesTotal)
        } else {
            null
        }
        val remaining = (group.bytesTotal - group.bytesDone).coerceAtLeast(0L)
        val remainingText = getString(R.string.progress_remaining, formatSize(this, remaining))
        val speed = group.speedMBps
        val speedText = if (speed != null && speed > 0) {
            getString(R.string.transport_speed_value, speed)
        } else {
            null
        }
        return buildActiveLine(filesProgress, remainingText, speedText, getString(R.string.dot_separator))
    }

    // TransferRow's own pausedLine.
    private fun pausedLineFor(group: TransferGroup): String {
        val code = group.errorCode
        val words = code?.let { errorWordsFor(it, group.errorDetail) }
        return buildPausedLine(
            code = code,
            why = words?.why,
            todo = words?.todo,
            pausedPlain = getString(R.string.progress_paused_plain),
            pausedTemplate = getString(R.string.progress_paused),
        )
    }

    // "12 files · 4.8 GB · 3 min": TransferRow's own doneLine.
    private fun doneLineFor(group: TransferGroup): String {
        val fileCount = if (!group.isSingleFile) {
            getString(R.string.transfers_file_count, group.filesTotal)
        } else {
            null
        }
        val size = formatSize(this, group.bytesTotal)
        val duration = group.durationSecs?.let { formatDuration(this, it) }
        return buildDoneLine(fileCount, size, duration, getString(R.string.dot_separator))
    }

    // The three-part words for one error code, from the generated table,
    // with `{detail}` filled the same way ErrorWords.kt fills it for
    // ErrorBlock. Falls back to the unknown-code words for a code the
    // table does not hold. ErrorWords.kt's buildThreePartError does the
    // lookup, fallback, fill, and trim; this wrapper resolves the
    // unknown-code words through getString, since a service has no
    // Compose context, and turns a null why or todo back into an empty
    // part, which is what FerryErrors.Words holds.
    private fun errorWordsFor(code: String, detail: String?): FerryErrors.Words {
        val words = buildThreePartError(
            code = code,
            detail = detail,
            unknownStopped = getString(R.string.error_unknown_stopped),
            unknownWhy = getString(R.string.error_unknown_why, code),
            unknownTodo = getString(R.string.error_unknown_todo),
        )
        return FerryErrors.Words(
            stopped = words.stopped,
            why = words.why.orEmpty(),
            todo = words.todo.orEmpty(),
        )
    }

    // Audit finding 9. A process death since the notification was posted
    // leaves this service's own engine created but not started, so retry
    // would otherwise fail silently: retry and retryBatch call the
    // engine's own methods, which do nothing before start() has run.
    // FerryEngine.start() records its own failure in the error state every
    // other engine call already shows through, so nothing further is
    // posted here beyond not attempting the retry itself.
    // A Retry tapped from the shade found an engine that could not start.
    // Nothing else is on screen, so the words go where the tap came from:
    // the same notification, on the same id. docs/voice.md rule 10.
    private fun postRetryFailed(groupId: String) {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        val failure = FerryEngine.error.value as? FerryException.Failed
        val words = if (failure != null) {
            errorWordsFor(failure.code, failure.detail)
        } else {
            errorWordsFor(FerryErrorCode.RUNTIME_NOT_STARTED, null)
        }
        val open = openAppIntent()
        val notification = Notification.Builder(this, TRANSFERS_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setContentTitle(words.stopped)
            .setContentText(listOf(words.why, words.todo).filter { it.isNotEmpty() }.joinToString(" "))
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        manager.notify(notificationIdFor(groupId), notification)
    }

    private fun handleRetryAction(intent: Intent) {
        val groupId = intent.getStringExtra(EXTRA_GROUP_ID) ?: return
        if (!FerryEngine.start()) {
            postRetryFailed(groupId)
            return
        }
        if (intent.getBooleanExtra(EXTRA_IS_BATCH, false)) {
            FerryEngine.retryBatch(groupId)
        } else {
            FerryEngine.retry(groupId)
        }
    }

    companion object {
        private const val CHANNEL_ID = "reachable"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_STOP_ADVERTISING = "app.ferry.action.STOP_ADVERTISING"
        private const val ACTION_START_ADVERTISING = "app.ferry.action.START_ADVERTISING"

        // docs/ux-fix-plan.md item 2. A separate channel from "reachable",
        // so a person can silence one without the other.
        private const val TRANSFERS_CHANNEL_ID = "transfers"
        private const val PROGRESS_MAX = 100
        private const val ACTION_RETRY_TRANSFER = "app.ferry.action.RETRY_TRANSFER"
        private const val EXTRA_GROUP_ID = "app.ferry.extra.GROUP_ID"
        private const val EXTRA_IS_BATCH = "app.ferry.extra.IS_BATCH"

        // True while the service runs, in either state. MainActivity reads
        // it to decide whether the engine may be stopped.
        @Volatile
        var running: Boolean = false
            private set

        fun turnOn(context: Context) {
            context.startForegroundService(Intent(context, ReachableService::class.java))
        }

        // Sent whether the tap came from the app's own switch or the
        // notification's own action: both mean the same thing, off. This
        // does not stop the service. The off state is a state of it, not
        // its absence, so the notification stays, with a way back.
        fun turnOff(context: Context) {
            context.startService(
                Intent(context, ReachableService::class.java).setAction(ACTION_STOP_ADVERTISING),
            )
        }
    }
}
