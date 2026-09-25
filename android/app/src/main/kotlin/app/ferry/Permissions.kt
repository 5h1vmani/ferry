package app.ferry

import android.Manifest
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import androidx.core.content.ContextCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

// What Android has granted this app, and the screens that change it.
//
// Every value can change while the app is in the background, because the
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

    private val _camera = MutableStateFlow(false)

    // True when this app may use the camera. Only pairing by scan needs
    // it, and it is asked for at the moment a person taps to scan — never
    // at first run, so a person who pairs by code is never asked at all.
    val camera: StateFlow<Boolean> = _camera.asStateFlow()

    private val _location = MutableStateFlow(false)

    // True when this app may read the phone's fine location. Only the
    // Wi-Fi network name needs it, and only pairing needs the name: it is
    // asked for the first time pairing starts, never at first run.
    val location: StateFlow<Boolean> = _location.asStateFlow()

    private val _locationApproximateOnly = MutableStateFlow(false)

    // True when the person allowed approximate location but not precise
    // location. Android gives the Wi-Fi name only with precise location,
    // so the name stays unknown, and Settings says which choice to change.
    // docs/audits/android-share-grant.md finding 5.
    val locationApproximateOnly: StateFlow<Boolean> = _locationApproximateOnly.asStateFlow()

    private val _cameraRefused = MutableStateFlow(false)

    // True once the person has refused the camera in this run. The pairing
    // screen then states that plainly and offers the code method, instead
    // of showing a camera that will never open.
    val cameraRefused: StateFlow<Boolean> = _cameraRefused.asStateFlow()

    private val _firstRunDone = MutableStateFlow(false)

    // True once the person has passed the first run screen, by granting or
    // by skipping. From then on Devices is shown, and it carries an error
    // block while all files access is still missing.
    val firstRunDone: StateFlow<Boolean> = _firstRunDone.asStateFlow()

    // Reads the current state from Android.
    fun refresh(context: Context) {
        _allFilesAccess.value = Environment.isExternalStorageManager()
        val manager = context.getSystemService(NotificationManager::class.java)
        _notifications.value = manager?.areNotificationsEnabled() ?: false
        val camera = ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA)
        _camera.value = camera == PackageManager.PERMISSION_GRANTED
        val location = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.ACCESS_FINE_LOCATION,
        )
        _location.value = location == PackageManager.PERMISSION_GRANTED
        val coarseLocation = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.ACCESS_COARSE_LOCATION,
        )
        _locationApproximateOnly.value =
            !_location.value && coarseLocation == PackageManager.PERMISSION_GRANTED
        if (_camera.value) {
            // A grant made on the system screen clears an earlier refusal,
            // so the pairing screen stops offering the fallback as if it
            // were the only way.
            _cameraRefused.value = false
        }
        if (_allFilesAccess.value) {
            _firstRunDone.value = true
        }
    }

    // Records that the person has passed the first run screen.
    fun markFirstRunDone() {
        _firstRunDone.value = true
    }

    // Records the answer to the camera prompt.
    fun cameraAnswered(granted: Boolean) {
        _camera.value = granted
        _cameraRefused.value = !granted
    }

    // Records the answer to the location prompt: precise, approximate
    // only, or refused. Only precise location reads the network name.
    // Neither of the other two is an error: the network name stays
    // unknown, and Settings says why.
    fun locationAnswered(fine: Boolean, coarse: Boolean) {
        _location.value = fine
        _locationApproximateOnly.value = !fine && coarse
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

    // This app's own settings page, where a refused camera or a refused
    // location is turned back on. Android stops showing either prompt
    // after two refusals, so this is the only way back from there.
    fun appSettingsIntent(context: Context): Intent = Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.parse("package:" + context.packageName),
    )

    // True on Android 13 and later, which is where POST_NOTIFICATIONS is
    // asked for at runtime.
    fun asksForNotifications(): Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
}
