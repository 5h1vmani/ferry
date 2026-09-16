package app.ferry.provider

import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.os.CancellationSignal
import android.os.Handler
import android.os.HandlerThread
import android.os.ParcelFileDescriptor
import android.os.ProxyFileDescriptorCallback
import android.os.storage.StorageManager
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import app.ferry.R
import app.ferry.engine.FerryEngine
import app.ferry.engine.delete
import app.ferry.engine.list
import app.ferry.engine.mkdir
import app.ferry.engine.readAt
import app.ferry.engine.rename
import app.ferry.engine.stat
import app.ferry.engine.truncate
import app.ferry.engine.writeAt
import app.ferry.model.ConnectionState
import app.ferry.model.DeviceInfo
import app.ferry.model.Transport
import app.ferry.model.toUi
import uniffi.ferry_runtime.Entry
import uniffi.ferry_runtime.EntryKind
import uniffi.ferry_runtime.FerryException
import java.io.FileNotFoundException

// The Mac's shared folders, inside the phone's Files app and inside every
// app's open and save dialog. Job 8, docs/engine-contract.md item 19.
//
// This is the mirror of the Finder mount. The Mac reaches the phone's files
// through the WebDAV bridge; the phone reaches the Mac's files through this
// provider. Both sit on the same file operations layer, so both see the
// same roots, the same paths, and the same refusals.
//
// A document id is the device key hex, a slash, and the root-relative path.
// A root's document id is the key hex on its own, and its children are
// list(key, ""), which is the Mac's shared roots.
//
// Every engine call below blocks on the network. A provider method runs on
// a binder thread, which may block, so each one calls straight through. The
// proxy file descriptor callback is the exception: it runs on the handler
// given to openProxyFileDescriptor, and that handler is this provider's own
// thread, made once in onCreate and reused for every open file.
//
// Nothing here formats English. The two words a person reads, the root
// summary and the refusal of an open mode, are keys in strings.xml, and the
// words for an engine error code live in the generated table.
class FerryDocumentsProvider : DocumentsProvider() {

    // The one thread every ProxyFileDescriptorCallback runs on.
    private lateinit var callbackThread: HandlerThread

    private lateinit var callbackHandler: Handler

    override fun onCreate(): Boolean {
        // A content provider is created before Application.onCreate, so the
        // engine does not exist yet. Nothing here touches it: queryRoots
        // reports no rows until FerryEngine.start has succeeded.
        callbackThread = HandlerThread(CALLBACK_THREAD_NAME)
        callbackThread.start()
        callbackHandler = Handler(callbackThread.looper)
        return true
    }

    // Every paired device, with its name as the title and how Ferry reaches
    // it right now as the summary.
    //
    // No rows while the engine is not started. start() opens the shared
    // root, so it fails until all files access is granted, and a root that
    // cannot answer a listing is worse than no root at all.
    override fun queryRoots(projection: Array<out String>?): Cursor {
        val cursor = MatrixCursor(projection ?: DEFAULT_ROOT_COLUMNS)
        if (!FerryEngine.started.value) {
            return cursor
        }
        for (device in devices()) {
            cursor.newRow()
                .add(Root.COLUMN_ROOT_ID, device.id)
                .add(Root.COLUMN_DOCUMENT_ID, device.id)
                .add(Root.COLUMN_TITLE, device.name)
                .add(Root.COLUMN_SUMMARY, summaryOf(device))
                .add(Root.COLUMN_FLAGS, Root.FLAG_SUPPORTS_CREATE or Root.FLAG_SUPPORTS_IS_CHILD)
                .add(Root.COLUMN_ICON, R.mipmap.ic_launcher)
                .add(Root.COLUMN_MIME_TYPES, ALL_MIME_TYPES)
        }
        return cursor
    }

    // One document: the device itself, or one entry on it.
    override fun queryDocument(documentId: String, projection: Array<out String>?): Cursor {
        val cursor = MatrixCursor(projection ?: DEFAULT_DOCUMENT_COLUMNS)
        val keyHex = keyOf(documentId)
        val path = pathOf(documentId)
        if (path.isEmpty()) {
            val device = devices().firstOrNull { it.id == keyHex }
                ?: throw FileNotFoundException(documentId)
            addRow(
                cursor = cursor,
                keyHex = keyHex,
                path = "",
                name = device.name,
                directory = true,
                size = 0L,
                modifiedUnixSecs = 0L,
            )
            return cursor
        }
        val entry = try {
            FerryEngine.stat(keyHex, path)
        } catch (e: FerryException) {
            throw refused(documentId, e)
        }
        addRow(cursor, keyHex, path, entry)
        return cursor
    }

    // One folder's entries. The device document's children are the Mac's
    // shared roots, which is what the engine answers for an empty path.
    override fun queryChildDocuments(
        parentDocumentId: String,
        projection: Array<out String>?,
        sortOrder: String?,
    ): Cursor {
        val cursor = MatrixCursor(projection ?: DEFAULT_DOCUMENT_COLUMNS)
        val keyHex = keyOf(parentDocumentId)
        val parentPath = pathOf(parentDocumentId)
        val entries = try {
            FerryEngine.list(keyHex, parentPath)
        } catch (e: FerryException) {
            throw refused(parentDocumentId, e)
        }
        for (entry in entries) {
            addRow(cursor, keyHex, childPath(parentPath, entry.name), entry)
        }
        // So a create, a delete, or a rename redraws the open folder.
        cursor.setNotificationUri(
            ctx().contentResolver,
            DocumentsContract.buildChildDocumentsUri(
                FerryEngine.DOCUMENTS_AUTHORITY,
                parentDocumentId,
            ),
        )
        return cursor
    }

    // A file descriptor backed by the engine, not by a local copy. Job 8
    // asks for no copy step, so nothing is downloaded here: every read and
    // every write goes to the Mac as it happens.
    override fun openDocument(
        documentId: String,
        mode: String,
        signal: CancellationSignal?,
    ): ParcelFileDescriptor {
        val open = openModeOf(mode)
            ?: throw UnsupportedOperationException(
                ctx().getString(R.string.documents_open_mode_unsupported, mode),
            )
        val keyHex = keyOf(documentId)
        val path = pathOf(documentId)
        if (path.isEmpty()) {
            // The device itself is a folder. It holds no bytes.
            throw FileNotFoundException(documentId)
        }
        // The offset every read and write is measured from. Zero for every
        // mode but append, where the file's current end is where the
        // writer's own first byte belongs.
        var base = 0L
        try {
            if (open.truncate) {
                FerryEngine.truncate(keyHex, path, 0uL)
            }
            if (open.append) {
                base = FerryEngine.stat(keyHex, path).size.toLong()
            }
        } catch (e: FerryException) {
            throw refused(documentId, e)
        }
        val access = when {
            open.read && open.write -> ParcelFileDescriptor.MODE_READ_WRITE
            open.write -> ParcelFileDescriptor.MODE_WRITE_ONLY
            else -> ParcelFileDescriptor.MODE_READ_ONLY
        }
        val storage = ctx().getSystemService(StorageManager::class.java)
        return storage.openProxyFileDescriptor(
            access,
            DocumentCallback(keyHex, path, base),
            callbackHandler,
        )
    }

    // A folder, or an empty file. Both are one engine call.
    override fun createDocument(
        parentDocumentId: String,
        mimeType: String,
        displayName: String,
    ): String {
        val keyHex = keyOf(parentDocumentId)
        val path = childPath(pathOf(parentDocumentId), displayName)
        try {
            if (mimeType == Document.MIME_TYPE_DIR) {
                FerryEngine.mkdir(keyHex, path)
            } else {
                FerryEngine.writeAt(keyHex, path, 0uL, ByteArray(0))
            }
        } catch (e: FerryException) {
            throw refused(parentDocumentId, e)
        }
        notifyChildren(parentDocumentId)
        return documentIdOf(keyHex, path)
    }

    // One file, or one empty folder. The wire has no recursive delete, so a
    // folder that still holds something is refused by the engine.
    override fun deleteDocument(documentId: String) {
        val keyHex = keyOf(documentId)
        val path = pathOf(documentId)
        try {
            FerryEngine.delete(keyHex, path)
        } catch (e: FerryException) {
            throw refused(documentId, e)
        }
        notifyChildren(documentIdOf(keyHex, parentPathOf(path)))
    }

    override fun renameDocument(documentId: String, displayName: String): String {
        val keyHex = keyOf(documentId)
        val path = pathOf(documentId)
        val parentPath = parentPathOf(path)
        val target = childPath(parentPath, displayName)
        try {
            FerryEngine.rename(keyHex, path, target)
        } catch (e: FerryException) {
            throw refused(documentId, e)
        }
        notifyChildren(documentIdOf(keyHex, parentPath))
        return documentIdOf(keyHex, target)
    }

    // Child, grandchild, or deeper. A document id carries its whole path,
    // so the answer is a prefix test and costs no round trip.
    override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean =
        documentId.startsWith("$parentDocumentId$PATH_SEPARATOR")

    // Reads, writes, and sizes one open file through the engine.
    //
    // Every method here runs on callbackHandler's thread and may block. A
    // refusal becomes an ErrnoException, which is what the callback
    // contract asks for: the kernel has no way to carry a Ferry code.
    private inner class DocumentCallback(
        private val keyHex: String,
        private val path: String,
        private val base: Long,
    ) : ProxyFileDescriptorCallback() {

        override fun onGetSize(): Long = try {
            FerryEngine.stat(keyHex, path).size.toLong()
        } catch (e: FerryException) {
            throw errnoOf("onGetSize", e)
        }

        override fun onRead(offset: Long, size: Int, data: ByteArray): Int {
            var done = 0
            try {
                while (done < size) {
                    val want = minOf(size - done, MAX_CALL_BYTES)
                    val chunk = FerryEngine.readAt(
                        keyHex,
                        path,
                        (base + offset + done).toULong(),
                        want.toUInt(),
                    )
                    chunk.copyInto(data, done)
                    done += chunk.size
                    if (chunk.size < want) {
                        // Fewer bytes than asked for is the end of the file.
                        break
                    }
                }
            } catch (e: FerryException) {
                throw errnoOf("onRead", e)
            }
            return done
        }

        override fun onWrite(offset: Long, size: Int, data: ByteArray): Int {
            var done = 0
            try {
                while (done < size) {
                    // The engine refuses more than one mebibyte in one call,
                    // so a larger write is split rather than sent and lost.
                    val len = minOf(size - done, MAX_CALL_BYTES)
                    FerryEngine.writeAt(
                        keyHex,
                        path,
                        (base + offset + done).toULong(),
                        data.copyOfRange(done, done + len),
                    )
                    done += len
                }
            } catch (e: FerryException) {
                throw errnoOf("onWrite", e)
            }
            return done
        }

        // Nothing is held back, so there is nothing to flush.
        override fun onFsync() = Unit

        // The engine owns the connection and the pool. Closing one file
        // releases nothing of this provider's own.
        override fun onRelease() = Unit
    }

    private fun ctx(): Context = checkNotNull(context)

    // Every paired device, as the screens see it. Mapping.kt is the one
    // file that reads an engine device record, and this provider reads the
    // same answer the Devices screen reads, so the two never disagree.
    private fun devices(): List<DeviceInfo> = FerryEngine.devices.value.map { it.toUi() }

    private fun summaryOf(device: DeviceInfo): String =
        if (device.connectionState is ConnectionState.NotReachable) {
            ctx().getString(R.string.transport_not_reachable)
        } else {
            ctx().getString(R.string.documents_root_reachable, transportWord(device.transport))
        }

    private fun transportWord(transport: Transport): String = when (transport) {
        Transport.Usb -> ctx().getString(R.string.transport_usb)
        Transport.Wifi -> ctx().getString(R.string.transport_wifi)
    }

    private fun addRow(cursor: MatrixCursor, keyHex: String, path: String, entry: Entry) {
        addRow(
            cursor = cursor,
            keyHex = keyHex,
            path = path,
            // stat of a shared root names the root; deeper it names the
            // file. An empty name means the path itself is the answer.
            name = entry.name.ifEmpty { path.substringAfterLast(PATH_SEPARATOR) },
            directory = entry.kind == EntryKind.DIRECTORY,
            size = entry.size.toLong(),
            modifiedUnixSecs = entry.modifiedUnixSecs,
        )
    }

    private fun addRow(
        cursor: MatrixCursor,
        keyHex: String,
        path: String,
        name: String,
        directory: Boolean,
        size: Long,
        modifiedUnixSecs: Long,
    ) {
        val mimeType = if (directory) Document.MIME_TYPE_DIR else mimeTypeOf(name)
        cursor.newRow()
            .add(Document.COLUMN_DOCUMENT_ID, documentIdOf(keyHex, path))
            .add(Document.COLUMN_DISPLAY_NAME, name)
            .add(Document.COLUMN_MIME_TYPE, mimeType)
            .add(Document.COLUMN_SIZE, size)
            // The engine reports zero when no real time is known. Android
            // reads milliseconds, and shows no time when the value is null.
            .add(
                Document.COLUMN_LAST_MODIFIED,
                if (modifiedUnixSecs == 0L) null else modifiedUnixSecs * MILLISECONDS_PER_SECOND,
            )
            .add(Document.COLUMN_FLAGS, flagsOf(depthOf(path), directory))
    }

    private fun notifyChildren(parentDocumentId: String) {
        ctx().contentResolver.notifyChange(
            DocumentsContract.buildChildDocumentsUri(
                FerryEngine.DOCUMENTS_AUTHORITY,
                parentDocumentId,
            ),
            null,
        )
    }
}

// The thread every open file's callback runs on. One thread serves every
// open file, as the contract asks.
private const val CALLBACK_THREAD_NAME = "ferry-documents"

// The most the engine carries in one read or one write. Item 19 sets both
// MAX_READ_LEN and MAX_WRITE_LEN to one mebibyte. A read over it is clamped;
// a write over it is refused, so the callback splits its writes here.
private const val MAX_CALL_BYTES = 1 shl 20

private const val MILLISECONDS_PER_SECOND = 1000L

// Ferry serves whatever the Mac shares, so the picker offers every type.
private const val ALL_MIME_TYPES = "*/*"

private val DEFAULT_ROOT_COLUMNS = arrayOf(
    Root.COLUMN_ROOT_ID,
    Root.COLUMN_DOCUMENT_ID,
    Root.COLUMN_TITLE,
    Root.COLUMN_SUMMARY,
    Root.COLUMN_FLAGS,
    Root.COLUMN_ICON,
    Root.COLUMN_MIME_TYPES,
)

private val DEFAULT_DOCUMENT_COLUMNS = arrayOf(
    Document.COLUMN_DOCUMENT_ID,
    Document.COLUMN_DISPLAY_NAME,
    Document.COLUMN_MIME_TYPE,
    Document.COLUMN_SIZE,
    Document.COLUMN_LAST_MODIFIED,
    Document.COLUMN_FLAGS,
)

// What one open mode string asks for. ParcelFileDescriptor documents six
// forms and the provider serves all six; anything else is refused.
private data class OpenMode(
    val read: Boolean,
    val write: Boolean,
    val truncate: Boolean,
    val append: Boolean,
)

private fun openModeOf(mode: String): OpenMode? = when (mode) {
    "r" -> OpenMode(read = true, write = false, truncate = false, append = false)
    "w", "wt" -> OpenMode(read = false, write = true, truncate = true, append = false)
    "wa" -> OpenMode(read = false, write = true, truncate = false, append = true)
    "rw" -> OpenMode(read = true, write = true, truncate = false, append = false)
    "rwt" -> OpenMode(read = true, write = true, truncate = true, append = false)
    else -> null
}
