package app.ferry.model

import uniffi.ferry_runtime.AccessEntry as EngineAccessEntry
import uniffi.ferry_runtime.AccessVerb as EngineAccessVerb
import uniffi.ferry_runtime.Actor as EngineActor
import uniffi.ferry_runtime.BatchInfo as EngineBatchInfo
import uniffi.ferry_runtime.DeviceInfo as EngineDeviceInfo
import uniffi.ferry_runtime.DeviceKind as EngineDeviceKind
import uniffi.ferry_runtime.Direction as EngineDirection
import uniffi.ferry_runtime.FerryException
import uniffi.ferry_runtime.Origin as EngineOrigin
import uniffi.ferry_runtime.PairingState
import uniffi.ferry_runtime.TransferInfo as EngineTransferInfo
import uniffi.ferry_runtime.TransferState as EngineTransferState
import uniffi.ferry_runtime.Transport as EngineTransport
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId

// Turns an engine record into what a screen draws. Pure functions, no
// strings, no Android. The words come later, from strings.xml, inside the
// view that draws them.
//
// This is the only file that names an engine type. A screen that needed a
// new engine field would change this file and its own body, and nothing in
// between.

// One megabyte is a million bytes here, because docs/voice.md shows speeds
// as "38 MB/s" and a person reads that as the decimal unit.
private const val BYTES_PER_MEGABYTE = 1_000_000L

// Sixteen bytes of the key, as thirty-two hex characters, is the
// fingerprint a person compares. The whole key is sixty-four characters and
// is too long to read off a screen.
private const val FINGERPRINT_HEX_CHARS = 32

private const val FINGERPRINT_GROUP = 4

private const val PAIRING_CODE_DIGITS = 6

private const val PAIRING_CODE_GROUP = 3

fun EngineTransport.toUi(): Transport = when (this) {
    EngineTransport.USB -> Transport.Usb
    EngineTransport.WIFI -> Transport.Wifi
}

fun EngineDeviceKind.toUi(): DeviceKind = when (this) {
    EngineDeviceKind.PHONE -> DeviceKind.Phone
    EngineDeviceKind.MAC -> DeviceKind.Mac
}

fun EngineDirection.toUi(): Direction = when (this) {
    EngineDirection.PULL -> Direction.Pull
    EngineDirection.PUSH -> Direction.Push
}

fun EngineOrigin.toUi(): Origin = when (this) {
    EngineOrigin.MANUAL -> Origin.Manual
    EngineOrigin.AUTOMATIC -> Origin.Automatic
}

fun EngineTransferState.toUi(): TransferState = when (this) {
    EngineTransferState.QUEUED -> TransferState.Queued
    EngineTransferState.ACTIVE -> TransferState.Active
    EngineTransferState.PAUSED -> TransferState.Paused
    EngineTransferState.DONE -> TransferState.Done
    EngineTransferState.FAILED -> TransferState.Failed
}

fun EngineDeviceInfo.toUi(): DeviceInfo {
    val via = reachableVia
    val speed = speedBytesPerSec
    val state = when {
        via == null -> ConnectionState.NotReachable
        speed != null && speed > 0uL -> ConnectionState.Moving(megabytesPerSecond(speed.toLong()))
        else -> ConnectionState.Idle
    }
    return DeviceInfo(
        id = keyHex,
        name = name,
        kind = kind.toUi(),
        // TransportBadge draws the not reachable icon whatever the
        // transport is, so a device with no transport right now is given
        // Wi-Fi to satisfy the type. Nothing on screen depends on it.
        transport = via?.toUi() ?: Transport.Wifi,
        connectionState = state,
        spareTransport = spareTransportOf(availableTransports, via),
        lastSeenUnixSecs = lastSeenUnixSecs,
        pairedUnixSecs = pairedUnixSecs,
        keyFingerprint = fingerprintOf(keyHex),
    )
}

// The transport that is available but is not carrying bytes. Only ever one:
// there are two transports in total, so a device with both has exactly one
// spare. A device reachable through neither has none.
private fun spareTransportOf(
    available: List<EngineTransport>,
    active: EngineTransport?,
): Transport? {
    if (active == null) {
        return null
    }
    return available.firstOrNull { it != active }?.toUi()
}

// Every Transfers row for one device: its batches, and the transfers that
// belong to no batch, newest first.
//
// A batch and a lone transfer draw the same way, so they are folded into
// one list here rather than in the screen. This is the whole reason the
// Transfers section is one list and not two.
fun transferGroupsFor(
    deviceId: String,
    batches: List<EngineBatchInfo>,
    transfers: List<EngineTransferInfo>,
): List<TransferGroup> {
    val mine = transfers.filter { it.deviceKeyHex == deviceId }
    val groups = batches
        .filter { it.deviceKeyHex == deviceId }
        .map { it.toUi() } +
        mine.filter { it.batchId == null }.map { it.toUi() }
    return groups.sortedByDescending { it.startedUnixSecs }
}

fun EngineBatchInfo.toUi(): TransferGroup = TransferGroup(
    id = id,
    deviceId = deviceKeyHex,
    label = label,
    direction = direction.toUi(),
    origin = origin.toUi(),
    state = state.toUi(),
    filesDone = filesDone.toInt(),
    filesTotal = filesTotal.toInt(),
    bytesDone = bytesDone.toLong(),
    bytesTotal = bytesTotal.toLong(),
    speedMBps = speedBytesPerSec?.let { megabytesPerSecond(it.toLong()) },
    transport = transport?.toUi(),
    errorCode = (error as? FerryException.Failed)?.code,
    errorDetail = (error as? FerryException.Failed)?.detail,
    // A batch is many files, so it has no single chunk count. Depth stops
    // at the file for a batch, and the chunks of the one file that failed
    // are reached through that transfer.
    chunks = null,
    startedUnixSecs = startedUnixSecs,
    endedUnixSecs = endedUnixSecs,
)

fun EngineTransferInfo.toUi(): TransferGroup {
    val failure = error as? FerryException.Failed
    return TransferGroup(
        id = id,
        deviceId = deviceKeyHex,
        label = fileName,
        direction = direction.toUi(),
        // A lone transfer is always something a person asked for: only
        // auto_copy makes an Automatic group, and it always makes a batch.
        origin = Origin.Manual,
        state = state.toUi(),
        filesDone = if (state == EngineTransferState.DONE) 1 else 0,
        filesTotal = 1,
        bytesDone = bytesDone.toLong(),
        bytesTotal = bytesTotal.toLong(),
        speedMBps = speedBytesPerSec?.let { megabytesPerSecond(it.toLong()) },
        transport = transport?.toUi(),
        errorCode = failure?.code,
        errorDetail = failure?.detail,
        chunks = chunkFactsOf(
            total = chunksTotal.toInt(),
            verified = chunksVerified.toInt(),
            detail = failure?.detail,
            failed = state == EngineTransferState.FAILED,
        ),
        startedUnixSecs = startedUnixSecs,
        endedUnixSecs = endedUnixSecs,
    )
}

// The chunk facts for one transfer, or null when the engine knows none.
//
// A verify failure names its chunk in the error's detail, which the engine
// fills with a value and never a sentence, so a detail that parses as a
// number is that index.
private fun chunkFactsOf(
    total: Int,
    verified: Int,
    detail: String?,
    failed: Boolean,
): ChunkFacts? {
    if (total <= 0) {
        return null
    }
    val index = if (failed) detail?.trim()?.toIntOrNull() else null
    return ChunkFacts(verified = verified, total = total, failedIndex = index)
}

fun EngineAccessEntry.toUi(): AccessEntry = AccessEntry(
    id = id,
    deviceId = deviceKeyHex,
    actor = when (actor) {
        EngineActor.PEER -> AccessActor.Peer
        EngineActor.THIS -> AccessActor.ThisDevice
    },
    verb = verb.toUi(),
    path = path,
    bytes = bytes?.toLong(),
    entries = entries?.toInt(),
    files = files?.toInt(),
    atUnixSecs = atUnixSecs,
)

fun EngineAccessVerb.toUi(): AccessVerb = when (this) {
    EngineAccessVerb.LIST -> AccessVerb.List
    EngineAccessVerb.STAT -> AccessVerb.Stat
    EngineAccessVerb.READ -> AccessVerb.Read
    EngineAccessVerb.WRITE -> AccessVerb.Write
    EngineAccessVerb.TRUNCATE -> AccessVerb.Truncate
    EngineAccessVerb.RENAME -> AccessVerb.Rename
    EngineAccessVerb.MKDIR -> AccessVerb.Mkdir
    EngineAccessVerb.DELETE -> AccessVerb.Delete
}

// Groups access log entries into days, newest first, keeping the engine's
// order inside each day.
//
// Which day a moment falls in is decided here and nowhere else, so the Mac
// and the phone and any preview agree about where midnight is.
fun accessDaysOf(
    entries: List<AccessEntry>,
    zone: ZoneId = ZoneId.systemDefault(),
    today: LocalDate = LocalDate.now(zone),
): List<AccessDay> {
    if (entries.isEmpty()) {
        return emptyList()
    }
    val yesterday = today.minusDays(1)
    return entries
        .groupBy { Instant.ofEpochSecond(it.atUnixSecs).atZone(zone).toLocalDate() }
        .toList()
        .sortedByDescending { (date, _) -> date }
        .map { (date, dayEntries) ->
            AccessDay(
                kind = when (date) {
                    today -> DayKind.Today
                    yesterday -> DayKind.Yesterday
                    else -> DayKind.Earlier
                },
                atUnixSecs = dayEntries.first().atUnixSecs,
                entries = dayEntries,
            )
        }
}

// Where the pairing flow is, from the engine's state and the method a
// person chose.
//
// Two engine states belong to the Mac only: Found, because the Mac browses
// and picks; and Offering, because the Mac shows the code. The phone
// reaching either would mean the engine and this app disagree about which
// device this is, so each maps to the step that is still true — this phone
// is waiting. Requested is different: the phone reaches it too, once its
// own scan's handshake finds the Mac, and it carries the Mac's name for
// the question this phone then asks before it stores the Mac.
fun pairingStepOf(
    state: PairingState,
    method: PairingMethod?,
    shortCode: String?,
    cameraRefused: Boolean,
    scanSent: Boolean,
): PairingStep {
    if (method == PairingMethod.Scan && cameraRefused) {
        return PairingStep.CameraRefused
    }
    return when (state) {
        is PairingState.Idle -> when {
            method == null -> PairingStep.Choosing
            method == PairingMethod.Scan && scanSent -> PairingStep.Scanned(null)
            method == PairingMethod.Scan -> PairingStep.Scanning
            else -> PairingStep.Waiting(shortCode, expiresUnixSecs = 0L)
        }

        is PairingState.Waiting -> if (method == PairingMethod.Scan) {
            if (scanSent) PairingStep.Scanned(null) else PairingStep.Scanning
        } else {
            PairingStep.Waiting(shortCode, state.expiresUnixSecs)
        }

        is PairingState.Code -> PairingStep.Code(
            digits = groupsOfThree(state.code),
            expiresUnixSecs = state.expiresUnixSecs,
        )

        is PairingState.Confirmed -> PairingStep.Confirmed(state.device.keyHex)

        is PairingState.Failed -> {
            val failure = state.error as? FerryException.Failed
            PairingStep.Failed(
                code = failure?.code.orEmpty(),
                detail = failure?.detail,
                method = method,
            )
        }

        // The Mac's own two. See the note above.
        is PairingState.Found -> PairingStep.Waiting(shortCode, state.expiresUnixSecs)
        is PairingState.Offering -> PairingStep.Waiting(shortCode, state.offer.expiresUnixSecs)
        // Reached by the phone too, once a scan's handshake finds the Mac.
        is PairingState.Requested -> PairingStep.Scanned(state.name)
    }
}

// Whether the phone's current Wi-Fi network is already in the trusted
// list. A null name is never trusted: an unknown network cannot be told
// apart from the list, so Settings offers no "Trust this network" control
// for it. docs/engine-contract.md, item 18.
fun isCurrentNetworkTrusted(currentNetworkName: String?, trustedNetworks: List<String>): Boolean =
    currentNetworkName != null && trustedNetworks.contains(currentNetworkName)

// Bytes per second as whole megabytes per second. A speed under half a
// megabyte reads as zero, which is what the number is.
fun megabytesPerSecond(bytesPerSec: Long): Int =
    Math.round(bytesPerSec.toDouble() / BYTES_PER_MEGABYTE.toDouble()).toInt()

// The first sixteen bytes of a key, uppercase, in groups of four.
fun fingerprintOf(keyHex: String): String =
    keyHex.take(FINGERPRINT_HEX_CHARS)
        .uppercase()
        .chunked(FINGERPRINT_GROUP)
        .joinToString(" ")

// The engine reports six digits with nothing between them. PairingCode
// draws two groups of three, so the string is split here rather than in the
// screen: it is a fact about the code, not about the view.
fun groupsOfThree(code: String): String =
    if (code.length == PAIRING_CODE_DIGITS) {
        code.substring(0, PAIRING_CODE_GROUP) + " " + code.substring(PAIRING_CODE_GROUP)
    } else {
        code
    }
