package com.lecer.lisapp

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.os.IBinder
import android.util.Log
import androidx.core.content.ContextCompat
import androidx.core.content.getSystemService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
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
            Lis.client.reconnectNow()
            discovery.restart()
            StatsReporter.sendNow(this@SyncService)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        discovery = Discovery(this) { found -> onFound(found) }
        getSystemService<ConnectivityManager>()?.registerDefaultNetworkCallback(networkCallback)

        scope.launch {
            Lis.state.map { it.connection to it.pcName }.distinctUntilChanged().collect { (connection, name) ->
                Notifications.showService(this@SyncService, statusText(connection, name))
                if (connection == ConnectionState.CONNECTED) discovery.stop() else discovery.start()
            }
        }
        scope.launch {
            while (true) {
                delay(STATS_EVERY_MS)
                StatsReporter.sendNow(this@SyncService)
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val state = Lis.state.value
        try {
            startForeground(
                Notifications.SERVICE_ID,
                Notifications.service(this, statusText(state.connection, state.pcName)),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
            )
        } catch (error: RuntimeException) {
            // Android refuses this from some background starts; it comes back with the app
            Log.w(TAG, "can't run in the foreground now", error)
            stopSelf()
            return START_NOT_STICKY
        }
        Lis.client.pairedPc()?.let { pc -> Lis.client.connect(pc.host, pc.port) }
        return START_STICKY
    }

    private fun onFound(found: List<FoundPc>) {
        Lis.update { copy(found = found) }
        val paired = Lis.client.pairedPc() ?: return
        if (Lis.state.value.connection == ConnectionState.CONNECTED) return
        // The PC's address can change; its certificate doesn't
        found.firstOrNull { paired.fingerprint.startsWith(it.fingerprintStart) }?.let { pc ->
            Lis.client.connect(pc.host, pc.port.toUShort())
        }
    }

    override fun onDestroy() {
        scope.cancel()
        discovery.stop()
        getSystemService<ConnectivityManager>()?.unregisterNetworkCallback(networkCallback)
        super.onDestroy()
    }

    companion object {
        private const val TAG = "LisSync"
        private const val STATS_EVERY_MS = 60_000L

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

        private fun statusText(connection: ConnectionState, name: String?): String = when (connection) {
            ConnectionState.CONNECTED -> "Connected to ${name ?: "your PC"}"
            ConnectionState.CONNECTING -> "Connecting…"
            ConnectionState.UNPAIRED -> "Not paired"
            ConnectionState.WRONG_PC -> "Another device answered instead of your PC"
            ConnectionState.DISCONNECTED -> "Looking for your PC…"
        }
    }
}
