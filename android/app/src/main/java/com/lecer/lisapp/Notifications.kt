package com.lecer.lisapp

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.media.session.MediaSession
import androidx.core.content.getSystemService
import uniffi.lis_ffi.MediaInfo

object Notifications {
    const val SERVICE_ID = 1
    private const val SERVICE_CHANNEL = "connection"
    private const val FILES_CHANNEL = "files"
    private const val MEDIA_CHANNEL = "media"
    private const val RING_CHANNEL = "ring"
    private const val LOCK_CHANNEL = "pc_lock"
    private const val PROMPT_CHANNEL = "pc_unlock_prompt"
    private const val MEDIA_ID = 2
    private const val RING_ID = 3
    private const val LOCK_ID = 4
    private const val PROMPT_ID = 5
    private const val PROMPT_SHOWS_MS = 30_000L

    fun createChannels(context: Context) {
        val manager = context.getSystemService<NotificationManager>() ?: return
        manager.createNotificationChannel(
            NotificationChannel(SERVICE_CHANNEL, "Connection", NotificationManager.IMPORTANCE_LOW),
        )
        manager.createNotificationChannel(
            NotificationChannel(FILES_CHANNEL, "Files", NotificationManager.IMPORTANCE_DEFAULT),
        )
        manager.createNotificationChannel(
            NotificationChannel(MEDIA_CHANNEL, "PC media", NotificationManager.IMPORTANCE_LOW),
        )
        // The ringing has its own sound
        manager.createNotificationChannel(
            NotificationChannel(RING_CHANNEL, "Find my phone", NotificationManager.IMPORTANCE_HIGH).apply {
                setSound(null, null)
                enableVibration(false)
            },
        )
        manager.createNotificationChannel(
            NotificationChannel(LOCK_CHANNEL, "PC locked", NotificationManager.IMPORTANCE_LOW),
        )
        // High, or it can't open by itself or show as a banner
        manager.createNotificationChannel(
            NotificationChannel(PROMPT_CHANNEL, "Unlock prompt", NotificationManager.IMPORTANCE_HIGH).apply {
                setSound(null, null)
            },
        )
    }

    private fun openApp(context: Context): PendingIntent = PendingIntent.getActivity(
        context,
        0,
        Intent(context, MainActivity::class.java),
        PendingIntent.FLAG_IMMUTABLE,
    )

    fun service(context: Context, text: String): Notification =
        Notification.Builder(context, SERVICE_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("Lis App")
            .setContentText(text)
            .setContentIntent(openApp(context))
            .setOngoing(true)
            .build()

    fun showService(context: Context, text: String) {
        context.getSystemService<NotificationManager>()?.notify(SERVICE_ID, service(context, text))
    }

    fun showFile(context: Context, id: Int, text: String) {
        if (!Prefs.current.notifyFiles) return
        val notification = Notification.Builder(context, FILES_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("Lis App")
            .setContentText(text)
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        context.getSystemService<NotificationManager>()?.notify(FILE_ID_BASE + id, notification)
    }

    fun showMedia(context: Context, token: MediaSession.Token, info: MediaInfo, art: Bitmap?) {
        val notification = Notification.Builder(context, MEDIA_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(info.title)
            .setContentText(info.artist)
            .setLargeIcon(art)
            .setContentIntent(openApp(context))
            .setStyle(Notification.MediaStyle().setMediaSession(token))
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setOngoing(info.playing)
            .setOnlyAlertOnce(true)
            .build()
        context.getSystemService<NotificationManager>()?.notify(MEDIA_ID, notification)
    }

    fun cancelMedia(context: Context) {
        context.getSystemService<NotificationManager>()?.cancel(MEDIA_ID)
    }

    fun showRinging(context: Context) {
        val stop = PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, StopRingReceiver::class.java),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val action = Notification.Action.Builder(null, "Stop", stop).build()
        val notification = Notification.Builder(context, RING_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("Your PC is looking for this phone")
            .setContentText("Tap Stop to stop the ringing")
            .setContentIntent(stop)
            .addAction(action)
            .setCategory(Notification.CATEGORY_ALARM)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setOngoing(true)
            .build()
        context.getSystemService<NotificationManager>()?.notify(RING_ID, notification)
    }

    fun cancelRinging(context: Context) {
        context.getSystemService<NotificationManager>()?.cancel(RING_ID)
    }

    private fun openPrompt(context: Context, request: Int): PendingIntent = PendingIntent.getActivity(
        context,
        request,
        UnlockActivity.intent(context),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    fun showPcLocked(context: Context, pcName: String) {
        if (!Prefs.current.notifyPcLocked) return
        val unlock = openPrompt(context, 1)
        val notification = Notification.Builder(context, LOCK_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("$pcName is locked")
            .setContentText("Tap to unlock it with your fingerprint")
            .setContentIntent(unlock)
            .setAutoCancel(true)
            .build()
        context.getSystemService<NotificationManager>()?.notify(LOCK_ID, notification)
    }

    fun cancelPcLocked(context: Context) {
        context.getSystemService<NotificationManager>()?.cancel(LOCK_ID)
    }

    fun showUnlockPrompt(context: Context, pcName: String) {
        val prompt = openPrompt(context, 2)
        val notification = Notification.Builder(context, PROMPT_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("Unlock $pcName")
            .setContentText("Tap to use your fingerprint")
            .setContentIntent(prompt)
            .setFullScreenIntent(prompt, true)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setAutoCancel(true)
            .setTimeoutAfter(PROMPT_SHOWS_MS)
            .build()
        context.getSystemService<NotificationManager>()?.notify(PROMPT_ID, notification)
    }

    fun cancelUnlockPrompt(context: Context) {
        context.getSystemService<NotificationManager>()?.cancel(PROMPT_ID)
    }

    private const val FILE_ID_BASE = 1000
}
