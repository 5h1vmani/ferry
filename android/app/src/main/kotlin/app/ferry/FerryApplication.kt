package app.ferry

import android.app.Activity
import android.app.Application
import android.os.Bundle
import app.ferry.engine.FerryEngine

// The process starts here, for the activity and for the reachable service
// alike. The engine is built now, whether or not all files access has been
// granted, so both of them find the same engine ready.
//
// Building the engine starts no thread. FerryEngine.start does that, and it
// runs only once the grant exists.
class FerryApplication : Application() {
    // How many activities exist right now, created but not yet destroyed.
    // ReachableService reads this through hasActivity to decide whether it
    // is the last thing keeping the engine alive.
    private var activityCount = 0

    override fun onCreate() {
        super.onCreate()
        Permissions.refresh(this)
        FerryEngine.create(this)
        // Registered once here rather than after create(): a callback that
        // fires before the engine exists is not lost, because NetworkName
        // holds the last name it read and FerryEngine.start() reads it
        // back.
        NetworkName.start(this)
        registerActivityLifecycleCallbacks(
            object : ActivityLifecycleCallbacks {
                override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {
                    activityCount++
                }

                override fun onActivityDestroyed(activity: Activity) {
                    activityCount--
                }

                override fun onActivityStarted(activity: Activity) = Unit
                override fun onActivityResumed(activity: Activity) = Unit
                override fun onActivityPaused(activity: Activity) = Unit
                override fun onActivityStopped(activity: Activity) = Unit
                override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) = Unit
            },
        )
    }

    // True while an activity exists that has not yet been destroyed.
    fun hasActivity(): Boolean = activityCount > 0
}
