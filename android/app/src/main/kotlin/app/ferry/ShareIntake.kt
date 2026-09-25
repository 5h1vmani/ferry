package app.ferry

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Process
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
    // Visible to ShareCacheRegistry, which decides whether a path was
    // copied into this folder without duplicating the folder name.
    internal const val CACHE_SUBDIR = "share"
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
        // A shared URI failed the share check in mayRead: audit finding
        // 1, and docs/audits/android-share-grant.md finding 1.
        data object NotGranted : AppError()
        data class Unreadable(val name: String) : AppError()

        // The file passed MAX_SHARE_COPY_BYTES, or copying it would have
        // left under MIN_FREE_BYTES_AFTER_COPY free: audit finding 3.
        data class TooLarge(val name: String) : AppError()

        // The share named more than MAX_SHARE_URIS files: audit finding 3.
        data class TooMany(val count: Int, val max: Int) : AppError()
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

    // Called once, from FerryApplication.onCreate. Takes a snapshot of the
    // cache registry before anything else can write to it, then waits for
    // the engine to report started, since batches() reads nothing before
    // that, then removes every cache copy in that snapshot no known batch
    // names, and keeps watching for the rest to reach Done: docs/audits/
    // principles-fixes.md row 6. The snapshot is taken here, on this
    // thread, before the coroutine below is even launched, so it can only
    // hold what a past run left behind, never a share this run is making.
    @Synchronized
    fun start(context: Context) {
        if (started) {
            return
        }
        started = true
        val appContext = context.applicationContext
        val atStart = ShareCacheRegistry.snapshotAtStart(appContext)
        scope.launch {
            FerryEngine.started.first { it }
            ShareCacheRegistry.sweepOrphaned(appContext, atStart)
            FerryEngine.batches.collect { batches -> ShareCacheRegistry.reconcile(appContext, batches) }
        }
    }

    fun isShareIntent(intent: Intent): Boolean =
        intent.action == Intent.ACTION_SEND || intent.action == Intent.ACTION_SEND_MULTIPLE

    // The files a share sends: EXTRA_STREAM, single or as a list depending
    // on the action, in the order the sender listed them, with no repeats.
    //
    // ClipData is not read. A sender may put a preview image there, for
    // example a thumbnail for a shared link, and that image is not a file
    // the person chose to send. Every URI returned here gets its own check
    // in resolve(), so the ClipData grant flag no longer decides anything.
    // docs/audits/android-share-grant.md finding 2.
    fun urisFrom(intent: Intent): List<Uri> = when (intent.action) {
        Intent.ACTION_SEND -> listOfNotNull(streamExtra(intent))
        Intent.ACTION_SEND_MULTIPLE -> streamListExtra(intent).distinct()
        else -> emptyList()
    }

    // The share check. docs/engine-contract.md item 5 lets Ferry read a
    // file only when the sender could read it too. Ferry holds all files
    // access, so the fact that Ferry can open a URI proves nothing.
    // docs/audits/android-share-grant.md finding 1.
    //
    // A URI passes only when all three rules hold.
    //
    // 1. It is a content URI. Android has no grant for a file:// path, so
    //    Ferry could read one only with its own all files access.
    // 2. Ferry holds a read grant for this exact URI. checkUriPermission
    //    counts only grants in Android's grant table. It never counts all
    //    files access. Android adds a grant only for an app that can read
    //    the URI itself, so the grant proves that some such app chose to
    //    hand this file to Ferry.
    // 3. senderCanRead says the sender itself can read it. On Android 15
    //    and later, Android answers this for the app that sent the share.
    //    On Android 14 and earlier, no API names the sender, so rule 2 is
    //    the only proof. The known limit there is a grant left over from
    //    an earlier share, because rule 2 cannot tell who made a grant.
    private fun mayRead(context: Context, uri: Uri, senderCanRead: (Uri) -> Boolean): Boolean {
        if (uri.scheme != ContentResolver.SCHEME_CONTENT) {
            return false
        }
        val ferryHasGrant = context.checkUriPermission(
            uri,
            Process.myPid(),
            Process.myUid(),
            Intent.FLAG_GRANT_READ_URI_PERMISSION,
        ) == PackageManager.PERMISSION_GRANTED
        return ferryHasGrant && senderCanRead(uri)
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
    // Every URI must pass mayRead before Ferry opens it. senderCanRead is
    // rule 3 of that check. MainActivity builds it for a share, and passes
    // one that always answers true for the system document picker, where
    // the person chose each file and the picker's own grant is the proof.
    fun resolve(context: Context, uris: List<Uri>, senderCanRead: (Uri) -> Boolean): Resolution {
        clearAppError()
        if (uris.size > MAX_SHARE_URIS) {
            setAppError(AppError.TooMany(uris.size, MAX_SHARE_URIS))
            return Resolution.TooMany
        }
        val paths = mutableListOf<String>()
        for (uri in uris) {
            if (!mayRead(context, uri, senderCanRead)) {
                setAppError(AppError.NotGranted)
                return Resolution.NotGranted
            }
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

}
