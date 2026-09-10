package app.ferry

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent

// The one activity. Everything else is Compose, switched by FerryApp's own
// screen state, not by the Android framework's back stack.
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            FerryApp()
        }
    }
}
