package com.lecer.lisapp

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.util.Log
import java.util.concurrent.ConcurrentHashMap

/** Files both ways. Rust reads and writes them through descriptors handed over from here. */
object Transfers {
    private const val TAG = "LisFiles"
    private val incoming = ConcurrentHashMap<ULong, Uri>()

    fun send(context: Context, uri: Uri) {
        try {
            var name = uri.lastPathSegment?.substringAfterLast('/') ?: "file"
            var size = 0L
            context.contentResolver.query(
                uri,
                arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE),
                null,
                null,
                null,
            )?.use { cursor ->
                if (cursor.moveToFirst()) {
                    cursor.getString(0)?.let { name = it }
                    if (!cursor.isNull(1)) size = cursor.getLong(1)
                }
            }
            val descriptor = context.contentResolver.openFileDescriptor(uri, "r")
                ?: throw IllegalStateException("no descriptor")
            Lis.client.sendFile(descriptor.detachFd(), name, size.toULong())
            Lis.update { copy(transfer = "Sending $name") }
        } catch (error: Exception) {
            Log.w(TAG, "can't send a file", error)
            Lis.update { copy(transfer = "Couldn't send that file: ${error.message}") }
        }
    }

    /** Pending until it's complete, so a partial file never shows up in Downloads. */
    fun openIncoming(context: Context, id: ULong, name: String): Int {
        return try {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val uri = context.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: return -1
            val descriptor = context.contentResolver.openFileDescriptor(uri, "w")
            if (descriptor == null) {
                context.contentResolver.delete(uri, null, null)
                return -1
            }
            incoming[id] = uri
            descriptor.detachFd()
        } catch (error: Exception) {
            Log.w(TAG, "can't save an incoming file", error)
            -1
        }
    }

    fun finish(context: Context, id: ULong, name: String, isIncoming: Boolean, ok: Boolean) {
        if (isIncoming) {
            incoming.remove(id)?.let { uri ->
                try {
                    if (ok) {
                        val values = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
                        context.contentResolver.update(uri, values, null, null)
                    } else {
                        context.contentResolver.delete(uri, null, null)
                    }
                } catch (error: Exception) {
                    Log.w(TAG, "can't finish an incoming file", error)
                }
            }
        }
        val text = when {
            !ok -> "Couldn't transfer $name"
            isIncoming -> "Saved $name to Downloads"
            else -> "Sent $name"
        }
        Lis.update { copy(transfer = text) }
        Notifications.showFile(context, id.toInt(), text)
    }
}
