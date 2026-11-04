package com.lecer.lisapp

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.util.Log
import androidx.core.content.getSystemService
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors

/** Finds PCs on the network. They advertise the start of their fingerprint, which is checked again on connecting. */
class Discovery(context: Context, private val onChange: (List<FoundPc>) -> Unit) {
    private val manager = context.getSystemService<NsdManager>()
    private val found = ConcurrentHashMap<String, FoundPc>()
    private val executor = Executors.newSingleThreadExecutor()
    private var listener: NsdManager.DiscoveryListener? = null

    @Synchronized
    fun start() {
        if (listener != null || manager == null) return
        val created = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) {}
            override fun onDiscoveryStopped(serviceType: String) {}

            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.w(TAG, "discovery failed to start: $errorCode")
                synchronized(this@Discovery) { listener = null }
            }

            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.w(TAG, "discovery failed to stop: $errorCode")
            }

            override fun onServiceFound(info: NsdServiceInfo) = resolve(info)

            override fun onServiceLost(info: NsdServiceInfo) {
                found.remove(info.serviceName)
                publish()
            }
        }
        listener = created
        manager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, created)
    }

    @Synchronized
    fun stop() {
        listener?.let { current ->
            runCatching { manager?.stopServiceDiscovery(current) }
        }
        listener = null
    }

    fun restart() {
        stop()
        found.clear()
        publish()
        start()
    }

    @Suppress("DEPRECATION")
    private fun resolve(info: NsdServiceInfo) {
        // One resolve at a time, which is all older Android versions allow
        executor.execute {
            val done = java.util.concurrent.CountDownLatch(1)
            manager?.resolveService(info, object : NsdManager.ResolveListener {
                override fun onResolveFailed(info: NsdServiceInfo, errorCode: Int) {
                    Log.w(TAG, "can't resolve a PC: $errorCode")
                    done.countDown()
                }

                override fun onServiceResolved(info: NsdServiceInfo) {
                    add(info)
                    done.countDown()
                }
            })
            done.await(RESOLVE_TIMEOUT_SECONDS, java.util.concurrent.TimeUnit.SECONDS)
        }
    }

    @Suppress("DEPRECATION")
    private fun add(info: NsdServiceInfo) {
        val address = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            info.hostAddresses.firstOrNull { it is java.net.Inet4Address } ?: info.hostAddresses.firstOrNull()
        } else {
            info.host
        }
        val host = address?.hostAddress ?: return
        val fingerprintStart = info.attributes["fp"]?.toString(Charsets.UTF_8) ?: return
        val name = info.attributes["name"]?.toString(Charsets.UTF_8) ?: info.serviceName
        found[info.serviceName] = FoundPc(name, host, info.port, fingerprintStart)
        publish()
    }

    private fun publish() = onChange(found.values.sortedBy { it.name })

    private companion object {
        const val TAG = "LisDiscovery"
        const val SERVICE_TYPE = "_lisapp._tcp."
        const val RESOLVE_TIMEOUT_SECONDS = 5L
    }
}
