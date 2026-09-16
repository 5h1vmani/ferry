package app.ferry

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.MediaStore
import android.provider.OpenableColumns
import java.io.File
import java.io.FileOutputStream
import java.util.UUID
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import app.ferry.engine.FerryEngine
import uniffi.ferry_runtime.BatchInfo
import uniffi.ferry_runtime.TransferState as EngineTransferState

// Turns a share, or a document picked from the "Send files" control, into
// absolute paths the engine can push, and clears a cache copy once its
// batch no longer needs it. docs/ux-fix-plan.md, item 1.
//
// The engine takes only absolute local paths. A content URI is resolved to
// one when the provider is MediaStore and the `_data` column names a file
// this app can still read. Every other URI is copied once into this app's
// own cache, because a content provider's own lifetime cannot be trusted to
// outlast a resumable transfer.
object ShareIntake {
    private const val CACHE_SUBDIR = "share"
    private const val REGISTRY_SUBDIR = "share_cache_registry"
    private const val LOG_TAG = "Ferry"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    @Volatile
    private var started = false

    // A counter FerryApp watches: each increase means "show Devices now",
    // which is what a share does when no Mac is paired, and what it does
    // whenever one is, since the transfer then shows there too.
    private val _navigateHome = MutableStateFlow(0)
    val navigateHome: StateFlow<Int> = _navigateHome.asStateFlow()

    fun requestNavigateHome() {
        _navigateHome.value += 1
    }

    // Called once, from FerryApplication.onCreate. Waits for the engine to
    // report started, since batches() reads nothing before that, then
    // removes every cache copy no known batch names, and keeps watching for
    // the rest to reach Done.
    @Synchronized
    fun start(context: Context) {
        if (started) {
            return
        }
        started = true
        val appContext = context.applicationContext
        scope.launch {
            FerryEngine.started.first { it }
            sweepOrphaned(appContext)
            FerryEngine.batches.collect { batches -> reconcile(appContext, batches) }
        }
    }

    fun isShareIntent(intent: Intent): Boolean =
        intent.action == Intent.ACTION_SEND || intent.action == Intent.ACTION_SEND_MULTIPLE

    // Every content URI a share, or the multi-select document picker, handed
    // over, in the order the sender listed them.
    fun urisFrom(intent: Intent): List<Uri> = when (intent.action) {
        Intent.ACTION_SEND -> listOfNotNull(streamExtra(intent))
        Intent.ACTION_SEND_MULTIPLE -> streamListExtra(intent)
        else -> emptyList()
    }

    @Suppress("DEPRECATION")
    private fun streamExtra(intent: Intent): Uri? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        } else {
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }

    @Suppress("DEPRECATION")
    private fun streamListExtra(intent: Intent): List<Uri> =
        (
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java)
            } else {
                intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM)
            }
            ).orEmpty()

    // Resolves every URI to an absolute path, copying into the cache where
    // needed. Blocks on disk, so the caller runs it off the main thread.
    fun resolveToLocalPaths(context: Context, uris: List<Uri>): List<String> =
        uris.mapNotNull { resolveOne(context, it) }

    private fun resolveOne(context: Context, uri: Uri): String? {
        val direct = mediaStorePath(context, uri)
        if (direct != null) {
            val file = File(direct)
            if (file.isFile && file.canRead()) {
                return direct
            }
        }
        return try {
            copyToCache(context, uri)
        } catch (e: java.io.IOException) {
            android.util.Log.w(LOG_TAG, "share content could not be read: $uri", e)
            null
        }
    }

    // The `_data` column, when the provider is MediaStore and the row
    // still names a file this app can read. Any other provider, or a
    // missing or unreadable file, falls through to a cache copy.
    private fun mediaStorePath(context: Context, uri: Uri): String? {
        if (uri.authority != MediaStore.AUTHORITY) {
            return null
        }
        context.contentResolver.query(
            uri,
            arrayOf(MediaStore.MediaColumns.DATA),
            null,
            null,
            null,
        )?.use { cursor ->
            val column = cursor.getColumnIndex(MediaStore.MediaColumns.DATA)
            if (column >= 0 && cursor.moveToFirst()) {
                return cursor.getString(column)
            }
        }
        return null
    }

    private fun copyToCache(context: Context, uri: Uri): String {
        val name = displayNameOf(context.contentResolver, uri)
            ?: uri.lastPathSegment
            ?: UUID.randomUUID().toString()
        val dir = File(File(context.cacheDir, CACHE_SUBDIR), UUID.randomUUID().toString())
        dir.mkdirs()
        val target = File(dir, name)
        context.contentResolver.openInputStream(uri)?.use { input ->
            FileOutputStream(target).use { output -> input.copyTo(output) }
        }
        return target.absolutePath
    }

    private fun displayNameOf(resolver: ContentResolver, uri: Uri): String? {
        resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            val column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
            if (column >= 0 && cursor.moveToFirst()) {
                return cursor.getString(column)
            }
        }
        return null
    }

    // True for a path this object copied into the cache, as opposed to a
    // path resolved straight from MediaStore, which this app never deletes.
    private fun isCacheCopy(context: Context, path: String): Boolean =
        path.startsWith(File(context.cacheDir, CACHE_SUBDIR).absolutePath)

    // Records which cache copies belong to one push_files batch, so they
    // can be deleted once that batch reaches Done. Called after pushFiles
    // returns a batch id; a call with no cache copies writes nothing.
    fun registerBatch(context: Context, batchId: String, localPaths: List<String>) {
        val cachePaths = localPaths.filter { isCacheCopy(context, it) }
        if (cachePaths.isEmpty()) {
            return
        }
        val registryDir = File(context.filesDir, REGISTRY_SUBDIR)
        registryDir.mkdirs()
        File(registryDir, batchId).writeText(cachePaths.joinToString("\n"))
    }

    // Removes every cache copy whose batch is Done, or whose batch no
    // engine record names any more: docs/ux-fix-plan.md item 1, "at app
    // start when no transfer names it".
    private fun sweepOrphaned(context: Context) {
        val entries = File(context.filesDir, REGISTRY_SUBDIR).listFiles().orEmpty()
        if (entries.isEmpty()) {
            return
        }
        val known = FerryEngine.batches.value.associateBy { it.id }
        for (entry in entries) {
            val batch = known[entry.name]
            if (batch == null || batch.state == EngineTransferState.DONE) {
                deleteRegistered(entry)
            }
        }
    }

    // The live half of the same rule: a batch that was still running at
    // start reaches Done later, observed from the batches flow FerryEngine
    // already exposes.
    private fun reconcile(context: Context, batches: List<BatchInfo>) {
        val entries = File(context.filesDir, REGISTRY_SUBDIR).listFiles().orEmpty()
        if (entries.isEmpty()) {
            return
        }
        val done = batches.filter { it.state == EngineTransferState.DONE }.map { it.id }.toSet()
        for (entry in entries) {
            if (entry.name in done) {
                deleteRegistered(entry)
            }
        }
    }

    private fun deleteRegistered(registryFile: File) {
        registryFile.readLines().forEach { path ->
            val file = File(path)
            file.delete()
            file.parentFile?.delete()
        }
        registryFile.delete()
    }
}
