package com.matthewdovi.audionet.engine

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.database.ContentObserver
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import uniffi.audionet_ffi.DeviceVolume
import uniffi.audionet_ffi.PlatformSoundSettings
import uniffi.audionet_ffi.setPlatformSoundSettings
import uniffi.audionet_ffi.soundSettingsChanged

/**
 * The phone's own volumes (media, calls, ring, notifications, alarms,
 * accessibility) for AudioNet's device control: the account's other
 * devices see them and may change them while "Let my other devices change
 * my sound settings" is on. Android routes sound itself, so these are what
 * there is to change (no default devices, nothing to turn off).
 *
 * Changes made anywhere (the volume keys, the system panel, other apps,
 * AudioNet) are told to the engine at once, so every screen updates.
 */
class PhoneVolumes private constructor(context: Context) : PlatformSoundSettings {
    private val app = context.applicationContext
    private val audio = app.getSystemService(AudioManager::class.java)

    private data class Stream(val id: String, val type: Int, val name: String)

    private val streams = buildList {
        add(Stream("stream:music", AudioManager.STREAM_MUSIC, "Media volume"))
        add(Stream("stream:voice_call", AudioManager.STREAM_VOICE_CALL, "Call volume"))
        add(Stream("stream:ring", AudioManager.STREAM_RING, "Ring volume"))
        add(Stream("stream:notification", AudioManager.STREAM_NOTIFICATION, "Notification volume"))
        add(Stream("stream:alarm", AudioManager.STREAM_ALARM, "Alarm volume"))
        add(Stream("stream:accessibility", AudioManager.STREAM_ACCESSIBILITY, "Accessibility volume"))
    }

    private fun min(type: Int): Int =
        if (Build.VERSION.SDK_INT >= 28) audio.getStreamMinVolume(type) else 0

    /** Muting is offered where Android allows a stream to be silent. */
    private fun canMute(type: Int): Boolean =
        !audio.isVolumeFixed && min(type) == 0 &&
            type != AudioManager.STREAM_ACCESSIBILITY && type != AudioManager.STREAM_VOICE_CALL

    override fun volumes(): List<DeviceVolume> = streams.mapNotNull { s ->
        val max = audio.getStreamMaxVolume(s.type)
        if (max <= 0) return@mapNotNull null
        DeviceVolume(
            id = s.id,
            name = s.name,
            volume = audio.getStreamVolume(s.type).toFloat() / max,
            muted = if (canMute(s.type)) audio.isStreamMute(s.type) else null,
            canMute = canMute(s.type),
        )
    }

    private fun stream(id: String) = streams.firstOrNull { it.id == id }

    override fun setVolume(id: String, volume: Float): String? {
        val s = stream(id) ?: return "That isn't one of this phone's volumes."
        if (audio.isVolumeFixed) return "This phone's volume can't be changed."
        val max = audio.getStreamMaxVolume(s.type)
        val index = Math.round(volume.coerceIn(0f, 1f) * max).coerceIn(min(s.type), max)
        return attempt { audio.setStreamVolume(s.type, index, 0) }
    }

    override fun setMuted(id: String, muted: Boolean): String? {
        val s = stream(id) ?: return "That isn't one of this phone's volumes."
        if (!canMute(s.type)) return "Android doesn't let ${s.name.lowercase()} be muted."
        val direction = if (muted) AudioManager.ADJUST_MUTE else AudioManager.ADJUST_UNMUTE
        return attempt { audio.adjustStreamVolume(s.type, direction, 0) }
    }

    /** Ring and notification volumes are locked while Do Not Disturb is on. */
    private fun attempt(change: () -> Unit): String? = try {
        change()
        null
    } catch (e: SecurityException) {
        "Android didn't allow it: Do Not Disturb is on, and AudioNet isn't allowed to change it."
    }

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) = soundSettingsChanged()
    }

    private val observer = object : ContentObserver(Handler(Looper.getMainLooper())) {
        override fun onChange(selfChange: Boolean) = soundSettingsChanged()
    }

    private fun startWatching() {
        val filter = IntentFilter().apply {
            // Sent by Android for every stream volume change (not in the
            // public API, but long-standing); the settings observer below
            // covers changes it misses.
            addAction("android.media.VOLUME_CHANGED_ACTION")
            addAction("android.media.STREAM_MUTE_CHANGED_ACTION")
            addAction(AudioManager.RINGER_MODE_CHANGED_ACTION)
        }
        if (Build.VERSION.SDK_INT >= 33) {
            app.registerReceiver(receiver, filter, Context.RECEIVER_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            app.registerReceiver(receiver, filter)
        }
        app.contentResolver.registerContentObserver(Settings.System.CONTENT_URI, true, observer)
    }

    companion object {
        @Volatile private var installed: PhoneVolumes? = null

        /** Gives the engine the phone's volumes; once, at startup. */
        fun install(context: Context) {
            if (installed != null) return
            val v = PhoneVolumes(context)
            installed = v
            setPlatformSoundSettings(v)
            v.startWatching()
        }
    }
}
