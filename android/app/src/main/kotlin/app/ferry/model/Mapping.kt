package app.ferry.model

import uniffi.ferry_runtime.DeviceInfo as EngineDeviceInfo
import uniffi.ferry_runtime.FerryException
import uniffi.ferry_runtime.TransferInfo as EngineTransferInfo
import uniffi.ferry_runtime.TransferState as EngineTransferState
import uniffi.ferry_runtime.Transport as EngineTransport

// Turns an engine record into what a screen draws. Pure functions, no
// strings, no Android. The words come later, from strings.xml, inside the
// view that draws them.

// One megabyte is a million bytes here, because docs/voice.md shows speeds
// as "38 MB/s" and a person reads that as the decimal unit.
private const val BYTES_PER_MEGABYTE = 1_000_000L

// Sixteen bytes of the key, as thirty-two hex characters, is the
// fingerprint a person compares. The whole key is sixty-four characters and
// is too long to read off a screen.
private const val FINGERPRINT_HEX_CHARS = 32

private const val FINGERPRINT_GROUP = 4

fun EngineTransport.toUi(): Transport = when (this) {
    EngineTransport.USB -> Transport.Usb
    EngineTransport.WIFI -> Transport.Wifi
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
        // TransportBadge draws the not reachable icon whatever the
        // transport is, so a device with no transport right now is given
        // Wi-Fi to satisfy the type. Nothing on screen depends on it.
        transport = via?.toUi() ?: Transport.Wifi,
        connectionState = state,
        lastSeenUnixSecs = lastSeenUnixSecs,
        pairedUnixSecs = pairedUnixSecs,
        keyFingerprint = fingerprintOf(keyHex),
    )
}

fun EngineTransferInfo.toUi(): TransferInfo {
    val failure = error as? FerryException.Failed
    return TransferInfo(
        id = id,
        deviceId = deviceKeyHex,
        fileName = fileName,
        // A transfer that is not moving names no transport. Wi-Fi stands in
        // for the type; the badge is drawn only while the transfer is
        // active, and then the transport is known.
        transport = transport?.toUi() ?: Transport.Wifi,
        state = state.toUi(),
        bytesDone = bytesDone.toLong(),
        bytesTotal = bytesTotal.toLong(),
        errorCode = failure?.code,
        errorDetail = failure?.detail,
    )
}

fun EngineTransferState.toUi(): TransferState = when (this) {
    EngineTransferState.QUEUED -> TransferState.Queued
    EngineTransferState.ACTIVE -> TransferState.Active
    EngineTransferState.PAUSED -> TransferState.Paused
    EngineTransferState.DONE -> TransferState.Done
    EngineTransferState.FAILED -> TransferState.Failed
}

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
