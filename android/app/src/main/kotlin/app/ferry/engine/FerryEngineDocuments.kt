package app.ferry.engine

import android.provider.DocumentsContract
import app.ferry.FerryErrorCode
import uniffi.ferry_runtime.Engine
import uniffi.ferry_runtime.Entry
import uniffi.ferry_runtime.FerryException

// The Mac's folders in the phone's Files app. docs/engine-contract.md item
// 19, job 8. FerryDocumentsProvider is the only caller. It runs in this
// process on binder threads, which may block, and every call below blocks
// for at least one round trip.
//
// Each one is a passthrough. The engine's error is thrown on unchanged,
// because only the provider knows whether a refusal becomes an errno or a
// FileNotFoundException. Nothing here decides what a refusal means.
//
// Split from FerryEngine.kt, docs/audits/principles.md row P9. Every
// function here is an extension on FerryEngine, so a caller still writes
// FerryEngine.list(...), unchanged from before the split.

// What a call made before the engine exists reports. It is the engine's
// own code for the same state, so the words are already in the table.
private const val NOT_STARTED_CODE = FerryErrorCode.RUNTIME_NOT_STARTED

// Every entry in one folder on a paired device. The empty path names
// that device's shared roots.
fun FerryEngine.list(keyHex: String, remotePath: String): List<Entry> =
    required().list(keyHex, remotePath)

// One file or folder on a paired device.
fun FerryEngine.stat(keyHex: String, remotePath: String): Entry =
    required().stat(keyHex, remotePath)

// At most one mebibyte. A longer ask is clamped by the engine, and a
// short answer means the end of the file.
fun FerryEngine.readAt(keyHex: String, remotePath: String, offset: ULong, len: UInt): ByteArray =
    required().readAt(keyHex, remotePath, offset, len)

// Creates the file when it does not exist. More than one mebibyte in
// one call is refused with Runtime::WriteTooLarge and writes nothing.
fun FerryEngine.writeAt(keyHex: String, remotePath: String, offset: ULong, bytes: ByteArray) =
    required().writeAt(keyHex, remotePath, offset, bytes)

// Sets a file's length. The provider truncates to zero on a truncating
// open mode.
fun FerryEngine.truncate(keyHex: String, remotePath: String, len: ULong) =
    required().truncate(keyHex, remotePath, len)

// Makes one folder. The parent must already exist.
fun FerryEngine.mkdir(keyHex: String, remotePath: String) = required().mkdir(keyHex, remotePath)

// Deletes one file, or one empty folder.
fun FerryEngine.delete(keyHex: String, remotePath: String) = required().delete(keyHex, remotePath)

// Moves or renames within one root.
fun FerryEngine.rename(keyHex: String, from: String, to: String) = required().rename(keyHex, from, to)

// The engine, or the engine's own code for not being there yet. Every
// call above reaches the network, and there is no network before create
// has built the engine. Internal: FerryEngineShare.kt's pushFiles calls
// this too.
internal fun FerryEngine.required(): Engine =
    engine ?: throw FerryException.Failed(NOT_STARTED_CODE, null)

// Tells the Files app that a root's summary has changed. Runs on an
// engine thread, from devicesChanged, and does nothing before create.
// Internal: FerryEngine's own listener calls this.
internal fun FerryEngine.notifyRoots() {
    val context = appContext ?: return
    context.contentResolver.notifyChange(
        DocumentsContract.buildRootsUri(DOCUMENTS_AUTHORITY),
        null,
    )
}
