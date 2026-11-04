package com.lecer.lisapp

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.content.getSystemService

object Notifications {
    const val SERVICE_ID = 1
    private const val SERVICE_CHANNEL = "connection"
    private const val FILES_CHANNEL = "files"

    fun createChannels(context: Context) {
        val manager = context.getSystemService<NotificationManager>() ?: return
        manager.createNotificationChannel(
            NotificationChannel(SERVICE_CHANNEL, "Connection", NotificationManager.IMPORTANCE_LOW),
        )
        manager.createNotificationChannel(
            NotificationChannel(FILES_CHANNEL, "Files", NotificationManager.IMPORTANCE_DEFAULT),
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
        val notification = Notification.Builder(context, FILES_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("Lis App")
            .setContentText(text)
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        context.getSystemService<NotificationManager>()?.notify(FILE_ID_BASE + id, notification)
    }

    private const val FILE_ID_BASE = 1000
}
