package app.ferry.engine

import android.content.Context
import uniffi.ferry_runtime.FerryException
import uniffi.ferry_runtime.KeyPair
import uniffi.ferry_runtime.generateKey
import java.io.File
import java.io.FileOutputStream

// The phone's own device key, generated once and kept on disk after that.
// Split from FerryEngine.kt, docs/audits/principles.md row P9.
//
// FerryEngine.create calls loadOrCreateKey to fill Config.key. Nothing here
// touches engine state; it is a pure Context-to-KeyPair function.

// The 64 byte key file: 32 private bytes, then 32 public bytes.
private const val KEY_FILE_NAME = "device.key"
private const val KEY_PART_BYTES = 32

// Not an engine code: there is no entry for a phone-side rename
// failure in the generated table, so this falls back to the unknown
// code words, which is a fault the person cannot fix by retrying.
private const val KEY_RENAME_FAILED_CODE = "Android::KeyRenameFailed"

// Reads the key from filesDir, or makes one on first run and writes it.
//
// The Android Keystore is not used. It keeps a key inside hardware and
// signs or encrypts on the app's behalf; it never hands back the raw
// bytes. The engine needs the raw 32 private bytes for the Noise
// handshake, so the key has to live where the app can read it. filesDir
// is private to this app, which is the strongest storage that still
// returns bytes.
//
// The bytes are never logged and never shown.
fun loadOrCreateKey(context: Context): KeyPair {
    val file = File(context.filesDir, KEY_FILE_NAME)
    val wholeSize = (KEY_PART_BYTES * 2).toLong()
    if (file.isFile && file.length() == wholeSize) {
        val bytes = file.readBytes()
        return KeyPair(
            `private` = bytes.copyOfRange(0, KEY_PART_BYTES),
            `public` = bytes.copyOfRange(KEY_PART_BYTES, KEY_PART_BYTES * 2),
        )
    }
    val fresh = generateKey()
    val whole = ByteArray(KEY_PART_BYTES * 2)
    fresh.`private`.copyInto(whole, 0)
    fresh.`public`.copyInto(whole, KEY_PART_BYTES)
    // Written to a temporary name and renamed, so a crash part way
    // through leaves the old file whole rather than half a key.
    //
    // The temporary file is synced to disk before the rename. Without
    // that, a power loss right after the rename can leave a zero
    // length file at the final name on ext4 and f2fs, and the length
    // check above then treats it as missing and generates a new key.
    // This is the same fsync-then-rename shape record.rs's
    // write_and_sync uses on the engine side.
    val temporary = File(context.filesDir, "$KEY_FILE_NAME.new")
    FileOutputStream(temporary).use { out ->
        out.write(whole)
        out.fd.sync()
    }
    if (!temporary.renameTo(file)) {
        // A failed rename here means the next launch finds no
        // device.key, generates another, and every paired Mac stops
        // recognising this phone. create()'s catch reports this the
        // same way it reports any other failure to build the engine.
        throw FerryException.Failed(KEY_RENAME_FAILED_CODE, null)
    }
    return fresh
}
