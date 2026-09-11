package app.ferry.engine

import android.content.Context
import android.os.Build
import android.os.Environment
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.ferry_runtime.AccessEntry
import uniffi.ferry_runtime.BatchInfo
import uniffi.ferry_runtime.Config
import uniffi.ferry_runtime.DeviceInfo
import uniffi.ferry_runtime.DeviceKind
import uniffi.ferry_runtime.Engine
import uniffi.ferry_runtime.EngineListener
import uniffi.ferry_runtime.FerryException
import uniffi.ferry_runtime.KeyPair
import uniffi.ferry_runtime.PairingState
import uniffi.ferry_runtime.Root
import uniffi.ferry_runtime.TransferInfo
import uniffi.ferry_runtime.generateKey
import uniffi.ferry_runtime.phonePort
import java.io.File
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
        }
    }

    // Opens the shared root and starts every loop. Returns true when the
    // engine is running.
    //
    // The engine's start() opens the shared root, so it fails while all
    // files access is missing. The caller checks the grant first and calls
    // this again on every resume until it succeeds.
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
            true
        } catch (e: FerryException) {
            _error.value = e
            false
        }
    }

    // Stops everything and joins every loop. Safe to call twice.
    //
    // The engine lives as long as the process. This is called only when the
    // reachable service is not running and the activity is finishing, so
    // the engine is never left started with nothing on screen and no
    // service.
    @Synchronized
    fun stop() {
        engine?.stop()
        // The reference is dropped so a later create() builds a fresh
        // engine. The crate documentation states that after stop returns
        // the data directory is free for another engine, and it does not
        // say a stopped engine can be started again.
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
    // listen port, and whether adb was found, so no screen holds a copy of
    // any of them.
    private fun readStatus(current: Engine) {
        _reachable.value = current.status().reachable
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
    // A scan ends in Confirmed or Failed. The phone never shows Requested:
    // scanning the Mac's screen is this phone's half of the trust, so it
    // asks no question of its own.
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

    // Accepts or rejects the device whose code is showing. Code method
    // only: a scan has nothing to compare.
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
        val temporary = File(context.filesDir, "$KEY_FILE_NAME.new")
        temporary.writeBytes(whole)
        if (!temporary.renameTo(file)) {
            // A failed rename here means the next launch finds no
            // device.key, generates another, and every paired Mac stops
            // recognising this phone. create()'s catch reports this the
            // same way it reports any other failure to build the engine.
            throw FerryException.Failed(KEY_RENAME_FAILED_CODE, null)
        }
        return fresh
    }
}
