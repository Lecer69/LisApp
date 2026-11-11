package com.lecer.lisapp

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.Ringtone
import android.media.RingtoneManager
import android.os.Handler
import android.os.Looper
import android.os.VibrationAttributes
import android.os.VibrationEffect
import android.os.VibratorManager
import android.util.Log
import androidx.core.content.getSystemService

/**
 * Finds a lost phone. It rings as an alarm, which is what still sounds on silent, at full volume
 * unless set otherwise, and puts the volume back afterwards. It gives up by itself after the time set.
 */
object Ringer {
    private const val TAG = "LisRinger"
    private val VIBRATION = longArrayOf(0, 800, 400)

    private val main = Handler(Looper.getMainLooper())
    private var ringtone: Ringtone? = null
    private var savedVolume: Int? = null
    private var appContext: Context? = null
    private val giveUp = Runnable { appContext?.let(::stop) }

    fun start(context: Context) {
        main.post {
            if (ringtone == null) begin(context.applicationContext)
        }
    }

    fun stop(context: Context) {
        main.post { end(context.applicationContext) }
    }

    private fun alarmSound() =
        RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
            ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION)

    private fun begin(context: Context) {
        appContext = context
        val settings = Prefs.current
        context.getSystemService<AudioManager>()?.takeIf { settings.ringFullVolume }?.let { audio ->
            // Refused in some Do Not Disturb modes, where the alarm's own volume has to do
            runCatching {
                savedVolume = audio.getStreamVolume(AudioManager.STREAM_ALARM)
                audio.setStreamVolume(AudioManager.STREAM_ALARM, audio.getStreamMaxVolume(AudioManager.STREAM_ALARM), 0)
            }.onFailure { Log.w(TAG, "can't raise the volume", it) }
        }
        ringtone = RingtoneManager.getRingtone(context, alarmSound())?.apply {
            audioAttributes = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_ALARM)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build()
            isLooping = true
            volume = 1f
            play()
        }
        if (settings.ringVibrate) {
            context.getSystemService<VibratorManager>()?.defaultVibrator?.vibrate(
                VibrationEffect.createWaveform(VIBRATION, 0),
                VibrationAttributes.createForUsage(VibrationAttributes.USAGE_ALARM),
            )
        }
        Notifications.showRinging(context)
        main.postDelayed(giveUp, settings.ringSeconds * 1000L)
        Lis.update { copy(ringing = true) }
        Lis.client.reportRinging(true)
    }

    private fun end(context: Context) {
        main.removeCallbacks(giveUp)
        val wasRinging = ringtone != null
        ringtone?.stop()
        ringtone = null
        context.getSystemService<VibratorManager>()?.defaultVibrator?.cancel()
        savedVolume?.let { volume ->
            runCatching { context.getSystemService<AudioManager>()?.setStreamVolume(AudioManager.STREAM_ALARM, volume, 0) }
        }
        savedVolume = null
        Notifications.cancelRinging(context)
        Lis.update { copy(ringing = false) }
        if (wasRinging) Lis.client.reportRinging(false)
    }
}
