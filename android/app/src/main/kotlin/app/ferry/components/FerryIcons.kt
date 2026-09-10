package app.ferry.components

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AddCircle
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Error
import androidx.compose.material.icons.outlined.LaptopMac
import androidx.compose.material.icons.outlined.PauseCircle
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material.icons.outlined.Smartphone
import androidx.compose.material.icons.outlined.SyncAlt
import androidx.compose.material.icons.outlined.Usb
import androidx.compose.material.icons.outlined.VerifiedUser
import androidx.compose.material.icons.outlined.Wifi
import androidx.compose.material.icons.outlined.WifiOff
import androidx.compose.ui.graphics.vector.ImageVector
import app.ferry.FerryIcon

// The one table that maps each FerryIcon semantic name to the Material icon
// that draws it, so the mapping is not invented again in every component.
// Every name below has an exact match in the Material icon set. None
// needed a "closest fit" substitute.
fun ferryIconFor(name: String): ImageVector = when (name) {
    FerryIcon.devicePhone -> Icons.Outlined.Smartphone
    FerryIcon.deviceMac -> Icons.Outlined.LaptopMac
    FerryIcon.usb -> Icons.Outlined.Usb
    FerryIcon.wifi -> Icons.Outlined.Wifi
    FerryIcon.notReachable -> Icons.Outlined.WifiOff
    FerryIcon.paired -> Icons.Outlined.VerifiedUser
    FerryIcon.transfer -> Icons.Outlined.SyncAlt
    FerryIcon.paused -> Icons.Outlined.PauseCircle
    FerryIcon.failed -> Icons.Outlined.Error
    FerryIcon.done -> Icons.Outlined.CheckCircle
    FerryIcon.forget -> Icons.Outlined.Delete
    FerryIcon.settings -> Icons.Outlined.Settings
    FerryIcon.pair -> Icons.Outlined.AddCircle
    else -> Icons.Outlined.Error
}
