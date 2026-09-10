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
// for all files access and notifications, the runtime notification prompt,
// and the engine's start and stop.
class MainActivity : ComponentActivity() {
    private lateinit var notificationPrompt: ActivityResultLauncher<String>

    // True once the person has used Continue on the first run screen. The
    // notification prompt follows the all files access screen, which is
    // step 3 of the phone first run in docs/ia.md.
    private var cameFromFirstRun = false

    // The prompt is shown once per run of the process. Android itself
    // refuses to show it again after two refusals, so asking again on every
    // resume would do nothing.
    private var promptShown = false

    // Set when the reachable switch went on and the prompt had to come
    // first. The service starts as soon as the prompt is answered.
    private var startServiceAfterPrompt = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        notificationPrompt = registerForActivityResult(
            ActivityResultContracts.RequestPermission(),
        ) {
            Permissions.refresh(this)
            if (startServiceAfterPrompt) {
                startServiceAfterPrompt = false
                ReachableService.turnOn(this)
            }
        }
        setContent {
            FerryApp(
                onContinueFirstRun = ::continueFirstRun,
                onOpenAllFilesAccess = { startActivity(Permissions.allFilesAccessIntent(this)) },
                onOpenNotificationSettings = {
                    startActivity(Permissions.notificationSettingsIntent(this))
                },
                onSetReachable = ::setReachable,
            )
        }
    }

    override fun onResume() {
        super.onResume()
        Permissions.refresh(this)
        // The process can outlive an activity that finished and stopped the
        // engine. This builds a fresh one in that case, and does nothing in
        // the ordinary one.
        FerryEngine.create(this)
        if (cameFromFirstRun && Permissions.asksForNotifications() && !promptShown &&
            !Permissions.notifications.value
        ) {
            cameFromFirstRun = false
            promptShown = true
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

    override fun onDestroy() {
        super.onDestroy()
        // The engine lives as long as the process, and dropping it does not
        // end it. It is stopped only when nothing is left to serve: no
        // reachable service, and this activity going away for good.
        if (isFinishing && !ReachableService.running) {
            FerryEngine.stop()
        }
    }

    private fun continueFirstRun() {
        cameFromFirstRun = true
        Permissions.markFirstRunDone()
        startActivity(Permissions.allFilesAccessIntent(this))
    }

    private fun setReachable(on: Boolean) {
        if (!on) {
            ReachableService.turnOff(this)
            return
        }
        // Android 13 and later need POST_NOTIFICATIONS before the service
        // can show its ongoing notification, so the prompt comes first.
        if (Permissions.asksForNotifications() && !Permissions.notifications.value && !promptShown) {
            promptShown = true
            startServiceAfterPrompt = true
            notificationPrompt.launch(Manifest.permission.POST_NOTIFICATIONS)
            return
        }
        ReachableService.turnOn(this)
    }
}
