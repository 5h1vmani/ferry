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
import uniffi.ferry_runtime.Config
import uniffi.ferry_runtime.DeviceInfo
import uniffi.ferry_runtime.Engine
import uniffi.ferry_runtime.EngineListener
import uniffi.ferry_runtime.FerryException
import uniffi.ferry_runtime.KeyPair
import uniffi.ferry_runtime.PairingState
import uniffi.ferry_runtime.TransferInfo
import uniffi.ferry_runtime.generateKey
import uniffi.ferry_runtime.phonePort
import java.io.File

// The one engine this process owns, and the state the screens read.
//
// The engine reports change through a listener, and the listener carries
// little or nothing. So each callback asks the engine again with devices()
// or transfers() and puts the answer in a flow. Both reads are local and
// instant, which the crate documentation states, so doing them on the
// engine's own thread costs nothing.
//
// Every callback arrives on an engine thread, never on the main thread.
// MutableStateFlow accepts a write from any thread, so no hop is needed
// here. Compose collects the flows with collectAsState().
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

    private val _devices = MutableStateFlow<List<DeviceInfo>>(emptyList())

    // Every paired Mac, as the engine reports it.
    val devices: StateFlow<List<DeviceInfo>> = _devices.asStateFlow()

    private val _transfers = MutableStateFlow<List<TransferInfo>>(emptyList())

    // Every transfer, as the engine reports it.
    val transfers: StateFlow<List<TransferInfo>> = _transfers.asStateFlow()

    private val _pairing = MutableStateFlow<PairingState>(PairingState.Idle)

    // Where pairing is. Idle until startPairing() is called.
    val pairing: StateFlow<PairingState> = _pairing.asStateFlow()

    private val _shortCode = MutableStateFlow<String?>(null)

    // The last four characters of this phone's own mDNS name. Null until
    // the phone is reachable.
    val shortCode: StateFlow<String?> = _shortCode.asStateFlow()

    private val _reachable = MutableStateFlow(false)

    // True while the phone advertises and accepts connections.
    val reachable: StateFlow<Boolean> = _reachable.asStateFlow()

    private val _started = MutableStateFlow(false)

    // True once start() has succeeded.
    val started: StateFlow<Boolean> = _started.asStateFlow()

    private val _error = MutableStateFlow<FerryException?>(null)

    // The last error the engine returned: building it, starting it, or
    // running forget or retry. Null when the last such call succeeded.
    val error: StateFlow<FerryException?> = _error.asStateFlow()

    private var engine: Engine? = null

    // forget and retry block on the network or the disk, so they run here.
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val listener = object : EngineListener {
        override fun devicesChanged() {
            engine?.let { _devices.value = it.devices() }
        }

        override fun transfersChanged() {
            engine?.let { _transfers.value = it.transfers() }
        }

        override fun pairingChanged(state: PairingState) {
            _pairing.value = state
            engine?.let { _shortCode.value = it.shortCode() }
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
            val config = Config(
                dataDir = dataDir.absolutePath,
                sharedRoot = Environment.getExternalStorageDirectory().absolutePath,
                displayName = Build.MODEL,
                // The port the Mac reaches through an adb forward. It comes from
                // the engine, so the number lives once.
                listenPort = phonePort(),
                key = loadOrCreateKey(context),
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
        _devices.value = emptyList()
        _transfers.value = emptyList()
    }

    // Advertises over mDNS and accepts connections, or stops doing both.
    // ReachableService calls this, so the switch and the notification always
    // say the same thing.
    fun setReachable(on: Boolean) {
        val current = engine ?: return
        current.setReachable(on)
        _reachable.value = on
        _shortCode.value = current.shortCode()
    }

    // Enters pairing. The phone waits for one Mac and reports the code
    // through the listener.
    fun startPairing() {
        _error.value = null
        engine?.startPairing()
    }

    // Accepts or rejects the Mac whose code is showing.
    fun confirmPairing(accept: Boolean) {
        engine?.confirmPairing(accept)
    }

    // Stops pairing and drops whatever it was holding.
    fun cancelPairing() {
        engine?.cancelPairing()
        _pairing.value = PairingState.Idle
    }

    // Removes a Mac's key and every transfer record for it. This writes the
    // device list to disk, so it runs off the main thread.
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
        temporary.renameTo(file)
        return fresh
    }
}
