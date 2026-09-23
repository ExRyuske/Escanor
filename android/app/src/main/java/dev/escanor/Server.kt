package dev.escanor

import android.content.Context
import android.os.SystemClock
import android.util.Log
import org.json.JSONObject
import java.io.BufferedWriter
import java.io.IOException
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.Executors
import java.util.concurrent.RejectedExecutionException

/**
 * TCP-сервер на 127.0.0.1. ПК подключается через `adb forward` и открывает три соединения;
 * первый байт каждого соединения — номер канала (Protocol.CHANNEL_*).
 */
class Server(
    private val context: Context,
    private val video: VideoStreamer,
    private val audio: AudioStreamer,
) : Thread("escanor-server") {

    private val serverSocket = ServerSocket().apply {
        reuseAddress = true
        bind(InetSocketAddress(InetAddress.getLoopbackAddress(), Protocol.PORT))
    }

    @Volatile
    private var session: Session? = null

    override fun run() {
        Status.set("Ожидание подключения ПК по USB…")
        while (!isInterrupted) {
            val socket = try {
                serverSocket.accept()
            } catch (e: IOException) {
                break
            }
            Thread({ route(socket) }, "escanor-accept").start()
        }
    }

    private fun route(socket: Socket) {
        try {
            socket.tcpNoDelay = true
            when (socket.getInputStream().read()) {
                Protocol.CHANNEL_CONTROL -> {
                    session?.close()
                    val s = Session(context, socket, video, audio) { closed ->
                        if (session === closed) {
                            session = null
                            Status.set("ПК отключён. Ожидание подключения…")
                        }
                    }
                    session = s
                    s.run()
                }
                Protocol.CHANNEL_VIDEO -> session?.attachVideo(socket) ?: socket.close()
                Protocol.CHANNEL_AUDIO -> session?.attachAudio(socket) ?: socket.close()
                else -> socket.close()
            }
        } catch (e: IOException) {
            Log.w(TAG, "connection failed", e)
            socket.close()
        }
    }

    fun shutdown() {
        interrupt()
        try {
            serverSocket.close()
        } catch (_: IOException) {
        }
        session?.close()
    }

    companion object {
        private const val TAG = "EscanorServer"
    }
}

/** Одно подключение ПК: управляющий канал (JSON-строки) и привязанные к нему медиаканалы. */
class Session(
    private val context: Context,
    private val control: Socket,
    private val video: VideoStreamer,
    private val audio: AudioStreamer,
    private val onClosed: (Session) -> Unit,
) : VideoStreamer.Listener, AudioStreamer.Listener {

    private val writer: BufferedWriter = control.getOutputStream().bufferedWriter()

    @Volatile
    private var videoSocket: Socket? = null

    @Volatile
    private var audioSocket: Socket? = null

    @Volatile
    private var videoOut: PacketWriter? = null

    @Volatile
    private var audioOut: PacketWriter? = null

    @Volatile
    private var closed = false

    /** Нажатия приходят из главного потока, а писать в сокет из него Android запрещает. */
    private val pressSender = Executors.newSingleThreadExecutor()

    fun run() {
        Macropad.onPress = { page, id, down ->
            try {
                pressSender.execute {
                    send(JSONObject().put("type", "macropad_press").put("page", page).put("id", id).put("down", down))
                }
            } catch (_: RejectedExecutionException) {
                // Сессия уже закрыта — нажатие некуда отправлять.
            }
        }
        try {
            control.getInputStream().bufferedReader().use { reader ->
                while (!closed) {
                    val line = reader.readLine() ?: break
                    if (line.isBlank()) continue
                    try {
                        handle(JSONObject(line))
                    } catch (e: Exception) {
                        Log.w(TAG, "bad message: $line", e)
                        send(error("Ошибка обработки команды: ${e.message}"))
                    }
                }
            }
        } catch (_: IOException) {
        } finally {
            close()
        }
    }

    fun attachVideo(socket: Socket) {
        // Небольшой буфер отправки: если ПК на мгновение отстал, в очереди не копятся секунды видео.
        socket.sendBufferSize = 256 * 1024
        videoSocket?.close()
        videoSocket = socket
        videoOut = PacketWriter(socket.getOutputStream())
        send(JSONObject().put("type", "channel").put("channel", "video"))
    }

    fun attachAudio(socket: Socket) {
        audioSocket?.close()
        audioSocket = socket
        audioOut = PacketWriter(socket.getOutputStream())
        send(JSONObject().put("type", "channel").put("channel", "audio"))
    }

    private fun handle(msg: JSONObject) {
        when (msg.getString("type")) {
            "hello" -> {
                send(
                    JSONObject()
                        .put("type", "hello")
                        .put("version", Protocol.VERSION)
                        .put("phone", DeviceInfo.phone()),
                )
                Status.set("Подключено к ПК")
            }
            "get_devices" -> send(DeviceInfo.devices(context).put("type", "devices"))
            "ping" -> send(
                JSONObject()
                    .put("type", "pong")
                    .put("t", msg.getLong("t"))
                    .put("realtime_us", SystemClock.elapsedRealtimeNanos() / 1000)
                    .put("monotonic_us", System.nanoTime() / 1000),
            )
            "start_video" -> {
                val out = videoOut ?: return send(error("Видеоканал не подключён"))
                video.start(VideoConfig.fromJson(msg), out, this)
            }
            "stop_video" -> {
                video.stop()
                send(JSONObject().put("type", "video_stopped"))
                Status.set("Подключено к ПК")
            }
            "request_keyframe" -> video.requestKeyframe()
            "set_controls" -> video.setControls(msg)
            "start_audio" -> {
                val out = audioOut ?: return send(error("Аудиоканал не подключён"))
                audio.start(AudioConfig.fromJson(msg), out, this)
            }
            "stop_audio" -> {
                audio.stop()
                send(JSONObject().put("type", "audio_stopped"))
            }
            "macropad" -> Macropad.setLayout(msg)
            "macropad_state" -> Macropad.setState(msg.getInt("page"), msg.getInt("id"), msg.getInt("state"))
            "macropad_off" -> Macropad.clear()
            else -> send(error("Неизвестная команда: ${msg.getString("type")}"))
        }
    }

    override fun onVideoStarted(info: JSONObject) {
        send(info.put("type", "video_started"))
        Status.set("Передаётся видео ${info.getInt("width")}×${info.getInt("height")} @ ${info.getInt("fps")}")
    }

    override fun onVideoError(message: String) {
        send(JSONObject().put("type", "video_error").put("message", message))
    }

    override fun onAudioStarted(info: JSONObject) = send(info.put("type", "audio_started"))

    override fun onAudioError(message: String) {
        send(JSONObject().put("type", "audio_error").put("message", message))
    }

    @Synchronized
    fun send(msg: JSONObject) {
        if (closed) return
        try {
            writer.write(msg.toString())
            writer.write('\n'.code)
            writer.flush()
        } catch (e: IOException) {
            Log.w(TAG, "send failed", e)
        }
    }

    fun close() {
        synchronized(this) {
            if (closed) return
            closed = true
        }
        video.stop()
        audio.stop()
        Macropad.onPress = null
        Macropad.clear()
        pressSender.shutdown()
        for (s in listOf(videoSocket, audioSocket, control)) {
            try {
                s?.close()
            } catch (_: IOException) {
            }
        }
        onClosed(this)
    }

    private fun error(message: String) = JSONObject().put("type", "error").put("message", message)

    companion object {
        private const val TAG = "EscanorSession"
    }
}
