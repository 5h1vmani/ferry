package app.ferry.engine

import app.ferry.FerryErrorCode
import app.ferry.ShareCacheRegistry
import app.ferry.ShareIntake
import kotlinx.coroutines.launch
import uniffi.ferry_runtime.DeviceInfo
import uniffi.ferry_runtime.FerryException
import java.util.UUID

// Sending files the OS gestures gather: docs/ux-fix-plan.md, item 1. Split
// from FerryEngine.kt, docs/audits/principles.md row P9. pushFiles and
// pushShared are extension functions on FerryEngine, so a caller still
// writes FerryEngine.pushShared(...), unchanged from before the split.
//
// The engine is gaining its own landing_folder call in a parallel batch;
// landingFolder, LANDING_ROOT_NAME, and LANDING_SUBFOLDER stay as they are
// here until the phone adopts it.

// The root a gesture-started push lands in when the peer has one named
// this, ignoring case; otherwise the first root the peer lists.
// docs/engine-contract.md item 5, "Where a push lands".
private const val LANDING_ROOT_NAME = "Downloads"
private const val LANDING_SUBFOLDER = "Ferry"

// Sends several files into one folder on a paired device, as one batch.
// Returns the batch id. In the same style as list, stat, and mkdir in
// FerryEngineDocuments.kt: a passthrough that throws the engine's own
// FerryException.
fun FerryEngine.pushFiles(keyHex: String, localPaths: List<String>, remoteFolder: String): String =
    required().pushFiles(keyHex, localPaths, remoteFolder)

// Sends every path to the target device's landing folder. Called by a
// share from another app and by the Devices screen's "Send files"
// control; both hand this the same list of absolute paths.
//
// Does nothing when no Mac is paired: Devices already shows that state
// on its own once ShareIntake asks it to. Otherwise this dials the
// target to list its roots, makes the landing folder, then pushes, and
// every one of those calls blocks, so it runs on scope, off the
// caller's thread.
fun FerryEngine.pushShared(localPaths: List<String>) {
    val devices = _devices.value
    if (devices.isEmpty()) {
        return
    }
    val device = targetDevice(devices) ?: run {
        // Several are paired and none is reachable: audit finding 6.
        // The same fault a dial would hit anyway, shown at once rather
        // than after picking one of several devices arbitrarily.
        _error.value = FerryException.Failed(FerryErrorCode.RUNTIME_NOT_REACHABLE, null)
        return
    }
    val keyHex = device.keyHex
    _error.value = null
    // Registered under a request id before the push, not only after
    // with the real batch id: audit finding 7. A throw below, or one
    // this coroutine never reaches because pushFiles itself never
    // returns, must not leave an unregistered copy on disk forever.
    val requestId = UUID.randomUUID().toString()
    val context = appContext
    if (context != null) {
        ShareCacheRegistry.registerBatch(context, requestId, localPaths)
    }
    scope.launch {
        try {
            val folder = landingFolder(keyHex) ?: run {
                // The Mac lists no root at all: audit finding 8. Not
                // an engine code, so this is an app-side fault shown
                // the same way the Mac's own DropError.noLandingFolder
                // is, naming the device.
                ShareIntake.setAppError(ShareIntake.AppError.NoLandingFolder(device.name))
                return@launch
            }
            try {
                mkdir(keyHex, folder)
            } catch (e: FerryException) {
                val code = (e as? FerryException.Failed)?.code
                // push does not create the parent folder itself
                // (docs/engine-contract.md item 5), so this call makes
                // it first. A folder already there is success, not a
                // fault.
                if (code != FerryErrorCode.OP_ERROR_ALREADY_EXISTS) {
                    throw e
                }
            }
            val batchId = pushFiles(keyHex, localPaths, folder)
            context?.let { ShareCacheRegistry.renameRegistration(it, requestId, batchId) }
        } catch (e: FerryException) {
            _error.value = e
        }
    }
}

// The only paired device, else the first reachable one: audit finding
// 6, the same rule the Mac's EngineModel.targetDevice uses. Null when
// several are paired and none is reachable.
private fun targetDevice(devices: List<DeviceInfo>): DeviceInfo? {
    if (devices.size == 1) {
        return devices.first()
    }
    return devices.firstOrNull { it.reachableVia != null }
}

// The root named "Downloads", matched ignoring case, else the first
// root the peer lists, each in a folder named "Ferry". Null when the
// peer lists no root at all: a Mac with nothing shared in Settings.
// docs/engine-contract.md item 5, "Where a push lands". Audit finding
// 8: this used to throw the pairing code Runtime::NoCandidate here,
// whose words say nothing true about a share.
private fun FerryEngine.landingFolder(keyHex: String): String? {
    val roots = list(keyHex, "")
    val chosen = roots.firstOrNull { it.name.equals(LANDING_ROOT_NAME, ignoreCase = true) }
        ?: roots.firstOrNull()
        ?: return null
    return "${chosen.name}/$LANDING_SUBFOLDER"
}
