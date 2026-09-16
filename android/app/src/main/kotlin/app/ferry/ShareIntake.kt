package app.ferry

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.provider.OpenableColumns
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
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

    // Audit finding 3, the three constants docs/engine-contract.md item 5
    // records. A hostile provider could otherwise serve an endless stream,
    // fill the phone's storage, or make a share out of hundreds of files.
    private const val MAX_SHARE_URIS = 100
    private const val MAX_SHARE_COPY_BYTES = 4L * 1024 * 1024 * 1024
    private const val MIN_FREE_BYTES_AFTER_COPY = 512L * 1024 * 1024
    private const val COPY_BUFFER_BYTES = 1 shl 20

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    // Runs a share's disk and network work on this object's own IO scope,
    // the same scope resolve, sweepOrphaned, and the rest of this file
    // use. MainActivity calls this instead of starting its own thread for
    // a share intent's resolve-then-push work, so both use the same kind
    // of background execution. docs/audits/principles.md row P19.
    fun launchOnIo(block: suspend () -> Unit) {
        scope.launch { block() }
    }

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

    // An app-side fault a share or a push into its folder can hit, with no
    // engine code of its own. FerryApp turns each into the same three
    // parts an engine error would show, from strings.xml, through the same
    // ErrorBlock: docs/voice.md rule 10, nothing is hidden. Set at the
    // start of every resolve call, so an error from an earlier share does
    // not outlive it; audit finding 10 also clears it when Devices is left.
    sealed class AppError {
        // The sharing app's own intent carried no read grant: audit
        // finding 1.
        data object NotGranted : AppError()
        data class Unreadable(val name: String) : AppError()

        // The file passed MAX_SHARE_COPY_BYTES, or copying it would have
        // left under MIN_FREE_BYTES_AFTER_COPY free: audit finding 3.
        data class TooLarge(val name: String) : AppError()

        // The share named more than MAX_SHARE_URIS files: audit finding 3.
        data class TooMany(val count: Int, val max: Int) : AppError()

        // The target device lists no shared root at all: audit finding 8.
        data class NoLandingFolder(val deviceName: String) : AppError()
    }

    private val _appError = MutableStateFlow<AppError?>(null)
    val appError: StateFlow<AppError?> = _appError.asStateFlow()

    fun setAppError(error: AppError) {
        _appError.value = error
    }

    fun clearAppError() {
        _appError.value = null
    }

    // What one resolve call found: every path, ready to push, or the
    // reason none of them will be. A share with any fault sends none of
    // its files, so a person is never left guessing which went and which
    // did not.
    sealed class Resolution {
        data class Success(val localPaths: List<String>) : Resolution()
        data object Unreadable : Resolution()
        data object TooLarge : Resolution()
        data object TooMany : Resolution()
        data object NotGranted : Resolution()
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
    //
    // Stops at the first file it cannot read and sends none of the paths:
    // a share that partly lands is a person guessing which files made it.
    //
    // `hasReadGrant` is `intent.flags` carrying `FLAG_GRANT_READ_URI_PERMISSION`
    // for a share, and always true for the document picker, which the
    // system itself always grants. Audit finding 1: the system gives that
    // grant only to a sender that could read the file itself, so it is the
    // proof docs/engine-contract.md item 5 asks for; without it, nothing is
    // read.
    fun resolve(context: Context, uris: List<Uri>, hasReadGrant: Boolean): Resolution {
        clearAppError()
        if (!hasReadGrant) {
            setAppError(AppError.NotGranted)
            return Resolution.NotGranted
        }
        if (uris.size > MAX_SHARE_URIS) {
            setAppError(AppError.TooMany(uris.size, MAX_SHARE_URIS))
            return Resolution.TooMany
        }
        val paths = mutableListOf<String>()
        for (uri in uris) {
            val path = try {
                resolveOne(context, uri)
            } catch (e: CopyTooLargeException) {
                setAppError(AppError.TooLarge(nameForError(context, uri)))
                return Resolution.TooLarge
            }
            if (path == null) {
                setAppError(AppError.Unreadable(nameForError(context, uri)))
                return Resolution.Unreadable
            }
            paths += path
        }
        return Resolution.Success(paths)
    }

    // The name to show for a file that could not be read: the same display
    // name copyToCache would have used, or the URI itself when the
    // provider gives none or refuses to say.
    private fun nameForError(context: Context, uri: Uri): String {
        val name = try {
            displayNameOf(context.contentResolver, uri)
        } catch (t: Throwable) {
            null
        }
        return name ?: uri.lastPathSegment ?: uri.toString()
    }

    // Audit finding 2. `ContentResolver.query` and `openInputStream` throw
    // `SecurityException` when the sender set no grant flag, and
    // `IllegalArgumentException` for a malformed URI. Both `mediaStorePath`
    // and `copyToCache` are inside this one try, not just the second of
    // them, so a fault in either becomes an ordinary Unreadable result
    // rather than an uncaught exception on the caller's thread.
    private fun resolveOne(context: Context, uri: Uri): String? {
        return try {
            val direct = mediaStorePath(context, uri)
            if (direct != null && isUnderExternalStorage(direct)) {
                val file = File(direct)
                if (file.isFile && file.canRead()) {
                    return direct
                }
            }
            copyToCache(context, uri)
        } catch (e: CopyTooLargeException) {
            // Caught by name in resolve, not folded into the generic
            // unreadable case below: audit finding 3.
            throw e
        } catch (t: Throwable) {
            android.util.Log.w(LOG_TAG, "share content could not be read: $uri", t)
            null
        }
    }

    // The engine's LocalFs opens the parent folder of whatever path it is
    // given (docs/engine-contract.md item 5), so a `_data` path outside the
    // phone's own shared root is refused rather than handed to the engine
    // directly; it is copied into the cache instead, the same as any path
    // this app cannot vouch for.
    private fun isUnderExternalStorage(path: String): Boolean {
        val root = Environment.getExternalStorageDirectory().canonicalFile.path
        val candidate = try {
            File(path).canonicalFile.path
        } catch (e: IOException) {
            return false
        }
        return candidate == root || candidate.startsWith(root + File.separator)
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

    // The name comes from another app's provider, so it is not trusted with
    // a raw file path: a name such as "../../shared_prefs/x.xml" would
    // write outside the fresh folder made for it below.
    private fun copyToCache(context: Context, uri: Uri): String {
        if (uri.scheme != ContentResolver.SCHEME_CONTENT) {
            throw IOException("refused scheme: ${uri.scheme}")
        }
        val rawName = displayNameOf(context.contentResolver, uri)
            ?: uri.lastPathSegment
            ?: UUID.randomUUID().toString()
        val name = sanitizedFileName(rawName)
        val dir = File(File(context.cacheDir, CACHE_SUBDIR), UUID.randomUUID().toString())
        dir.mkdirs()
        val target = File(dir, name)
        // Belt and braces on top of sanitizedFileName: the copy is refused
        // unless it still lands directly inside the folder made for it.
        if (target.canonicalFile.parentFile != dir.canonicalFile) {
            throw IOException("refused path escape: $rawName")
        }
        val input = context.contentResolver.openInputStream(uri)
            ?: throw IOException("no stream for $uri")
        try {
            input.use { source ->
                FileOutputStream(target).use { output -> copyWithLimit(source, output, target) }
            }
        } catch (t: Throwable) {
            target.delete()
            dir.delete()
            throw t
        }
        return target.absolutePath
    }

    // Audit finding 3. A hostile provider could serve an endless stream and
    // fill the phone's storage, whether or not it declared a size, so this
    // is checked as bytes arrive rather than trusted from a header. The
    // partial file is removed by copyToCache's own catch once this throws.
    private fun copyWithLimit(input: InputStream, output: OutputStream, target: File) {
        val buffer = ByteArray(COPY_BUFFER_BYTES)
        var total = 0L
        while (true) {
            val read = input.read(buffer)
            if (read < 0) {
                break
            }
            total += read
            if (total > MAX_SHARE_COPY_BYTES) {
                throw CopyTooLargeException("copy of ${target.name} passed $MAX_SHARE_COPY_BYTES bytes")
            }
            val free = target.parentFile?.usableSpace ?: 0L
            if (free < MIN_FREE_BYTES_AFTER_COPY) {
                throw CopyTooLargeException(
                    "copy of ${target.name} would leave under $MIN_FREE_BYTES_AFTER_COPY bytes free",
                )
            }
            output.write(buffer, 0, read)
        }
    }

    private class CopyTooLargeException(message: String) : IOException(message)

    // Reduces a name from another app to a plain file name: its last path
    // segment, or a fresh UUID when that segment is empty, ".", "..", or
    // holds a control character or a slash.
    private fun sanitizedFileName(rawName: String): String {
        val lastSegment = rawName.substringAfterLast('/')
        val invalid = lastSegment.isEmpty() ||
            lastSegment == "." ||
            lastSegment == ".." ||
            lastSegment.contains('/') ||
            lastSegment.any { it.isISOControl() }
        return if (invalid) UUID.randomUUID().toString() else lastSegment
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

    // Records which cache copies belong to one share, under an id the
    // caller already has, so they can be deleted once that id names a
    // batch that reaches Done. Audit finding 7: called with a request id
    // before the push, not only after with the real batch id, since a
    // push that never returns one must not lose track of its copies.
    // A call with no cache copies writes nothing.
    fun registerBatch(context: Context, id: String, localPaths: List<String>) {
        val cachePaths = localPaths.filter { isCacheCopy(context, it) }
        if (cachePaths.isEmpty()) {
            return
        }
        val registryDir = File(context.filesDir, REGISTRY_SUBDIR)
        registryDir.mkdirs()
        File(registryDir, id).writeText(cachePaths.joinToString("\n"))
    }

    // Moves a registration from the request id it was made under to the
    // real batch id pushFiles returned, once it succeeds: audit finding 7.
    fun renameRegistration(context: Context, fromId: String, toId: String) {
        val registryDir = File(context.filesDir, REGISTRY_SUBDIR)
        val from = File(registryDir, fromId)
        if (from.exists()) {
            from.renameTo(File(registryDir, toId))
        }
    }

    // Removes every cache copy whose batch is Done, or whose batch no
    // engine record names any more: docs/ux-fix-plan.md item 1, "at app
    // start when no transfer names it". Also removes every folder under
    // the cache itself that no surviving registry entry names: audit
    // finding 7. A push that threw before pushFiles returned a batch id
    // left its copy registered only under a request id no batch will ever
    // match, and the registry-only sweep above never looked at the cache
    // folder to find it.
    private fun sweepOrphaned(context: Context) {
        val registryDir = File(context.filesDir, REGISTRY_SUBDIR)
        val known = FerryEngine.batches.value.associateBy { it.id }
        val survivingPaths = mutableListOf<String>()
        for (entry in registryDir.listFiles().orEmpty()) {
            val batch = known[entry.name]
            if (batch == null || batch.state == EngineTransferState.DONE) {
                deleteRegistered(entry)
            } else {
                survivingPaths += entry.readLines()
            }
        }
        deleteUnnamedCacheFolders(context, survivingPaths)
    }

    // Every per-share folder under the cache that no path in
    // survivingPaths sits inside is removed outright: nothing still
    // pending names it, so it is either already spent or was never
    // registered at all before the process that made it died.
    private fun deleteUnnamedCacheFolders(context: Context, survivingPaths: List<String>) {
        val namedDirs = survivingPaths.mapNotNull { File(it).parentFile?.absolutePath }.toSet()
        val cacheRoot = File(context.cacheDir, CACHE_SUBDIR)
        for (dir in cacheRoot.listFiles().orEmpty()) {
            if (dir.absolutePath !in namedDirs) {
                dir.deleteRecursively()
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
