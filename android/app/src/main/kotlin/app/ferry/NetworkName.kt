package app.ferry

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiInfo
import androidx.core.content.ContextCompat
import app.ferry.engine.FerryEngine

// Reads the phone's Wi-Fi network name and hands it to the engine.
// docs/engine-contract.md, item 18, "The phone".
//
// The engine decides what to do with the name; this object only reads it.
// ConnectivityManager reports it through a NetworkCallback registered for
// the Wi-Fi transport, never by polling — polling would mean the phone is
// never quiet, and this feature exists to let it be quiet.
//
// A callback made before the location permission is granted never carries
// the name: the quotes come back stripped from `<unknown ssid>`, which
// looks the same as Wi-Fi being off. So refresh() re-registers the
// callback on the refused-to-granted edge, in addition to the one
// registration start() makes at process start.
object NetworkName {
    // What WifiInfo.getSSID() returns when the caller cannot see the real
    // name: no location permission, or location off.
    private const val UNKNOWN_SSID = "<unknown ssid>"

    private lateinit var connectivityManager: ConnectivityManager
    private var callback: ConnectivityManager.NetworkCallback? = null
    private var lastLocationGranted = false

    // The last name this object read, kept even while no engine exists yet
    // to hand it to. FerryEngine.start() reads this once its engine is
    // ready, so a callback that fired before create() or before start() is
    // not lost.
    @Volatile
    private var lastKnown: String? = null

    // The Network that lastKnown came from. Android hides the SSID from a
    // background app unless it holds background location. The unknown
    // placeholder can arrive for a network that never changed. So a
    // placeholder for the same Network handle is ignored, not reported as
    // null.
    @Volatile
    private var lastNetwork: Network? = null

    // Called once, from FerryApplication.onCreate.
    fun start(context: Context) {
        val appContext = context.applicationContext
        val manager = appContext.getSystemService(ConnectivityManager::class.java) ?: return
        connectivityManager = manager
        lastLocationGranted = hasLocationPermission(appContext)
        register()
    }

    // Called on every resume, after Permissions.refresh(context). Only the
    // refused-to-granted edge does anything: an already-registered
    // callback that already carries the name has nothing to gain from
    // registering again.
    fun refresh(context: Context) {
        if (!::connectivityManager.isInitialized) {
            // start() found no ConnectivityManager and returned early.
            // There is nothing to re-register.
            return
        }
        val granted = hasLocationPermission(context)
        if (granted && !lastLocationGranted) {
            register()
        }
        lastLocationGranted = granted
    }

    // The name FerryEngine.start() pushes once the engine exists, so a
    // reading made before the engine was ready still reaches it.
    fun current(): String? = lastKnown

    private fun register() {
        callback?.let { connectivityManager.unregisterNetworkCallback(it) }
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .build()
        val newCallback = object : ConnectivityManager.NetworkCallback(
            ConnectivityManager.NetworkCallback.FLAG_INCLUDE_LOCATION_INFO,
        ) {
            override fun onCapabilitiesChanged(
                network: Network,
                networkCapabilities: NetworkCapabilities,
            ) {
                val name = nameFrom(networkCapabilities)
                if (name != null) {
                    setNetwork(name, network)
                } else if (network != lastNetwork) {
                    // A different Network than the one lastKnown came from,
                    // reporting a name we cannot read yet. Track its handle
                    // so a repeat of this placeholder is recognised as the
                    // same network, but wait for a real name, or onLost,
                    // before telling the engine anything.
                    lastNetwork = network
                }
                // else: the same Network as lastKnown, now hidden behind the
                // unknown placeholder. Keep the last known name and say
                // nothing to the engine.
            }

            override fun onLost(network: Network) {
                setNetwork(null, null)
            }
        }
        callback = newCallback
        connectivityManager.registerNetworkCallback(request, newCallback)
    }

    private fun setNetwork(name: String?, network: Network?) {
        lastKnown = name
        lastNetwork = network
        FerryEngine.setNetwork(name)
    }

    // The quotes WifiInfo.getSSID() wraps the name in, stripped. The
    // unknown placeholder and a network with no SSID at all both become
    // null, which is what "cannot read the name" is on the wire.
    private fun nameFrom(capabilities: NetworkCapabilities): String? {
        val wifiInfo = capabilities.transportInfo as? WifiInfo ?: return null
        val ssid = wifiInfo.ssid
        if (ssid.isNullOrEmpty() || ssid == UNKNOWN_SSID) {
            return null
        }
        return ssid.removeSurrounding("\"")
    }

    private fun hasLocationPermission(context: Context): Boolean =
        ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.ACCESS_FINE_LOCATION,
        ) == PackageManager.PERMISSION_GRANTED
}
