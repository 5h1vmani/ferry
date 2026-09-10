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
import android.os.IBinder
import app.ferry.engine.FerryEngine

// The foreground service that keeps the phone reachable.
//
// While it runs, the engine advertises over mDNS and accepts connections
// from paired devices, and one ongoing notification says so. That
// notification is what Android needs from a foreground service, and it is
// also the honest answer to "is Ferry running" (docs/ia.md).
//
// The service type is dataSync. Android 15 and later limit a dataSync
// foreground service to six hours in a day, after which the system stops
// it. The Pixel 3 XL this build targets runs Android 12, which has no such
// limit, so nothing here works around it yet.
class ReachableService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_TURN_OFF) {
            stopSelf()
            return START_NOT_STICKY
        }
        createChannel()
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

    private fun buildNotification(): Notification {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val turnOff = PendingIntent.getService(
            this,
            1,
            Intent(this, ReachableService::class.java).setAction(ACTION_TURN_OFF),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.notification_reachable_title))
            .setSmallIcon(android.R.drawable.stat_sys_upload)
            .setOngoing(true)
            .setContentIntent(open)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                    getString(R.string.notification_action_turn_off),
                    turnOff,
                ).build(),
            )
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "reachable"
        private const val NOTIFICATION_ID = 1
        private const val ACTION_TURN_OFF = "app.ferry.action.TURN_OFF"

        // True while the service runs. MainActivity reads it to decide
        // whether the engine may be stopped.
        @Volatile
        var running: Boolean = false
            private set

        fun turnOn(context: Context) {
            context.startForegroundService(Intent(context, ReachableService::class.java))
        }

        fun turnOff(context: Context) {
            context.stopService(Intent(context, ReachableService::class.java))
        }
    }
}
