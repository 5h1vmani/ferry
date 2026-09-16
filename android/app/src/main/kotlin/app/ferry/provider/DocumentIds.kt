package app.ferry.provider

import android.provider.DocumentsContract.Document
import android.system.ErrnoException
import android.system.OsConstants
import android.webkit.MimeTypeMap
import app.ferry.FerryErrorCode
import uniffi.ferry_runtime.FerryException
import java.io.FileNotFoundException

// The id and path helpers, and the errno map, behind FerryDocumentsProvider.
// Split from FerryDocumentsProvider.kt, docs/audits/principles.md row P12.
//
// A document id is the device key hex, a slash, and the root-relative path.
// Nothing here touches the engine, the provider, or a Context: every
// function is a pure conversion the provider's own methods call.

// Every path a document id carries is split on this. Internal, since
// FerryDocumentsProvider.isChildDocument and addRow test it directly too.
internal const val PATH_SEPARATOR = '/'

// What a name with no extension, or an extension the platform table does
// not know, is served as.
private const val DEFAULT_MIME_TYPE = "application/octet-stream"

// Every code for a path the engine will not accept at all.
private const val PATH_ERROR_PREFIX = "PathError::"

// The device key hex, which is everything before the first slash.
fun keyOf(documentId: String): String = documentId.substringBefore(PATH_SEPARATOR)

// The root-relative path, which is everything after the first slash. Empty
// for a device's own document.
fun pathOf(documentId: String): String = documentId.substringAfter(PATH_SEPARATOR, "")

fun documentIdOf(keyHex: String, path: String): String =
    if (path.isEmpty()) keyHex else "$keyHex$PATH_SEPARATOR$path"

fun childPath(parentPath: String, name: String): String =
    if (parentPath.isEmpty()) name else "$parentPath$PATH_SEPARATOR$name"

fun parentPathOf(path: String): String = path.substringBeforeLast(PATH_SEPARATOR, "")

// How many segments a root-relative path holds. Zero is the device itself,
// one is a shared root on the Mac, two and more are inside a shared root.
fun depthOf(path: String): Int =
    if (path.isEmpty()) 0 else path.count { it == PATH_SEPARATOR } + 1

// What may be done to one document.
//
// The device itself takes nothing: the Mac decides what it shares, and the
// engine makes no new root. A shared root takes creation only, because the
// engine refuses mkdir, delete, rename, and writeAt on a one segment path.
// Everything under a shared root is an ordinary file or folder.
fun flagsOf(depth: Int, directory: Boolean): Int = when (depth) {
    0 -> 0
    1 -> Document.FLAG_DIR_SUPPORTS_CREATE
    else -> {
        val common = Document.FLAG_SUPPORTS_WRITE or
            Document.FLAG_SUPPORTS_DELETE or
            Document.FLAG_SUPPORTS_RENAME
        if (directory) common or Document.FLAG_DIR_SUPPORTS_CREATE else common
    }
}

fun mimeTypeOf(name: String): String {
    val extension = name.substringAfterLast('.', "")
    if (extension.isEmpty()) {
        return DEFAULT_MIME_TYPE
    }
    return MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension.lowercase())
        ?: DEFAULT_MIME_TYPE
}

// A provider method may report only FileNotFoundException, so every engine
// refusal arrives as one. The message carries the document id and the
// engine's own code, never English: the words for a code live in the
// generated table, and nothing reads this message but a log.
fun refused(documentId: String, error: FerryException): FileNotFoundException {
    val code = (error as? FerryException.Failed)?.code.orEmpty()
    return FileNotFoundException("$documentId $code")
}

// The closest errno for one engine code. The proxy file descriptor callback
// answers the kernel, which knows errno and nothing else.
fun errnoOf(function: String, error: FerryException): ErrnoException {
    val code = (error as? FerryException.Failed)?.code.orEmpty()
    val errno = when (code) {
        FerryErrorCode.OP_ERROR_NOT_FOUND -> OsConstants.ENOENT
        FerryErrorCode.OP_ERROR_ALREADY_EXISTS -> OsConstants.EEXIST
        FerryErrorCode.OP_ERROR_NOT_EMPTY -> OsConstants.ENOTEMPTY
        FerryErrorCode.OP_ERROR_PERMISSION_DENIED -> OsConstants.EACCES
        FerryErrorCode.OP_ERROR_IS_A_DIRECTORY -> OsConstants.EISDIR
        FerryErrorCode.OP_ERROR_NOT_A_DIRECTORY -> OsConstants.ENOTDIR
        FerryErrorCode.OP_ERROR_UNSUPPORTED -> OsConstants.EOPNOTSUPP
        FerryErrorCode.OP_ERROR_INVALID_PATH -> OsConstants.EINVAL
        FerryErrorCode.OP_ERROR_RANGE_TOO_LARGE -> OsConstants.EINVAL
        FerryErrorCode.RUNTIME_WRITE_TOO_LARGE -> OsConstants.EINVAL
        FerryErrorCode.RUNTIME_NOT_REACHABLE -> OsConstants.EHOSTUNREACH
        FerryErrorCode.RUNTIME_NOT_PAIRED -> OsConstants.ENODEV
        FerryErrorCode.RUNTIME_NOT_STARTED -> OsConstants.EAGAIN
        else -> if (code.startsWith(PATH_ERROR_PREFIX)) OsConstants.EINVAL else OsConstants.EIO
    }
    return ErrnoException(function, errno)
}
