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
// The off state is a state of this service, not its absence. docs-v2/ia.md,
// Presence, both platforms, promises a notification with "Not advertising"
// and a "Start advertising" action. Tapping Stop from the shade, or
// turning the switch off inside the app, both leave a way back without
// opening Ferry. The service ends only when the engine cannot start, or
// the process itself dies. Toggling advertising off never stops it.
//
// The notification never shows the quiet-on-this-network line. Both its
// lines show only in Settings, Networks: docs-v2/ia.md, Presence, both
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

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
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

    // While this service runs the phone is advertising, so the notification
    // states that and offers the one action that changes it. The off state
    // states that instead, with the action that reverses it, so both ends
    // of the switch are reachable from the shade. docs-v2/ia.md, Presence,
    // both platforms.
    //
    // The device is named rather than called "your phone", because a person
    // reading this may have two.
    private fun buildNotification(): Notification {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
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

    companion object {
        private const val CHANNEL_ID = "reachable"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_STOP_ADVERTISING = "app.ferry.action.STOP_ADVERTISING"
        private const val ACTION_START_ADVERTISING = "app.ferry.action.START_ADVERTISING"

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
