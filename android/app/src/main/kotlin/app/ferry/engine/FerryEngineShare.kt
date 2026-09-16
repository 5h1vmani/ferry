package app.ferry.engine

import app.ferry.FerryErrorCode
import app.ferry.ShareCacheRegistry
import kotlinx.coroutines.launch
import uniffi.ferry_runtime.DeviceInfo
import uniffi.ferry_runtime.FerryException
import java.util.UUID

// Sending files the OS gestures gather: docs/ux-fix-plan.md, item 1. Split
// from FerryEngine.kt, docs/audits/principles.md row P9. pushFiles and
// pushShared are extension functions on FerryEngine, so a caller still
// writes FerryEngine.pushShared(...), unchanged from before the split.

// Where a gesture-started push lands on the target device. The engine
// picks the folder and makes it, so this app no longer picks a folder
// name of its own. docs/engine-contract.md item 5, "Where a push lands".
// In the same style as list, stat, and mkdir in FerryEngineDocuments.kt:
// a passthrough that throws the engine's own FerryException.
fun FerryEngine.landingFolder(keyHex: String): String = required().landingFolder(keyHex)

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
// on its own once ShareIntake asks it to. Otherwise this asks the engine
// for the landing folder, then pushes, and both calls block, so this
// runs on scope, off the caller's thread.
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
            // The engine picks the folder, makes it, and returns
            // Runtime::NotPaired, Runtime::NotReachable,
            // OpError::PermissionDenied, or RootsError::NoRoots on its
            // own errors. Every one already has words in the generated
            // table, so no app-side fault is built for any of them here.
            val folder = landingFolder(keyHex)
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
