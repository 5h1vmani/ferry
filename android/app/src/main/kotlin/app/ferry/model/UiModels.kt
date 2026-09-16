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

// Whether a peer is a phone or a Mac. The engine sends this in hello, so
// the row draws the device's own icon instead of assuming every peer of a
// phone is a Mac.
enum class DeviceKind {
    Phone,
    Mac,
}

// A device this phone has paired with. Matches docs/ia.md's Device.
data class DeviceInfo(
    // The public key as 64 lowercase hex characters. This is the identity,
    // and it is what forget takes.
    val id: String,
    val name: String,
    val kind: DeviceKind,
    val transport: Transport,
    val connectionState: ConnectionState,
    // A transport that is available but is not the one carrying bytes.
    // Stated once beside the badge so a pulled cable is not a surprise.
    // Null when there is no second path.
    val spareTransport: Transport?,
    // Set only when connectionState is NotReachable, and only when the
    // engine knows when the device was last seen.
    val lastSeenUnixSecs: Long?,
    val pairedUnixSecs: Long,
    // The first sixteen bytes of the key, in groups of four characters.
    val keyFingerprint: String,
    // Where this device's roots are mounted on this phone, if a bridge is
    // serving them. Null until DocumentsProvider ships; see README.
    val mountPath: String?,
)

// One file moving in one direction. Matches docs/ia.md's Transfer.
enum class TransferState {
    Queued,
    Active,
    Paused,
    Done,
    Failed,
}

// Which way bytes move.
enum class Direction {
    // This phone fetched the file from the Mac.
    Pull,
    // This phone sent the file to the Mac.
    Push,
}

// Why a group of transfers exists: a person asked, or Ferry decided. Job 7.
enum class Origin {
    Manual,
    Automatic,
}

// The three-part rule from docs/voice.md: what stopped, why, what to do.
// Any part the engine does not know is left null, never guessed.
data class ThreePartError(
    val stopped: String,
    val why: String?,
    val todo: String?,
)

// How far into a file's chunks a transfer has verified. The bottom of the
// depth axis in docs/ia.md, shown by the chunk disclosure.
data class ChunkFacts(
    val verified: Int,
    val total: Int,
    // The chunk the engine named in a verify failure, if it named one.
    val failedIndex: Int?,
)

// One row of the Transfers section: either one batch, or one transfer that
// belongs to no batch. Both draw the same way, so no view asks which it is.
//
// This is what makes the Transfers section one list rather than two. A
// folder copy is 120 transfers and one row; a single pull is one transfer
// and one row.
data class TransferGroup(
    // The batch id, or the transfer id for a group of one.
    val id: String,
    val deviceId: String,
    // "Internal storage/DCIM/Camera" for a batch, or a file's name.
    val label: String,
    val direction: Direction,
    val origin: Origin,
    val state: TransferState,
    val filesDone: Int,
    val filesTotal: Int,
    val bytesDone: Long,
    val bytesTotal: Long,
    val speedMBps: Int?,
    val transport: Transport?,
    // The engine's error code, such as TransferError::Local. The view turns
    // it into words with threePartError, so no English is stored here.
    val errorCode: String?,
    val errorDetail: String?,
    // Present only for a group of one, and only once the engine knows the
    // file's size. A batch has no single chunk count.
    val chunks: ChunkFacts?,
    val startedUnixSecs: Long,
    val endedUnixSecs: Long?,
) {
    val fraction: Float
        get() = if (bytesTotal > 0L) bytesDone.toFloat() / bytesTotal.toFloat() else 0f

    val percent: Int
        get() = (fraction * 100f).toInt()

    // True when this group stands for exactly one file, which is every
    // group a single pull or push makes.
    val isSingleFile: Boolean
        get() = filesTotal <= 1

    // How long a finished group took, or null while it is still running.
    val durationSecs: Long?
        get() = endedUnixSecs?.let { it - startedUnixSecs }
}

// What a file operation did, in the file operations layer's own words, so a
// log line and a protocol trace say the same word. L5, job 9.
enum class AccessVerb {
    List,
    Stat,
    Read,
    Write,
    Truncate,
    Rename,
    Mkdir,
    Delete,
}

// Who performed the operation. Ferry is symmetric: either device serves
// files to the other, so both answers are ordinary.
enum class AccessActor {
    // The paired device, on this phone's files.
    Peer,
    // This phone, on the paired device's files.
    ThisDevice,
}

// One file operation, as one row of the access log.
data class AccessEntry(
    val id: String,
    val deviceId: String,
    val actor: AccessActor,
    val verb: AccessVerb,
    // Root-relative, beginning with the root name: "Desktop/Q3 notes.md".
    val path: String,
    // Bytes moved, for a read or a write.
    val bytes: Long?,
    // For a list, how many entries were returned.
    val entries: Int?,
    // For a folder copy, how many files it covered.
    val files: Int?,
    val atUnixSecs: Long,
)

// One day of the access log, newest first. Which day a moment falls in is
// decided once, in Mapping.kt, so two screens cannot disagree about where
// midnight is.
data class AccessDay(
    // Which day this is, for the view to name. The view reads the words.
    val kind: DayKind,
    // Set only when kind is Earlier, for the date the view formats.
    val atUnixSecs: Long,
    val entries: List<AccessEntry>,
) {
    val id: String
        get() = if (kind == DayKind.Earlier) "earlier-$atUnixSecs" else kind.name
}

enum class DayKind {
    Today,
    Yesterday,
    Earlier,
}

// Which way in a person chose. Pairing is a once-ever job, so two ways is
// one screen more, not two things to maintain.
enum class PairingMethod {
    Scan,
    Code,
}

// Where the pairing flow is, for the one screen that draws it.
//
// The engine's PairingState and the method a person chose are folded into
// this one value, so PairingScreen switches once instead of twice. Found
// and Offering belong to the Mac and never reach this phone. Requested
// does reach this phone: it is what the engine reports once a scan's
// handshake finds the Mac, and it is this state that carries the name
// Scanned shows below.
sealed class PairingStep {
    // No method chosen yet. Both ways in are offered.
    data object Choosing : PairingStep()

    // The camera is open, looking for the Mac's code.
    data object Scanning : PairingStep()

    // The camera cannot be used. Pairing by code is the way onward.
    data object CameraRefused : PairingStep()

    // The code was read and sent. Null while the handshake with the Mac is
    // still under way; once the engine reports Requested, the name is
    // here and this phone asks its own question before it stores the Mac.
    data class Scanned(val deviceName: String?) : PairingStep()

    // Code method: waiting for the Mac to find this phone.
    data class Waiting(
        val shortCode: String?,
        val expiresUnixSecs: Long,
    ) : PairingStep()

    // Code method: the six digits, grouped three and three.
    data class Code(
        val digits: String,
        val expiresUnixSecs: Long,
    ) : PairingStep()

    // Both methods end here.
    data class Confirmed(val deviceId: String) : PairingStep()

    data class Failed(
        val code: String,
        val detail: String?,
        // Which method failed, so the screen can offer the other one.
        val method: PairingMethod?,
    ) : PairingStep()
}
