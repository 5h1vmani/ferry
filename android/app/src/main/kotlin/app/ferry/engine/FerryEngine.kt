package app.ferry.engine

import android.content.Context
import android.os.Build
import android.os.Environment
import android.util.Log
import android.provider.DocumentsContract
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import app.ferry.NetworkName
import app.ferry.ShareIntake
import uniffi.ferry_runtime.AccessEntry
import uniffi.ferry_runtime.BatchInfo
import uniffi.ferry_runtime.Config
import uniffi.ferry_runtime.DeviceInfo
import uniffi.ferry_runtime.DeviceKind
import uniffi.ferry_runtime.Engine
import uniffi.ferry_runtime.EngineListener
import uniffi.ferry_runtime.Entry
import uniffi.ferry_runtime.FerryException
import uniffi.ferry_runtime.KeyPair
import uniffi.ferry_runtime.PairingState
import uniffi.ferry_runtime.Root
import uniffi.ferry_runtime.TransferInfo
import uniffi.ferry_runtime.generateKey
import uniffi.ferry_runtime.phonePort
import java.io.File
import java.io.FileOutputStream
import uniffi.ferry_runtime.PairingMethod as EnginePairingMethod
import app.ferry.model.PairingMethod as UiPairingMethod

// The one engine this process owns, and the state the screens read.
//
// The engine reports change through a listener, and the listener carries
// little or nothing. So each callback asks the engine again with devices(),
// transfers(), batches(), or accessLog() and puts the answer in a flow.
// Those reads are local and instant, which the crate documentation states,
// so doing them on the engine's own thread costs nothing.
//
// Every callback arrives on an engine thread, never on the main thread.
// MutableStateFlow accepts a write from any thread, so no hop is needed
// here. Compose collects the flows with collectAsState().
//
// Nothing in this object formats a number or holds a sentence. It holds
// engine records; the mapping functions in model/Mapping.kt turn them into
// what a screen draws, and strings.xml holds the words.
//
// The listener below holds the engine and the engine holds the listener.
// That ring means Drop never runs, so the app must call stop(). See
// MainActivity.onDestroy and ReachableService.
object FerryEngine {

    // The engine's own files: the paired device list and transfer records.
    private const val DATA_DIR_NAME = "ferry"

    // The engine writes no log of its own, so the one place a field
    // failure can be read is here, through adb logcat.
    private const val LOG_TAG = "Ferry"

    private fun describe(e: FerryException): String =
        (e as? FerryException.Failed)?.let { "${it.code} ${it.detail ?: ""}" } ?: e.toString()

    // The 64 byte key file: 32 private bytes, then 32 public bytes.
    private const val KEY_FILE_NAME = "device.key"
    private const val KEY_PART_BYTES = 32

    // Not an engine code: there is no entry for a phone-side rename
    // failure in the generated table, so this falls back to the unknown
    // code words, which is a fault the person cannot fix by retrying.
    private const val KEY_RENAME_FAILED_CODE = "Android::KeyRenameFailed"

    // What the peer sees as the first segment of every path it asks for.
    // The phone serves one root, and this is the name a person recognises.
    private const val PHONE_ROOT_NAME = "Internal storage"

    // How many access log entries one screen can hold. The engine caps its
    // own limit at a thousand; a screen that scrolls past a few hundred is
    // not being read, it is being searched, and searching is not job 9.
    private const val ACCESS_LOG_LIMIT = 200u

    private val _devices = MutableStateFlow<List<DeviceInfo>>(emptyList())

    // Every paired device, as the engine reports it.
    val devices: StateFlow<List<DeviceInfo>> = _devices.asStateFlow()

    private val _transfers = MutableStateFlow<List<TransferInfo>>(emptyList())

    // Every transfer, as the engine reports it.
    val transfers: StateFlow<List<TransferInfo>> = _transfers.asStateFlow()

    private val _batches = MutableStateFlow<List<BatchInfo>>(emptyList())

    // Every batch: one group of transfers made by one folder copy.
    val batches: StateFlow<List<BatchInfo>> = _batches.asStateFlow()

    private val _accessLog = MutableStateFlow<List<AccessEntry>>(emptyList())

    // What either device did to the other's files, newest first. L5, job 9.
    val accessLog: StateFlow<List<AccessEntry>> = _accessLog.asStateFlow()

    private val _pairing = MutableStateFlow<PairingState>(PairingState.Idle)

    // Where pairing is, as the engine reports it. Idle until a pairing
    // method is chosen.
    val pairing: StateFlow<PairingState> = _pairing.asStateFlow()

    private val _pairingMethod = MutableStateFlow<UiPairingMethod?>(null)

    // Which way in a person chose, or null before they have chosen. The
    // engine does not report this back, so it is held here — the one place
    // it lives.
    val pairingMethod: StateFlow<UiPairingMethod?> = _pairingMethod.asStateFlow()

    private val _scanSent = MutableStateFlow(false)

    // True once a scanned code has been handed to the engine. The engine
    // answers a scan with Confirmed or Failed and reports no state in
    // between, so this is what tells the screen to stop showing a camera.
    val scanSent: StateFlow<Boolean> = _scanSent.asStateFlow()

    private val _shortCode = MutableStateFlow<String?>(null)

    // The last four characters of this phone's own mDNS name. Null until
    // the phone is reachable.
    val shortCode: StateFlow<String?> = _shortCode.asStateFlow()

    private val _reachable = MutableStateFlow(false)

    // True while the phone advertises and accepts connections. Read from
    // the engine's own status(), never cached from what was last set.
    val reachable: StateFlow<Boolean> = _reachable.asStateFlow()

    private val _network = MutableStateFlow<String?>(null)

    // The Wi-Fi network name NetworkName last set. Null when unknown: Wi-Fi
    // off, location refused, or the name unreadable. Read from status(),
    // same as reachable.
    val network: StateFlow<String?> = _network.asStateFlow()

    private val _wifiPresence = MutableStateFlow(false)

    // True while this device advertises, browses, and accepts over Wi-Fi.
    // False while reachable is off, and also while it is on but this
    // network is not trusted. docs/engine-contract.md item 18.
    val wifiPresence: StateFlow<Boolean> = _wifiPresence.asStateFlow()

    private val _trustedNetworks = MutableStateFlow<List<String>>(emptyList())

    // Every trusted Wi-Fi network name, oldest first, as the engine reports
    // it.
    val trustedNetworks: StateFlow<List<String>> = _trustedNetworks.asStateFlow()

    private val _started = MutableStateFlow(false)

    // True once start() has succeeded.
    val started: StateFlow<Boolean> = _started.asStateFlow()

    private val _error = MutableStateFlow<FerryException?>(null)

    // The last error the engine returned: building it, starting it, or
    // running forget or retry. Null when the last such call succeeded.
    val error: StateFlow<FerryException?> = _error.asStateFlow()

    // create and stop write this under @Synchronized. Listener callbacks
    // read it on engine threads with no such lock, so @Volatile is what
    // gives those reads a happens-before edge against the last write.
    @Volatile
    private var engine: Engine? = null

    // forget, retry and offerScanned block on the network or the disk, so
    // they run here.
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val listener = object : EngineListener {
        override fun devicesChanged() {
            engine?.let {
                _devices.value = it.devices()
                // A device becoming reachable can change this phone's own
                // status, and status is one call for four facts.
                readStatus(it)
            }
            // A root's summary in the Files app is that device's
            // reachability, so the picker is told whenever it changes.
            notifyRoots()
        }

        override fun transfersChanged() {
            engine?.let {
                _transfers.value = it.transfers()
                // A batch's progress is computed from its transfers, so it
                // changes whenever they do.
                _batches.value = it.batches()
            }
        }

        override fun pairingChanged(state: PairingState) {
            _pairing.value = state
            engine?.let { _shortCode.value = it.shortCode() }
        }

        override fun accessLogChanged() {
            engine?.let { _accessLog.value = it.accessLog(null, ACCESS_LOG_LIMIT) }
        }
    }

    // Builds the engine. Does nothing on the second call, because one engine
    // at a time may use a data directory and the second is refused.
    //
    // This runs whether or not all files access is granted. start() is the
    // call that opens the shared root, so it is the one that has to wait.
    @Synchronized
    fun create(context: Context) {
        // Kept for notifyRoots, which runs on an engine thread and has no
        // context of its own. The application context outlives every
        // activity, so holding it leaks nothing.
        appContext = context.applicationContext
        if (engine != null) {
            return
        }
        val dataDir = File(context.filesDir, DATA_DIR_NAME)
        dataDir.mkdirs()
        try {
            val externalStorage = Environment.getExternalStorageDirectory()
            val config = Config(
                dataDir = dataDir.absolutePath,
                sharedRoots = listOf(
                    Root(
                        name = PHONE_ROOT_NAME,
                        path = externalStorage.absolutePath,
                        writable = true,
                    ),
                ),
                downloadDir = File(externalStorage, "Download").absolutePath,
                displayName = Build.MODEL,
                // The port the Mac reaches through an adb forward. It comes
                // from the engine, so the number lives once.
                listenPort = phonePort(),
                key = loadOrCreateKey(context),
                kind = DeviceKind.PHONE,
            )
            engine = Engine(config, listener)
            _error.value = null
        } catch (e: FerryException) {
            _error.value = e
            Log.w(LOG_TAG, "engine could not be built: " + describe(e))
        }
    }

    // Opens the shared root and starts every loop. Returns true when the
    // engine is running.
    //
    // The engine's start() opens the shared root, so it fails while all
    // files access is missing. The caller checks the grant first and calls
    // this again on every resume until it succeeds.
    //
    // @Synchronized so a start() racing a stop() on another thread waits
    // for stop()'s own monitor block to finish first. It then finds engine
    // already null and returns false, instead of racing to start an engine
    // stop() is in the middle of tearing down.
    @Synchronized
    fun start(): Boolean {
        if (_started.value) {
            return true
        }
        val current = engine ?: return false
        return try {
            current.start()
            _started.value = true
            _error.value = null
            _devices.value = current.devices()
            _transfers.value = current.transfers()
            _batches.value = current.batches()
            _accessLog.value = current.accessLog(null, ACCESS_LOG_LIMIT)
            readStatus(current)
            // NetworkName may have read a name before this engine was
            // built, or before start() opened it for calls. That reading
            // is not lost: it is held there and pushed again here, which
            // is the "once after start" half of setNetwork's contract.
            setNetwork(NetworkName.current())
            true
        } catch (e: FerryException) {
            _error.value = e
            Log.w(LOG_TAG, "engine could not start: " + describe(e))
            false
        }
    }

    // Stops everything and joins every loop. Safe to call twice.
    //
    // The engine lives as long as the process. This is called only when the
    // reachable service is not running and the activity is finishing, so
    // the engine is never left started with nothing on screen and no
    // service.
    //
    // The engine is taken out and the field nulled under the monitor, and
    // only then is the taken engine's own stop() called, outside it. That
    // call joins every worker thread and can block for several seconds
    // against a peer that stopped answering. Holding the monitor for that
    // whole time would make create() on another thread — MainActivity's
    // onResume, run right after this on a swipe-away-and-reopen — wait out
    // the join before it could see engine as null and build a fresh one.
    // By the time the monitor is released here, the field is already null,
    // so create() and the @Synchronized start() never wait on the slow
    // part.
    fun stop() {
        val current: Engine?
        synchronized(this) {
            current = engine
            // The reference is dropped so a later create() builds a fresh
            // engine. The crate documentation states that after stop
            // returns the data directory is free for another engine, and
            // it does not say a stopped engine can be started again.
            engine = null
            _started.value = false
            _reachable.value = false
            _shortCode.value = null
            _pairing.value = PairingState.Idle
            _pairingMethod.value = null
            _scanSent.value = false
            _devices.value = emptyList()
            _transfers.value = emptyList()
            _batches.value = emptyList()
            _accessLog.value = emptyList()
            _network.value = null
            _wifiPresence.value = false
            _trustedNetworks.value = emptyList()
        }
        current?.stop()
    }

    // Advertises over mDNS and accepts connections, or stops doing both.
    // Job 5's only control. ReachableService calls this, so the switch and
    // the notification always say the same thing.
    fun setReachable(on: Boolean) {
        if (!_started.value) {
            return
        }
        val current = engine ?: return
        current.setReachable(on)
        readStatus(current)
        _shortCode.value = current.shortCode()
    }

    // Reads what this engine currently is. One call for reachability, the
    // network name, Wi-Fi presence, the listen port, and whether adb was
    // found, so no screen holds a copy of any of them. trustedNetworks() is
    // its own call, read here too, because the trusted list changes on the
    // same devicesChanged callback as the rest of this.
    private fun readStatus(current: Engine) {
        val status = current.status()
        _reachable.value = status.reachable
        _network.value = status.network
        _wifiPresence.value = status.wifiPresence
        _trustedNetworks.value = current.trustedNetworks()
    }

    // Hands the engine the phone's current Wi-Fi network name, or null
    // when it cannot be read. NetworkName calls this after start and on
    // every change; idempotent, so a repeat costs a round trip and nothing
    // else. This does not throw, so it needs no error handling of its own,
    // but it runs on Dispatchers.IO with trustNetwork and forgetNetwork so
    // none of the three ever blocks the caller's thread.
    fun setNetwork(name: String?) {
        val current = engine ?: return
        scope.launch {
            current.setNetwork(name)
            readStatus(current)
        }
    }

    // Adds a name to the trusted list. Refused with Runtime::NetworkName
    // for an empty name, a name over 32 bytes, or a 33rd name.
    fun trustNetwork(name: String) {
        val current = engine ?: return
        _error.value = null
        scope.launch {
            try {
                current.trustNetwork(name)
                readStatus(current)
            } catch (e: FerryException) {
                _error.value = e
            }
        }
    }

    // Removes a name from the trusted list. A name that is not trusted is
    // not an error and changes nothing.
    fun forgetNetwork(name: String) {
        val current = engine ?: return
        _error.value = null
        scope.launch {
            try {
                current.forgetNetwork(name)
                readStatus(current)
            } catch (e: FerryException) {
                _error.value = e
            }
        }
    }

    // Records which way in a person chose, without touching the engine.
    // Used when the camera permission has to be asked for first, so a
    // refusal is attributed to the right method and pairingStepOf can show
    // CameraRefused.
    fun setPairingMethod(method: UiPairingMethod) {
        _pairingMethod.value = method
    }

    // Enters pairing by one method, and remembers which. Called when the
    // pairing screen opens and again if a person switches methods.
    //
    // Both methods time out after two minutes and both end at Confirmed or
    // Failed, so nothing downstream of pairing knows which was used.
    fun startPairing(method: UiPairingMethod) {
        val current = engine ?: return
        _error.value = null
        _scanSent.value = false
        _pairingMethod.value = method
        if (method == UiPairingMethod.Scan) {
            // The phone offers the engine nothing yet: it only opens the
            // camera. offerScanned is the call that enters pairing, once a
            // code has been read.
            return
        }
        // start_pairing_with refuses a second call while pairing already
        // runs and reports the same state again, which left "Scan
        // instead" and "Use a code instead" dead once pairing was under
        // way. Cancelling first only when there is something to cancel
        // keeps the first, ordinary call unchanged.
        if (_pairing.value !is PairingState.Idle) {
            current.cancelPairing()
        }
        current.startPairingWith(EnginePairingMethod.CODE)
    }

    // Hands the engine the bytes the camera read. The engine dials the
    // offer's addresses and runs the handshake, so this blocks and runs off
    // the main thread.
    //
    // A successful handshake reports Requested with the Mac's name, not
    // Confirmed straight away: this phone asks its own question before it
    // stores the Mac, the same as the code method does. confirmPairing
    // answers it.
    fun offerScanned(payload: ByteArray) {
        val current = engine ?: return
        if (_scanSent.value) {
            return
        }
        _scanSent.value = true
        _error.value = null
        scope.launch {
            try {
                current.offerScanned(payload)
            } catch (e: FerryException) {
                // _scanSent stays true. Clearing it would reopen the
                // camera on the same code and send it again at once,
                // failing the same way in a loop. The pairing screen
                // shows the error instead and offers the code method.
                _error.value = e
            }
        }
    }

    // Accepts or rejects the device whose name or code is showing: the code
    // method's six digits, or the scan method's Requested, once offerScanned
    // has found the Mac's name.
    fun confirmPairing(accept: Boolean) {
        engine?.confirmPairing(accept)
    }

    // Stops pairing and drops whatever it was holding.
    fun cancelPairing() {
        engine?.cancelPairing()
        _pairing.value = PairingState.Idle
        _pairingMethod.value = null
        _scanSent.value = false
    }

    // Removes a device's key and every transfer record for it. This writes
    // the device list to disk, so it runs off the main thread.
    fun forget(keyHex: String) {
        val current = engine ?: return
        _error.value = null
        scope.launch {
            try {
                current.forget(keyHex)
                _devices.value = current.devices()
            } catch (e: FerryException) {
                _error.value = e
            }
        }
    }

    // Restarts a failed transfer from its resume point. This dials the
    // device, so it runs off the main thread.
    fun retry(transferId: String) {
        val current = engine ?: return
        _error.value = null
        scope.launch {
            try {
                current.retry(transferId)
                _transfers.value = current.transfers()
                _batches.value = current.batches()
            } catch (e: FerryException) {
                _error.value = e
            }
        }
    }

    // Restarts every failed transfer in a batch. One tap for a folder copy
    // where several files failed, instead of one tap per file.
    fun retryBatch(batchId: String) {
        val current = engine ?: return
        _error.value = null
        scope.launch {
            try {
                current.retryBatch(batchId)
                _transfers.value = current.transfers()
                _batches.value = current.batches()
            } catch (e: FerryException) {
                _error.value = e
            }
        }
    }

    // Clears the last error, so a screen does not keep showing an error the
    // person has already acted on.
    fun clearError() {
        _error.value = null
    }

    // ---- Sending files the OS gestures gather: docs/ux-fix-plan.md, item 1 ----

    // The root a gesture-started push lands in when the peer has one named
    // this, ignoring case; otherwise the first root the peer lists.
    // docs/engine-contract.md item 5, "Where a push lands".
    private const val LANDING_ROOT_NAME = "Downloads"
    private const val LANDING_SUBFOLDER = "Ferry"

    // Sends several files into one folder on a paired device, as one batch.
    // Returns the batch id. In the same style as list, stat, and mkdir
    // below: a passthrough that throws the engine's own FerryException.
    fun pushFiles(keyHex: String, localPaths: List<String>, remoteFolder: String): String =
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
    fun pushShared(localPaths: List<String>) {
        val devices = _devices.value
        if (devices.isEmpty()) {
            return
        }
        val device = targetDevice(devices) ?: run {
            // Several are paired and none is reachable: audit finding 6.
            // The same fault a dial would hit anyway, shown at once rather
            // than after picking one of several devices arbitrarily.
            _error.value = FerryException.Failed("Runtime::NotReachable", null)
            return
        }
        val keyHex = device.keyHex
        _error.value = null
        scope.launch {
            try {
                val folder = landingFolder(keyHex)
                try {
                    mkdir(keyHex, folder)
                } catch (e: FerryException) {
                    val code = (e as? FerryException.Failed)?.code
                    // push does not create the parent folder itself
                    // (docs/engine-contract.md item 5), so this call makes
                    // it first. A folder already there is success, not a
                    // fault.
                    if (code != "OpError::AlreadyExists") {
                        throw e
                    }
                }
                val batchId = pushFiles(keyHex, localPaths, folder)
                appContext?.let { context -> ShareIntake.registerBatch(context, batchId, localPaths) }
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
    // root the peer lists, each in a folder named "Ferry".
    // docs/engine-contract.md item 5, "Where a push lands". `list` always
    // returns at least one root for a device that started (item 15), so
    // the fallback below is never expected to run.
    private fun landingFolder(keyHex: String): String {
        val roots = list(keyHex, "")
        val chosen = roots.firstOrNull { it.name.equals(LANDING_ROOT_NAME, ignoreCase = true) }
            ?: roots.firstOrNull()
            ?: throw FerryException.Failed("Runtime::NoCandidate", null)
        return "${chosen.name}/$LANDING_SUBFOLDER"
    }

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
    private fun loadOrCreateKey(context: Context): KeyPair {
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

    // ---- The Mac's folders in the phone's Files app ----
    //
    // docs/engine-contract.md item 19, job 8. FerryDocumentsProvider is the
    // only caller. It runs in this process on binder threads, which may
    // block, and every call below blocks for at least one round trip.
    //
    // Each one is a passthrough. The engine's error is thrown on unchanged,
    // because only the provider knows whether a refusal becomes an errno or
    // a FileNotFoundException. Nothing here decides what a refusal means.

    // The authority the manifest declares for FerryDocumentsProvider. The
    // manifest holds the same text, because XML cannot read Kotlin.
    const val DOCUMENTS_AUTHORITY = "app.ferry.documents"

    // What a call made before the engine exists reports. It is the engine's
    // own code for the same state, so the words are already in the table.
    private const val NOT_STARTED_CODE = "Runtime::NotStarted"

    // Set by create, so notifyRoots has a context on an engine thread.
    @Volatile
    private var appContext: Context? = null

    // Every entry in one folder on a paired device. The empty path names
    // that device's shared roots.
    fun list(keyHex: String, remotePath: String): List<Entry> =
        required().list(keyHex, remotePath)

    // One file or folder on a paired device.
    fun stat(keyHex: String, remotePath: String): Entry =
        required().stat(keyHex, remotePath)

    // At most one mebibyte. A longer ask is clamped by the engine, and a
    // short answer means the end of the file.
    fun readAt(keyHex: String, remotePath: String, offset: ULong, len: UInt): ByteArray =
        required().readAt(keyHex, remotePath, offset, len)

    // Creates the file when it does not exist. More than one mebibyte in
    // one call is refused with Runtime::WriteTooLarge and writes nothing.
    fun writeAt(keyHex: String, remotePath: String, offset: ULong, bytes: ByteArray) =
        required().writeAt(keyHex, remotePath, offset, bytes)

    // Sets a file's length. The provider truncates to zero on a truncating
    // open mode.
    fun truncate(keyHex: String, remotePath: String, len: ULong) =
        required().truncate(keyHex, remotePath, len)

    // Makes one folder. The parent must already exist.
    fun mkdir(keyHex: String, remotePath: String) = required().mkdir(keyHex, remotePath)

    // Deletes one file, or one empty folder.
    fun delete(keyHex: String, remotePath: String) = required().delete(keyHex, remotePath)

    // Moves or renames within one root.
    fun rename(keyHex: String, from: String, to: String) = required().rename(keyHex, from, to)

    // The engine, or the engine's own code for not being there yet. Every
    // call above reaches the network, and there is no network before
    // create has built the engine.
    private fun required(): Engine =
        engine ?: throw FerryException.Failed(NOT_STARTED_CODE, null)

    // Tells the Files app that a root's summary has changed. Runs on an
    // engine thread, from devicesChanged, and does nothing before create.
    private fun notifyRoots() {
        val context = appContext ?: return
        context.contentResolver.notifyChange(
            DocumentsContract.buildRootsUri(DOCUMENTS_AUTHORITY),
            null,
        )
    }
}
