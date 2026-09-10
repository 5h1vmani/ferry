package app.ferry

import android.app.Application
import app.ferry.engine.FerryEngine

// The process starts here, for the activity and for the reachable service
// alike. The engine is built now, whether or not all files access has been
// granted, so both of them find the same engine ready.
//
// Building the engine starts no thread. FerryEngine.start does that, and it
// runs only once the grant exists.
class FerryApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Permissions.refresh(this)
        FerryEngine.create(this)
    }
}
