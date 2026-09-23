package dev.escanor

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
import android.os.IBinder
import android.os.PowerManager
import android.util.Log

/**
 * Foreground-сервис с типами camera|microphone: держит сервер и захват,
 * пока экран телефона выключен или приложение свёрнуто.
 */
class StreamService : Service() {

    private var server: Server? = null
    private var video: VideoStreamer? = null
    private var audio: AudioStreamer? = null
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        startForeground(NOTIFICATION_ID, notification(), foregroundTypes())
        if (server == null) {
            val v = VideoStreamer(this).also { video = it }
            val a = AudioStreamer(this).also { audio = it }
            try {
                server = Server(this, v, a).also { it.start() }
            } catch (e: Exception) {
                Log.e(TAG, "server start failed", e)
                Status.set("Не удалось открыть порт ${Protocol.PORT}: ${e.message}")
            }
            wakeLock = getSystemService(PowerManager::class.java)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "escanor:stream")
                .apply { acquire() }
        }
        running = true
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        server?.shutdown()
        video?.release()
        audio?.stop()
        wakeLock?.release()
        Status.set("Остановлено")
        super.onDestroy()
    }

    private fun foregroundTypes(): Int {
        var types = ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        }
        return types
    }

    private fun notification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Трансляция", NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, StreamService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentTitle("Escanor работает")
            .setContentText("Камера и микрофон доступны ПК по USB")
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, "Остановить", stop).build())
            .setOngoing(true)
            .build()
    }

    companion object {
        private const val TAG = "EscanorService"
        private const val CHANNEL_ID = "stream"
        private const val NOTIFICATION_ID = 1
        const val ACTION_STOP = "dev.escanor.STOP"

        @Volatile
        var running = false
            private set

        fun start(context: Context) {
            context.startForegroundService(Intent(context, StreamService::class.java))
        }

        fun stop(context: Context) {
            context.stopService(Intent(context, StreamService::class.java))
        }
    }
}
