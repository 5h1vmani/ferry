package app.ferry

import android.Manifest
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import app.ferry.engine.FerryEngine

// The one activity. Everything else is Compose, switched by FerryApp's own
// screen state, not by the Android framework's back stack.
//
// The activity owns everything that needs an Activity: the system screens
// for all files access, notifications and this app's own settings, the
// three runtime prompts, and the engine's start and stop.
class MainActivity : ComponentActivity() {
    private lateinit var notificationPrompt: ActivityResultLauncher<String>
    private lateinit var cameraPrompt: ActivityResultLauncher<String>
    private lateinit var locationPrompt: ActivityResultLauncher<String>

    // True once the person has granted on the first run screen. The
    // notification prompt follows the all files access screen, which is
    // step 3 of the phone first run in docs-v2/ia.md.
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
            ActivityResultContracts.RequestPermission(),
        ) { granted ->
            Permissions.locationAnswered(granted)
            // A callback registered before this grant never carries the
            // name, so a grant here is exactly the edge NetworkName has to
            // re-register for.
            NetworkName.refresh(this)
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
            )
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
        locationPrompt.launch(Manifest.permission.ACCESS_FINE_LOCATION)
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
