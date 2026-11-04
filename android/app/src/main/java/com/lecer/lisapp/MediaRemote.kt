package com.lecer.lisapp

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.MediaMetadata
import android.media.VolumeProvider
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import uniffi.lis_ffi.MediaCommand
import uniffi.lis_ffi.MediaInfo

/**
 * What's playing on the PC, as a media session of its own: Android puts it on the lock screen, and
 * while it's there the phone's volume buttons turn the PC's volume, as with a Chromecast.
 */
object MediaRemote {
    private const val VOLUME_STEP = 5
    private const val MAX_VOLUME = 100
    private const val ACTION_VOLUME_UP = "volume_up"
    private const val ACTION_VOLUME_DOWN = "volume_down"

    private val main = Handler(Looper.getMainLooper())
    private var session: MediaSession? = null
    private var volume: VolumeProvider? = null
    private var media: MediaInfo? = null
    private var art: Bitmap? = null

    private fun setVolume(percent: Int) {
        val clamped = percent.coerceIn(0, MAX_VOLUME)
        volume?.currentVolume = clamped
        Lis.client.mediaSetVolume(clamped.toUByte())
    }

    private val callback = object : MediaSession.Callback() {
        override fun onPlay() = Lis.client.mediaCommand(MediaCommand.PLAY_PAUSE)
        override fun onPause() = Lis.client.mediaCommand(MediaCommand.PLAY_PAUSE)
        override fun onSkipToNext() = Lis.client.mediaCommand(MediaCommand.NEXT)
        override fun onSkipToPrevious() = Lis.client.mediaCommand(MediaCommand.PREVIOUS)

        override fun onCustomAction(action: String, extras: android.os.Bundle?) {
            val current = volume?.currentVolume ?: return
            when (action) {
                ACTION_VOLUME_UP -> setVolume(current + VOLUME_STEP)
                ACTION_VOLUME_DOWN -> setVolume(current - VOLUME_STEP)
            }
        }
    }

    private fun openSession(context: Context): MediaSession =
        session ?: MediaSession(context.applicationContext, "LisPcMedia").also { created ->
            created.setCallback(callback, main)
            val provider = object : VolumeProvider(VOLUME_CONTROL_ABSOLUTE, MAX_VOLUME, 0) {
                override fun onSetVolumeTo(target: Int) = setVolume(target)
                override fun onAdjustVolume(direction: Int) {
                    if (direction != 0) setVolume(currentVolume + direction * VOLUME_STEP)
                }
            }
            created.setPlaybackToRemote(provider)
            created.isActive = true
            volume = provider
            session = created
        }

    private fun state(info: MediaInfo): PlaybackState {
        val playing = info.playing
        return PlaybackState.Builder()
            .setActions(
                PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE or
                    PlaybackState.ACTION_SKIP_TO_NEXT or PlaybackState.ACTION_SKIP_TO_PREVIOUS,
            )
            .setState(
                if (playing) PlaybackState.STATE_PLAYING else PlaybackState.STATE_PAUSED,
                info.positionMs.toLong(),
                if (playing) 1f else 0f,
                SystemClock.elapsedRealtime(),
            )
            .addCustomAction(ACTION_VOLUME_DOWN, "Volume down", R.drawable.ic_volume_down)
            .addCustomAction(ACTION_VOLUME_UP, "Volume up", R.drawable.ic_volume_up)
            .build()
    }

    private fun metadata(info: MediaInfo): MediaMetadata {
        val builder = MediaMetadata.Builder()
            .putString(MediaMetadata.METADATA_KEY_TITLE, info.title)
            .putString(MediaMetadata.METADATA_KEY_ARTIST, info.artist)
            .putString(MediaMetadata.METADATA_KEY_ALBUM, info.album)
            .putLong(MediaMetadata.METADATA_KEY_DURATION, info.lengthMs.toLong())
        art?.let { builder.putBitmap(MediaMetadata.METADATA_KEY_ART, it) }
        return builder.build()
    }

    private fun publish(context: Context) {
        val info = media ?: return
        val open = openSession(context)
        volume?.currentVolume = info.volumePercent.toInt().coerceIn(0, MAX_VOLUME)
        open.setMetadata(metadata(info))
        open.setPlaybackState(state(info))
        Notifications.showMedia(context, open.sessionToken, info, art)
    }

    /** Null when nothing is left to control. */
    fun update(context: Context, info: MediaInfo?) {
        main.post {
            if (info == null) {
                end(context)
            } else {
                media = info
                publish(context)
            }
        }
    }

    fun updateArt(context: Context, data: ByteArray) {
        main.post {
            art = if (data.isEmpty()) null else BitmapFactory.decodeByteArray(data, 0, data.size)
            publish(context)
        }
    }

    fun clear(context: Context) {
        main.post { end(context) }
    }

    private fun end(context: Context) {
        Notifications.cancelMedia(context)
        session?.isActive = false
        session?.release()
        session = null
        volume = null
        media = null
        art = null
    }
}
