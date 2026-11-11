package com.lecer.lisapp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** The Stop button on the ringing notification. */
class StopRingReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        Ringer.stop(context)
    }
}

class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val wanted = Prefs.current.startOnBoot && Prefs.current.keepConnected
        if (intent.action == Intent.ACTION_BOOT_COMPLETED && wanted && Lis.client.pairedPc() != null) {
            SyncService.start(context)
        }
    }
}
