package com.lecer.lisapp

import android.app.ActivityManager
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.BatteryManager
import android.os.Environment
import android.os.StatFs
import androidx.core.content.getSystemService
import uniffi.lis_ffi.ConnectionState
import uniffi.lis_ffi.PhoneStats

object StatsReporter {
    fun sendNow(context: Context) {
        if (Lis.state.value.connection != ConnectionState.CONNECTED) return
        Lis.client.sendStats(read(context))
    }

    private fun read(context: Context): PhoneStats {
        val battery = context.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        val level = battery?.getIntExtra(BatteryManager.EXTRA_LEVEL, 0) ?: 0
        val scale = battery?.getIntExtra(BatteryManager.EXTRA_SCALE, 100)?.takeIf { it > 0 } ?: 100
        val status = battery?.getIntExtra(BatteryManager.EXTRA_STATUS, -1) ?: -1

        val storage = StatFs(Environment.getDataDirectory().path)
        val memory = ActivityManager.MemoryInfo()
        context.getSystemService<ActivityManager>()?.getMemoryInfo(memory)

        return PhoneStats(
            batteryPercent = (level * 100 / scale).toUByte(),
            charging = status == BatteryManager.BATTERY_STATUS_CHARGING || status == BatteryManager.BATTERY_STATUS_FULL,
            storageUsed = (storage.totalBytes - storage.availableBytes).toULong(),
            storageTotal = storage.totalBytes.toULong(),
            memoryUsed = (memory.totalMem - memory.availMem).toULong(),
            memoryTotal = memory.totalMem.toULong(),
            network = network(context),
        )
    }

    private fun network(context: Context): String {
        val manager = context.getSystemService<ConnectivityManager>() ?: return ""
        val capabilities = manager.getNetworkCapabilities(manager.activeNetwork) ?: return "Offline"
        return when {
            capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> "Wi-Fi"
            capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "Mobile"
            capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "Ethernet"
            else -> "Other"
        }
    }
}
