package app.ferry

import android.Manifest
import android.app.ComponentCaller
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.annotation.RequiresApi
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import app.ferry.engine.FerryEngine
import app.ferry.engine.pushShared

// The one activity. Everything else is Compose, switched by FerryApp's own
// screen state, not by the Android framework's back stack.
//
// The activity owns everything that needs an Activity: the system screens
// for all files access, notifications and this app's own settings, the
// three runtime prompts, and the engine's start and stop.
class MainActivity : ComponentActivity() {
    private lateinit var notificationPrompt: ActivityResultLauncher<String>
    private lateinit var cameraPrompt: ActivityResultLauncher<String>

    // docs/audits/oss-capability.md M1. Android ignores a request for fine
    // location that does not also ask for coarse location, on an app that
    // targets API 31 or later, so both are requested together.
    private lateinit var locationPrompt: ActivityResultLauncher<Array<String>>

    // The Devices screen's "Send files" control, docs/ux-fix-plan.md item 1.
    // Opens the system picker for one or more documents, and feeds the
    // chosen paths through the same call a share from another app uses.
    private lateinit var sendFilesPrompt: ActivityResultLauncher<Array<String>>

    // True once the person has granted on the first run screen. The
    // notification prompt follows the all files access screen, which is
    // step 3 of the phone first run in docs/ia.md.
    private var cameFromFirstRun = false

    // Each prompt is shown once per run of the process. Android itself
    // refuses to show one again after two refusals, so asking again on
    // every resume would do nothing.
    private var notificationPromptShown = false
    private var cameraPromptShown = false

    // Asked for the first time pairing starts, in the same shape as the
    // two prompts above: once per run of the process, because Android
    // itself stops showing a prompt again after two refusals.
    private var locationPromptShown = false

    // Set when advertising went on and the notification prompt had to come
    // first. The service starts as soon as the prompt is answered.
    private var startServiceAfterPrompt = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (savedInstanceState != null) {
            cameFromFirstRun = savedInstanceState.getBoolean(KEY_CAME_FROM_FIRST_RUN)
            notificationPromptShown = savedInstanceState.getBoolean(KEY_NOTIFICATION_PROMPT_SHOWN)
            cameraPromptShown = savedInstanceState.getBoolean(KEY_CAMERA_PROMPT_SHOWN)
            locationPromptShown = savedInstanceState.getBoolean(KEY_LOCATION_PROMPT_SHOWN)
            startServiceAfterPrompt = savedInstanceState.getBoolean(KEY_START_SERVICE_AFTER_PROMPT)
        }
        notificationPrompt = registerForActivityResult(
            ActivityResultContracts.RequestPermission(),
        ) {
            Permissions.refresh(this)
            if (startServiceAfterPrompt) {
                startServiceAfterPrompt = false
                ReachableService.turnOn(this)
            }
        }
        cameraPrompt = registerForActivityResult(
            ActivityResultContracts.RequestPermission(),
        ) { granted ->
            Permissions.cameraAnswered(granted)
            if (granted) {
                // The tap that asked for the camera was a tap to scan, so
                // granting it starts the scan rather than making a person
                // tap the same control twice.
                FerryEngine.startPairing(app.ferry.model.PairingMethod.Scan)
            }
        }
        locationPrompt = registerForActivityResult(
            ActivityResultContracts.RequestMultiplePermissions(),
        ) { grants ->
            // Only fine location unlocks the Wi-Fi network name; coarse is
            // requested alongside it only because Android otherwise
            // ignores the fine request. docs/audits/oss-capability.md M1.
            val granted = grants[Manifest.permission.ACCESS_FINE_LOCATION] == true
            Permissions.locationAnswered(granted)
            // A callback registered before this grant never carries the
            // name, so a grant here is exactly the edge NetworkName has to
            // re-register for.
            NetworkName.refresh(this)
        }
        sendFilesPrompt = registerForActivityResult(
            ActivityResultContracts.OpenMultipleDocuments(),
        ) { uris ->
            // The person chose each file in the system picker, and the
            // picker grants Ferry each one it returns. No other app is
            // involved, so rule 3 of ShareIntake's check always passes.
            handleSharedUris(uris) { true }
        }
        setContent {
            FerryApp(
                onGrantFirstRunAccess = ::grantFirstRunAccess,
                onSkipFirstRun = { Permissions.markFirstRunDone() },
                onOpenAllFilesAccess = { startActivity(Permissions.allFilesAccessIntent(this)) },
                onOpenNotificationSettings = {
                    startActivity(Permissions.notificationSettingsIntent(this))
                },
                onOpenAppSettings = { startActivity(Permissions.appSettingsIntent(this)) },
                onRequestCamera = ::requestCamera,
                onRequestLocation = ::requestLocationIfNeeded,
                onSetAdvertising = ::setAdvertising,
                onSendFilesClick = { sendFilesPrompt.launch(arrayOf("*/*")) },
            )
        }
        handleIntentIfShare(intent, fromNewIntent = false)
    }

    // A share from another app arrives here when this activity is not
    // already the front of the task; singleTop in the manifest sends a
    // second one to onNewIntent instead of a new instance, so one share
    // never builds a second engine.
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIntentIfShare(intent, fromNewIntent = true)
    }

    // docs/ux-fix-plan.md item 1. Every path resolves off the main thread,
    // because a content URI can mean a disk copy, and the push itself
    // dials the Mac. FerryEngine.pushShared does nothing when no Mac is
    // paired; requestNavigateHome shows Devices either way, since that is
    // where the empty state, and the pushed transfer, both are.
    private fun handleIntentIfShare(intent: Intent, fromNewIntent: Boolean) {
        if (!ShareIntake.isShareIntent(intent)) {
            return
        }
        handleSharedUris(ShareIntake.urisFrom(intent), shareSenderCheck(fromNewIntent))
    }

    // Rule 3 of ShareIntake's share check: can the app that sent this
    // share read a URI itself? docs/audits/android-share-grant.md finding 1.
    //
    // Android 15 added ComponentCaller for this. Android records what the
    // sender could read at the moment the share arrived. The system share
    // sheet launches Ferry as the sender, so the answer is about the app
    // the person shared from. Android keeps one caller for the launch and
    // one for each onNewIntent. currentCaller works only inside
    // onNewIntent, so the caller is taken here, on the main thread.
    //
    // Android 14 and earlier have no such API, so the check passes every
    // URI there, and ShareIntake's grant check is the only proof.
    private fun shareSenderCheck(fromNewIntent: Boolean): (Uri) -> Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.VANILLA_ICE_CREAM) {
            return { true }
        }
        val caller = try {
            if (fromNewIntent) currentCaller else initialCaller
        } catch (e: IllegalStateException) {
            // No caller was recorded. Without one, nothing can prove the
            // sender could read the files, so every URI is refused.
            android.util.Log.w("Ferry", "no caller recorded for this share", e)
            return { false }
        }
        return { uri -> callerCanRead(caller, uri) }
    }

    // Asks Android whether the caller could read this URI when it sent the
    // share. Android throws SecurityException when Ferry itself has no
    // access to the URI, and IllegalArgumentException for a URI the
    // caller's intent did not carry. Both mean no.
    @RequiresApi(Build.VERSION_CODES.VANILLA_ICE_CREAM)
    private fun callerCanRead(caller: ComponentCaller, uri: Uri): Boolean = try {
        caller.checkContentUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION) ==
            PackageManager.PERMISSION_GRANTED
    } catch (e: SecurityException) {
        false
    } catch (e: IllegalArgumentException) {
        false
    }

    private fun handleSharedUris(uris: List<Uri>, senderCanRead: (Uri) -> Boolean) {
        if (uris.isEmpty()) {
            return
        }
        ShareIntake.requestNavigateHome()
        val context = applicationContext
        ShareIntake.launchOnIo {
            try {
                // A file that cannot be read stops the whole share: nothing
                // is sent, and ShareIntake.appError already carries why,
                // for FerryApp to show through ErrorBlock. docs/voice.md
                // rule 10.
                val resolution = ShareIntake.resolve(context, uris, senderCanRead)
                if (resolution is ShareIntake.Resolution.Success && resolution.localPaths.isNotEmpty()) {
                    FerryEngine.pushShared(resolution.localPaths)
                }
            } catch (t: Throwable) {
                // Audit finding 2. Any app on the phone can hand a share
                // intent to Ferry; a fault this deep must not crash the
                // process. There is no one file to name here, so the error
                // says as much through the same path a per-file failure
                // uses.
                android.util.Log.w("Ferry", "share handling failed", t)
                ShareIntake.setAppError(
                    ShareIntake.AppError.Unreadable(context.getString(R.string.share_generic_file_name)),
                )
            }
        }
    }

    override fun onResume() {
        super.onResume()
        Permissions.refresh(this)
        // Picks up a location grant made on the system screen since the
        // last resume: the network callback registered before it exists
        // has to be remade to carry the name.
        NetworkName.refresh(this)
        // The process can outlive an activity that finished and stopped the
        // engine. This builds a fresh one in that case, and does nothing in
        // the ordinary one.
        FerryEngine.create(this)
        if (cameFromFirstRun && Permissions.asksForNotifications() && !notificationPromptShown &&
            !Permissions.notifications.value
        ) {
            cameFromFirstRun = false
            notificationPromptShown = true
            notificationPrompt.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        // The engine's start opens the shared root, so it can only succeed
        // once all files access is granted. Trying again on every resume is
        // what turns a grant made on the system screen into a running
        // engine, with nothing else to press.
        if (Permissions.allFilesAccess.value) {
            FerryEngine.start()
        }
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        outState.putBoolean(KEY_CAME_FROM_FIRST_RUN, cameFromFirstRun)
        outState.putBoolean(KEY_NOTIFICATION_PROMPT_SHOWN, notificationPromptShown)
        outState.putBoolean(KEY_CAMERA_PROMPT_SHOWN, cameraPromptShown)
        outState.putBoolean(KEY_LOCATION_PROMPT_SHOWN, locationPromptShown)
        outState.putBoolean(KEY_START_SERVICE_AFTER_PROMPT, startServiceAfterPrompt)
    }

    override fun onDestroy() {
        super.onDestroy()
        // The engine lives as long as the process, and dropping it does not
        // end it. It is stopped only when nothing is left to serve: no
        // reachable service, and this activity going away for good.
        //
        // stop() opens a loopback connection to wake the accept loop and
        // joins every thread, which can take a moment. A plain thread runs
        // it here, not a coroutine, because a coroutine on this activity's
        // own scope would be cancelled by the same onDestroy.
        if (isFinishing && !ReachableService.running) {
            Thread { FerryEngine.stop() }.start()
        }
    }

    private fun grantFirstRunAccess() {
        cameFromFirstRun = true
        Permissions.markFirstRunDone()
        startActivity(Permissions.allFilesAccessIntent(this))
    }

    // Asked for when a person taps to scan, never at first run. Refused
    // twice, Android stops showing the prompt, so the pairing screen offers
    // this app's settings page and the code method instead.
    private fun requestCamera() {
        if (cameraPromptShown) {
            Permissions.cameraAnswered(false)
            return
        }
        cameraPromptShown = true
        cameraPrompt.launch(Manifest.permission.CAMERA)
    }

    // Asked for the first time pairing starts, never at first run, in the
    // same shape as requestCamera: once per run of the process, and never
    // asked again once granted. docs/engine-contract.md item 18. Refused
    // is not an error; Settings and the presence surfaces state why the
    // network name stays unknown.
    private fun requestLocationIfNeeded() {
        if (locationPromptShown || Permissions.location.value) {
            return
        }
        locationPromptShown = true
        locationPrompt.launch(
            arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION),
        )
    }

    private fun setAdvertising(on: Boolean) {
        if (!on) {
            ReachableService.turnOff(this)
            return
        }
        // Android 13 and later need POST_NOTIFICATIONS before the service
        // can show its ongoing notification, so the prompt comes first.
        if (Permissions.asksForNotifications() && !Permissions.notifications.value &&
            !notificationPromptShown
        ) {
            notificationPromptShown = true
            startServiceAfterPrompt = true
            notificationPrompt.launch(Manifest.permission.POST_NOTIFICATIONS)
            return
        }
        ReachableService.turnOn(this)
    }

    private companion object {
        // Keys for the five flags saved across a recreation, so a system
        // prompt or a system screen in front does not lose a decision this
        // activity already made this run.
        const val KEY_CAME_FROM_FIRST_RUN = "cameFromFirstRun"
        const val KEY_NOTIFICATION_PROMPT_SHOWN = "notificationPromptShown"
        const val KEY_CAMERA_PROMPT_SHOWN = "cameraPromptShown"
        const val KEY_LOCATION_PROMPT_SHOWN = "locationPromptShown"
        const val KEY_START_SERVICE_AFTER_PROMPT = "startServiceAfterPrompt"
    }
}
