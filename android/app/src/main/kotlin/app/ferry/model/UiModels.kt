package app.ferry.model

// What the screens draw. Each type is built from an engine record by the
// functions in Mapping.kt, and holds only what a view needs.
//
// These are not the engine's own types. The engine reports raw numbers: a
// unix time, a byte count, a key in hex. A view needs a state to switch on
// and a number to format, so the shape differs on purpose. Text that a
// person reads is never stored here; it is read from strings.xml at the
// moment the view draws.

// One way Ferry reaches a device. Matches docs/ia.md's Transport object.
enum class Transport {
    Usb,
    Wifi,
}

// How a device is reachable right now, for TransportBadge and DeviceRow.
// Matches the state table in docs/components.md.
sealed class ConnectionState {
    data object Idle : ConnectionState()
    data class Moving(val speedMBps: Int) : ConnectionState()
    data object Connecting : ConnectionState()
    data object NotReachable : ConnectionState()
}

// A Mac this phone has paired with. Matches docs/ia.md's Device object.
data class DeviceInfo(
    // The public key as 64 lowercase hex characters. This is the identity,
    // and it is what forget takes.
    val id: String,
    val name: String,
    val transport: Transport,
    val connectionState: ConnectionState,
    // Set only when connectionState is NotReachable, and only when the
    // engine knows when the device was last seen.
    val lastSeenUnixSecs: Long?,
    val pairedUnixSecs: Long,
    // The first sixteen bytes of the key, in groups of four characters.
    val keyFingerprint: String,
)

// One file moving in one direction. Matches docs/ia.md's Transfer object.
enum class TransferState {
    Queued,
    Active,
    Paused,
    Done,
    Failed,
}

// The three-part rule from docs/voice.md: what stopped, why, what to do.
// Any part the engine does not know is left null, never guessed.
data class ThreePartError(
    val stopped: String,
    val why: String?,
    val todo: String?,
)

data class TransferInfo(
    val id: String,
    val deviceId: String,
    val fileName: String,
    val transport: Transport,
    val state: TransferState,
    val bytesDone: Long,
    val bytesTotal: Long,
    // The engine's error code, such as TransferError::Local. The view turns
    // it into words with threePartError, so no English is stored here.
    val errorCode: String?,
    val errorDetail: String?,
)
