package app.ferry

import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

// What Android has granted this app, and the screens that change it.
//
// Both values can change while the app is in the background, because the
// person changes them on a system screen. So MainActivity calls refresh on
// every resume and the screens read these flows.
object Permissions {
    private val _allFilesAccess = MutableStateFlow(false)

    // True when the person has granted all files access. The engine cannot
    // open the shared root without it.
    val allFilesAccess: StateFlow<Boolean> = _allFilesAccess.asStateFlow()

    private val _notifications = MutableStateFlow(false)

    // True when this app may post notifications. The reachable service
    // needs it to show its ongoing notification.
    val notifications: StateFlow<Boolean> = _notifications.asStateFlow()

    private val _firstRunDone = MutableStateFlow(false)

    // True once the person has used the Continue control on the first run
    // screen. From then on Devices is shown, and it carries an error block
    // while all files access is still missing.
    val firstRunDone: StateFlow<Boolean> = _firstRunDone.asStateFlow()

    // Reads the current state from Android.
    fun refresh(context: Context) {
        _allFilesAccess.value = Environment.isExternalStorageManager()
        val manager = context.getSystemService(NotificationManager::class.java)
        _notifications.value = manager?.areNotificationsEnabled() ?: false
        if (_allFilesAccess.value) {
            _firstRunDone.value = true
        }
    }

    // Records that the person has passed the first run screen.
    fun markFirstRunDone() {
        _firstRunDone.value = true
    }

    // The system screen where all files access is granted for this app.
    fun allFilesAccessIntent(context: Context): Intent = Intent(
        Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
        Uri.parse("package:" + context.packageName),
    )

    // This app's own notification settings.
    fun notificationSettingsIntent(context: Context): Intent =
        Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)

    // True on Android 13 and later, which is where POST_NOTIFICATIONS is
    // asked for at runtime.
    fun asksForNotifications(): Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
}
