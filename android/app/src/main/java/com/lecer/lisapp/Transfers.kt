package com.lecer.lisapp

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.util.Log
import java.io.File
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import java.util.concurrent.ConcurrentHashMap

/** Files both ways. Rust reads and writes them through descriptors handed over from here. */
object Transfers {
    private const val TAG = "LisFiles"
    private val incoming = ConcurrentHashMap<ULong, Uri>()
    /** Sizes for the log: incoming by id, outgoing by name, as an outgoing id comes later. */
    private val incomingSizes = ConcurrentHashMap<ULong, Long>()
    private val outgoingSizes = ConcurrentHashMap<String, Long>()

    fun noteIncomingSize(id: ULong, size: Long) {
        incomingSizes[id] = size
    }
    /** Photos on their way, by name, which two taken in the same second can share. */
    private val snaps = mutableListOf<String>()
    private val SNAP_NAME = DateTimeFormatter.ofPattern("yyyy-MM-dd HH-mm-ss")

    /** From a file in the cache that's gone at once, as the descriptor keeps it readable until it's sent. */
    fun sendSnap(context: Context, photo: ByteArray) {
        val name = "Snap ${LocalDateTime.now().format(SNAP_NAME)}.jpg"
        try {
            val file = File.createTempFile("snap", ".jpg", context.cacheDir)
            val descriptor = try {
                file.writeBytes(photo)
                ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
            } finally {
                file.delete()
            }
            synchronized(snaps) { snaps.add(name) }
            outgoingSizes[name] = photo.size.toLong()
            Lis.client.sendSnap(descriptor.detachFd(), name, photo.size.toULong())
            snapNote(SnapNote("Sending the photo…", done = false))
        } catch (error: Exception) {
            synchronized(snaps) { snaps.remove(name) }
            Log.w(TAG, "can't send a photo", error)
            snapNote(SnapNote("Couldn't send the photo: ${error.message}", failed = true))
        }
    }

    private fun snapNote(note: SnapNote) {
        Lis.update { copy(camera = camera.copy(snap = note)) }
    }

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
            outgoingSizes[name] = size
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
        val size = if (isIncoming) incomingSizes.remove(id) else outgoingSizes.remove(name)
        val verb = if (isIncoming) "Received" else "Sent"
        EventLog.add(
            if (isIncoming) EventKind.FILE_RECEIVED else EventKind.FILE_SENT,
            if (ok) "$verb $name" else "Couldn't ${if (isIncoming) "receive" else "send"} $name",
            size,
            failed = !ok,
        )
        if (!isIncoming && synchronized(snaps) { snaps.remove(name) }) {
            snapNote(if (ok) SnapNote("Sent to the PC") else SnapNote("Couldn't send the photo", failed = true))
            return
        }
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
