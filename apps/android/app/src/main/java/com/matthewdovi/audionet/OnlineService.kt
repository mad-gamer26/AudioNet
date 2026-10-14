package com.matthewdovi.audionet

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * Keeps AudioNet online, and its streams playing and recording, with the
 * screen off or another app in front: a foreground service, running while
 * this phone is signed in and online, with the notification Android
 * requires ("AudioNet: online in alice on audionet.example.com, 1 stream").
 *
 * While a stream runs it also keeps the CPU awake and Wi-Fi in its low
 * latency mode, and lets multicast in (finding devices on this network);
 * all three are let go when the last stream ends.
 */
class OnlineService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val text = intent?.getStringExtra(EXTRA_TEXT) ?: "Online."
        val types = if (microphoneAllowed(this)) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        } else {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        }
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(this, text), types)
        return START_STICKY
    }

    override fun onDestroy() {
        releaseLocks()
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "online"
        private const val NOTIFICATION_ID = 1
        private const val EXTRA_TEXT = "text"
        private var running = false
        private var lastText = ""
        private var wakeLock: PowerManager.WakeLock? = null
        private var wifiLock: WifiManager.WifiLock? = null
        private var multicastLock: WifiManager.MulticastLock? = null

        private fun microphoneAllowed(context: Context) =
            ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED

        /** Starts, updates or stops the service to match the model. */
        fun update(context: Context, model: AppModel) {
            if (!model.online) {
                if (running) {
                    context.stopService(Intent(context, OnlineService::class.java))
                    running = false
                    lastText = ""
                }
                return
            }
            val text = describe(model)
            if (running && text == lastText) return
            lastText = text
            if (!running) {
                running = true
                // Started while AudioNet is in front (at launch or sign-in),
                // as Android requires of a foreground service.
                runCatching {
                    ContextCompat.startForegroundService(context, Intent(context, OnlineService::class.java).putExtra(EXTRA_TEXT, text))
                }.onFailure {
                    running = false
                    model.note("AudioNet could not stay online in the background: ${it.message}")
                }
            } else {
                val manager = context.getSystemService(NotificationManager::class.java)
                manager.notify(NOTIFICATION_ID, notification(context, text))
            }
        }

        /** The service's state in words. */
        private fun describe(model: AppModel): String {
            val where = if (model.accounts.size == 1) model.accounts[0].accountName else "${model.accounts.size} accounts"
            val sharing = if (model.sharingAccounts.isEmpty()) "not sharing" else "sharing"
            val streams = when (model.engineStreams) {
                0 -> "no streams"
                1 -> "1 stream"
                else -> "${model.engineStreams} streams"
            }
            val state = if (model.connected) "Online" else "Connecting"
            return "$state in $where, $sharing, $streams."
        }

        private fun notification(context: Context, text: String): Notification {
            val manager = context.getSystemService(NotificationManager::class.java)
            if (manager.getNotificationChannel(CHANNEL) == null) {
                manager.createNotificationChannel(NotificationChannel(CHANNEL, context.getString(R.string.channel_online), NotificationManager.IMPORTANCE_LOW).apply {
                    description = context.getString(R.string.channel_online_description)
                    setShowBadge(false)
                })
            }
            val open = PendingIntent.getActivity(context, 0, Intent(context, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE)
            return NotificationCompat.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_launcher_foreground)
                .setContentTitle("AudioNet")
                .setContentText(text)
                .setContentIntent(open)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setCategory(NotificationCompat.CATEGORY_SERVICE)
                .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
                .build()
        }

        /** Streams started or ended: the locks follow. */
        fun streamsChanged(context: Context, streams: Int, microphones: Int) {
            if (streams > 0) acquireLocks(context) else releaseLocks()
        }

        private fun acquireLocks(context: Context) {
            if (wakeLock?.isHeld != true) {
                val power = context.getSystemService(PowerManager::class.java)
                wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "AudioNet:streaming").apply {
                    setReferenceCounted(false)
                    acquire()
                }
            }
            val wifi = context.applicationContext.getSystemService(WifiManager::class.java) ?: return
            if (wifiLock?.isHeld != true) {
                val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) WifiManager.WIFI_MODE_FULL_LOW_LATENCY
                           else @Suppress("DEPRECATION") WifiManager.WIFI_MODE_FULL_HIGH_PERF
                wifiLock = wifi.createWifiLock(mode, "AudioNet:streaming").apply {
                    setReferenceCounted(false)
                    acquire()
                }
            }
            if (multicastLock?.isHeld != true) {
                multicastLock = wifi.createMulticastLock("AudioNet:local").apply {
                    setReferenceCounted(false)
                    acquire()
                }
            }
        }

        private fun releaseLocks() {
            wakeLock?.takeIf { it.isHeld }?.release()
            wifiLock?.takeIf { it.isHeld }?.release()
            multicastLock?.takeIf { it.isHeld }?.release()
            wakeLock = null
            wifiLock = null
            multicastLock = null
        }
    }
}
