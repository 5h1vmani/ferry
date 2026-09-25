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
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import androidx.annotation.RequiresApi
import app.ferry.engine.FerryEngine
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import uniffi.ferry_runtime.FerryException

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
// it and calls onTimeout below, so this service can still end cleanly and
// say why. The Pixel 3 XL this build targets runs Android 12, which has no
// such limit. docs/audits/oss-capability.md M3.
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

    // One instance per service, so the state it tracks for lastNotified
    // starts empty exactly when the service does: audit finding 16, a
    // fresh instance must not assume a notification some earlier, now-dead
    // instance posted is still tracked.
    private val transferNotifier = TransferNotifier(this)

    // Held while the phone advertises and browses over Wi-Fi. Not
    // reference-counted: this service is the one place that acquires and
    // releases it, so a plain acquired/not-acquired state is enough, and a
    // duplicate acquire from a repeat start does not need to be balanced by
    // a duplicate release. docs/audits/oss-capability.md M4: without this
    // lock, Android's Wi-Fi stack drops multicast on many phones, and mDNS
    // is how this phone and a Mac find each other.
    private var multicastLock: WifiManager.MulticastLock? = null

    private fun acquireMulticastLock() {
        if (multicastLock?.isHeld == true) {
            return
        }
        val wifiManager = applicationContext.getSystemService(WifiManager::class.java) ?: return
        val lock = wifiManager.createMulticastLock("app.ferry.mdns")
        lock.setReferenceCounted(false)
        lock.acquire()
        multicastLock = lock
    }

    private fun releaseMulticastLock() {
        multicastLock?.let { if (it.isHeld) it.release() }
        multicastLock = null
    }

    override fun onCreate() {
        super.onCreate()
        createTransfersChannel()
        cancelStaleTransferNotifications()
        notifyJob = notifyScope.launch {
            combine(FerryEngine.batches, FerryEngine.transfers) { batches, transfers ->
                transferNotifier.allGroups(batches, transfers)
            }.collect { groups -> transferNotifier.updateTransferNotifications(groups) }
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
            releaseMulticastLock()
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
        acquireMulticastLock()
        cancelDailyLimitNotice()
        startForeground(
            NOTIFICATION_ID,
            buildNotification(),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
        running = true
        FerryEngine.setReachable(true)
        return START_STICKY
    }

    // Android 15 and later stop a dataSync foreground service after six
    // hours in a day, and this callback is that stop: it cannot be
    // refused. Android crashes the app if the service is still in the
    // foreground a few seconds later, so leaving the foreground and
    // stopping come first. stopSelf() takes no start id, so a start that
    // arrived after the timeout cannot keep the service alive.
    //
    // A plain notification on the "reachable" channel then says why. It
    // carries its own tag, because stopForeground removes the ongoing
    // notification on NOTIFICATION_ID, and Android may do that after this
    // method returns. docs/audits/oss-capability.md M3,
    // docs/audits/android-share-grant.md finding 4.
    @RequiresApi(Build.VERSION_CODES.VANILLA_ICE_CREAM)
    override fun onTimeout(startId: Int, fgsType: Int) {
        super.onTimeout(startId, fgsType)
        advertising = false
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        postDailyLimitNotice()
        FerryEngine.setReachable(false)
        releaseMulticastLock()
        running = false
    }

    private fun postDailyLimitNotice() {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        val open = openAppIntent(this)
        val notification = Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.notification_daily_limit_stopped, Build.MODEL))
            .setContentText(getString(R.string.notification_daily_limit_why_todo))
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        manager.notify(DAILY_LIMIT_TAG, NOTIFICATION_ID, notification)
    }

    // The daily limit notice is out of date once the phone is reachable
    // again, so it goes when advertising starts.
    private fun cancelDailyLimitNotice() {
        getSystemService(NotificationManager::class.java)?.cancel(DAILY_LIMIT_TAG, NOTIFICATION_ID)
    }

    override fun onDestroy() {
        // notifyJob?.cancel() does not join, so a pass already running on
        // Dispatchers.Default can still post after this call returns.
        // updateTransferNotifications now checks isActive before each
        // post and skips once cancelled, but a row it posted before that
        // check would still sit in the shade with no instance left to
        // cancel it. Clearing the whole channel here, the same way
        // onCreate does for a notification a past instance left behind,
        // covers that row too: docs/audits/principles-fixes.md row 5.
        notifyJob?.cancel()
        cancelStaleTransferNotifications()
        FerryEngine.setReachable(false)
        releaseMulticastLock()
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

    // While this service runs the phone is advertising, so the notification
    // states that and offers the one action that changes it. The off state
    // states that instead, with the action that reverses it, so both ends
    // of the switch are reachable from the shade. docs/ia.md, Presence,
    // both platforms.
    //
    // The device is named rather than called "your phone", because a person
    // reading this may have two.
    private fun buildNotification(): Notification {
        val open = openAppIntent(this)
        if (!advertising) {
            val start = PendingIntent.getService(
                this,
                2,
                Intent(this, ReachableService::class.java).setAction(ACTION_START_ADVERTISING),
                PendingIntent.FLAG_IMMUTABLE,
            )
            return Notification.Builder(this, CHANNEL_ID)
                .setContentTitle(getString(R.string.notification_not_advertising, Build.MODEL))
                .setSmallIcon(R.drawable.ic_notification)
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
            .setSmallIcon(R.drawable.ic_notification)
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

    // Audit finding 9. A process death since the notification was posted
    // leaves this service's own engine created but not started, so retry
    // would otherwise fail silently: retry and retryBatch call the
    // engine's own methods, which do nothing before start() has run.
    // FerryEngine.start() records its own failure in the error state every
    // other engine call already shows through, so nothing further is
    // posted here beyond not attempting the retry itself.
    // A Retry tapped from the shade found an engine that could not start,
    // or found no group id at all: docs/audits/principles.md row P16.
    // Nothing else is on screen, so the words go where the tap came from:
    // the same notification, on the same id. docs/voice.md rule 10.
    private fun postRetryFailed(notificationGroupId: String, code: String, detail: String?) {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        val words = errorWordsFor(this, code, detail)
        val open = openAppIntent(this)
        val notification = Notification.Builder(this, TRANSFERS_CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(words.stopped)
            .setContentText(listOf(words.why, words.todo).filter { it.isNotEmpty() }.joinToString(" "))
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        manager.notify(transferNotifier.notificationIdFor(notificationGroupId), notification)
        // This posted over the group's own row, on the group's own id,
        // without going through TransferNotifier. Its record of what that
        // id last showed is now wrong, so the next collector pass would
        // see no change and never redraw the group's own row: docs/audits/
        // principles-fixes.md row 8.
        transferNotifier.forgetLastNotified(notificationGroupId)
    }

    private fun handleRetryAction(intent: Intent) {
        val groupId = intent.getStringExtra(EXTRA_GROUP_ID) ?: run {
            // The extra is missing, which should never happen since this
            // service builds every such intent itself. There is no group
            // to update, so this names the fault as an app-side code, the
            // same way FerryEngine's KeyRenameFailed does, and posts the
            // unknown-code words instead of returning with nothing shown.
            postRetryFailed(RETRY_MISSING_GROUP_ID_CODE, RETRY_MISSING_GROUP_ID_CODE, null)
            return
        }
        if (!FerryEngine.start()) {
            val failure = FerryEngine.error.value as? FerryException.Failed
            postRetryFailed(groupId, failure?.code ?: FerryErrorCode.RUNTIME_NOT_STARTED, failure?.detail)
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

        // Internal: TransferNotifier.notificationIdFor and buildNotification
        // both need this, to keep a transfer's notification id clear of the
        // "reachable" notification's own.
        internal const val NOTIFICATION_ID = 1

        // Tags the daily limit notice, so it never shares a key with the
        // ongoing notification on NOTIFICATION_ID or a transfer's own.
        private const val DAILY_LIMIT_TAG = "daily_limit"
        private const val ACTION_STOP_ADVERTISING = "app.ferry.action.STOP_ADVERTISING"
        private const val ACTION_START_ADVERTISING = "app.ferry.action.START_ADVERTISING"

        // docs/ux-fix-plan.md item 2. A separate channel from "reachable",
        // so a person can silence one without the other. Internal:
        // TransferNotifier.buildTransferNotification posts to it too.
        internal const val TRANSFERS_CHANNEL_ID = "transfers"

        // Not an engine code: there is no entry in the generated table for
        // a Retry tap whose own extra never arrived, so this falls back to
        // the unknown-code words, the same way FerryEngine's
        // KeyRenameFailed does.
        private const val RETRY_MISSING_GROUP_ID_CODE = "Android::RetryMissingGroupId"

        // Internal: TransferNotifier.buildTransferNotification builds the
        // Retry action with these, and handleRetryAction below reads them
        // back out of the intent that action carries.
        internal const val ACTION_RETRY_TRANSFER = "app.ferry.action.RETRY_TRANSFER"
        internal const val EXTRA_GROUP_ID = "app.ferry.extra.GROUP_ID"
        internal const val EXTRA_IS_BATCH = "app.ferry.extra.IS_BATCH"

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

// Opens MainActivity from a tap on any notification either this service or
// TransferNotifier posts. Every notification opens the same screen, so
// this is the one place the intent is built.
fun openAppIntent(context: Context): PendingIntent = PendingIntent.getActivity(
    context,
    0,
    Intent(context, MainActivity::class.java),
    PendingIntent.FLAG_IMMUTABLE,
)
