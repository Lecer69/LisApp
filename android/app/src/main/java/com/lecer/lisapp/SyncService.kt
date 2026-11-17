package com.lecer.lisapp

import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.IBinder
import android.util.Log
import androidx.core.content.ContextCompat
import androidx.core.content.getSystemService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import uniffi.lis_ffi.ConnectionState

/** Keeps the connection up: finds the PC, follows network changes and reports the phone's figures. */
class SyncService : Service() {
    private val scope = CoroutineScope(Dispatchers.Default + SupervisorJob())
    private lateinit var discovery: Discovery

    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            if (Lis.state.value.paused) return
            Lis.client.reconnectNow()
            discovery.restart()
            StatsReporter.sendNow(this@SyncService)
        }
    }

    /** Any Wi-Fi, with or without internet, as the PC is usually on the same one. */
    private val wifiNetworks = mutableSetOf<Network>()
    private val wifiCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = changeWifi { add(network) }
        override fun onLost(network: Network) = changeWifi { remove(network) }
    }

    private fun changeWifi(change: MutableSet<Network>.() -> Unit) {
        val on = synchronized(wifiNetworks) {
            wifiNetworks.change()
            wifiNetworks.isNotEmpty()
        }
        Lis.setWifi(on)
    }

    /** The PC hears about the charger at once, rather than at the next round of figures. */
    private val chargingReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (!Prefs.current.reportCharging) return
            // The battery's own status can take a moment to follow
            scope.launch {
                StatsReporter.sendNow(this@SyncService)
                delay(CHARGE_SETTLES_MS)
                StatsReporter.sendNow(this@SyncService)
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        discovery = Discovery(this) { found -> onFound(found) }
        getSystemService<ConnectivityManager>()?.let { manager ->
            manager.registerDefaultNetworkCallback(networkCallback)
            val active = manager.getNetworkCapabilities(manager.activeNetwork)
            Lis.setWifi(active?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true)
            val wifi = NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .build()
            manager.registerNetworkCallback(wifi, wifiCallback)
        }
        val power = IntentFilter().apply {
            addAction(Intent.ACTION_POWER_CONNECTED)
            addAction(Intent.ACTION_POWER_DISCONNECTED)
        }
        ContextCompat.registerReceiver(this, chargingReceiver, power, ContextCompat.RECEIVER_NOT_EXPORTED)

        scope.launch {
            Lis.state.map { Triple(it.connection, it.pcName, it.paused) }.distinctUntilChanged().collect { (connection, name, paused) ->
                Notifications.showService(this@SyncService, statusText(connection, name, paused))
                if (connection == ConnectionState.CONNECTED || paused) discovery.stop() else discovery.start()
            }
        }
        scope.launch {
            Prefs.settings.map { it.pauseWithoutWifi }.distinctUntilChanged().collect { Lis.refreshPause() }
        }
        scope.launch {
            Prefs.settings.map { it.statsSeconds }.distinctUntilChanged().collectLatest { seconds ->
                while (true) {
                    delay(seconds * 1000L)
                    StatsReporter.sendNow(this@SyncService)
                }
            }
        }
        // Nothing announces a cable being plugged in, and backing off would make it slow to notice
        scope.launch {
            while (true) {
                delay(Prefs.current.usbRetrySeconds * 1000L)
                val state = Lis.state.value
                if (state.usbMode && state.connection != ConnectionState.CONNECTED) Lis.client.reconnectNow()
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val state = Lis.state.value
        try {
            startForeground(
                Notifications.SERVICE_ID,
                Notifications.service(this, statusText(state.connection, state.pcName, state.paused)),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
            )
        } catch (error: RuntimeException) {
            // Android refuses this from some background starts; it comes back with the app
            Log.w(TAG, "can't run in the foreground now", error)
            stopSelf()
            return START_NOT_STICKY
        }
        Lis.connectToPc()
        return START_STICKY
    }

    private fun onFound(found: List<FoundPc>) {
        Lis.update { copy(found = found) }
        val paired = Lis.client.pairedPc() ?: return
        if (Lis.state.value.connection == ConnectionState.CONNECTED || Lis.state.value.usbMode) return
        // The PC's address can change; its certificate doesn't
        found.firstOrNull { paired.fingerprint.startsWith(it.fingerprintStart) }?.let { pc ->
            Lis.client.connect(pc.host, pc.port.toUShort())
        }
    }

    override fun onDestroy() {
        scope.cancel()
        discovery.stop()
        unregisterReceiver(chargingReceiver)
        getSystemService<ConnectivityManager>()?.let { manager ->
            manager.unregisterNetworkCallback(networkCallback)
            manager.unregisterNetworkCallback(wifiCallback)
        }
        super.onDestroy()
    }

    companion object {
        private const val TAG = "LisSync"
        private const val CHARGE_SETTLES_MS = 3_000L

        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, SyncService::class.java))
            } catch (error: RuntimeException) {
                Log.w(TAG, "can't start the service now", error)
            }
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, SyncService::class.java))
        }

        private fun statusText(connection: ConnectionState, name: String?, paused: Boolean): String = when {
            paused && connection != ConnectionState.CONNECTED -> "Paused while Wi-Fi is off"
            else -> statusText(connection, name)
        }

        private fun statusText(connection: ConnectionState, name: String?): String = when (connection) {
            ConnectionState.CONNECTED -> "Connected to ${name ?: "your PC"}"
            ConnectionState.CONNECTING -> "Connecting…"
            ConnectionState.UNPAIRED -> "Not paired"
            ConnectionState.WRONG_PC -> "Another device answered instead of your PC"
            ConnectionState.DISCONNECTED -> "Looking for your PC…"
        }
    }
}
