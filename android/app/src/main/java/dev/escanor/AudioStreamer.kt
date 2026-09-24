package dev.escanor

import android.annotation.SuppressLint
import android.content.Context
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.MediaRecorder
import android.util.Log
import org.json.JSONObject
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder

data class AudioConfig(val deviceId: Int?, val source: String, val channels: Int) {
    companion object {
        fun fromJson(json: JSONObject) = AudioConfig(
            deviceId = if (json.has("device") && !json.isNull("device")) json.getInt("device") else null,
            source = json.optString("source", "camcorder"),
            channels = json.optInt("channels", 1).coerceIn(1, 2),
        )
    }
}

/**
 * Микрофон → PCM s16le 48 кГц кусками по 5 мс → PacketWriter.
 * Основной путь — AAudio в режиме минимальной задержки; если не открылся — AudioRecord.
 */
class AudioStreamer(private val context: Context) {

    interface Listener {
        fun onAudioStarted(info: JSONObject)
        fun onAudioError(message: String)
    }

    private var worker: Worker? = null

    @Synchronized
    fun start(config: AudioConfig, out: PacketWriter, listener: Listener) {
        stop()
        worker = Worker(config, out, listener).also { it.start() }
    }

    @Synchronized
    fun stop() {
        worker?.let {
            it.running = false
            it.join(500)
        }
        worker = null
    }

    /** Один источник звука: AAudio или AudioRecord. */
    private interface Capture {
        val info: JSONObject
        /** Читает кусок в buffer (position = 0, limit = длина), возвращает время его начала (мкс, CLOCK_MONOTONIC). */
        fun read(buffer: ByteBuffer): Long
        fun close()
    }

    private inner class Worker(
        private val config: AudioConfig,
        private val out: PacketWriter,
        private val listener: Listener,
    ) : Thread("escanor-audio") {
        @Volatile
        var running = true

        override fun run() {
            priority = MAX_PRIORITY
            val capture = try {
                openAAudio() ?: openAudioRecord()
            } catch (e: Exception) {
                Log.e(TAG, "microphone failed", e)
                listener.onAudioError("Не удалось открыть микрофон: ${e.message}")
                return
            }
            val buffer = ByteBuffer.allocateDirect(CHUNK_FRAMES * config.channels * 2).order(ByteOrder.LITTLE_ENDIAN)
            try {
                listener.onAudioStarted(capture.info)
                while (running) {
                    buffer.clear()
                    val pts = capture.read(buffer)
                    if (buffer.remaining() > 0) out.write(0, pts, buffer)
                }
            } catch (e: IOException) {
                if (running) listener.onAudioError("Звуковой канал закрыт: ${e.message}")
            } finally {
                capture.close()
            }
        }

        private fun openAAudio(): Capture? {
            val handle = try {
                NativeAudio.open(config.deviceId ?: 0, NativeAudio.preset(config.source), config.channels, SAMPLE_RATE)
            } catch (e: LinkageError) {
                // Библиотеки нет: первое обращение бросает ExceptionInInitializerError, следующие —
                // NoClassDefFoundError; оба — LinkageError, как и UnsatisfiedLinkError.
                Log.w(TAG, "AAudio library missing", e)
                0L
            }
            if (handle == 0L) return null
            if (NativeAudio.sampleRate(handle) != SAMPLE_RATE || NativeAudio.channelCount(handle) != config.channels) {
                NativeAudio.close(handle)
                return null
            }
            var framesRead = 0L
            val exclusive = NativeAudio.isExclusive(handle)
            return object : Capture {
                override val info: JSONObject = JSONObject()
                    .put("sample_rate", SAMPLE_RATE)
                    .put("channels", config.channels)
                    .put("device", NativeAudio.deviceId(handle))
                    .put("api", if (exclusive) "AAudio, эксклюзивный" else "AAudio, общий")

                override fun read(buffer: ByteBuffer): Long {
                    val n = NativeAudio.read(handle, buffer, CHUNK_FRAMES, 100_000_000L)
                    if (n < 0) throw IOException("AAudio read = $n")
                    val start = framesRead
                    framesRead += n
                    buffer.limit(n * config.channels * 2)
                    val t = NativeAudio.frameTime(handle, start)
                    return if (t > 0) t / 1000 else System.nanoTime() / 1000 - CHUNK_US
                }

                override fun close() = NativeAudio.close(handle)
            }
        }

        @SuppressLint("MissingPermission")
        private fun openAudioRecord(): Capture {
            val mask = if (config.channels == 2) AudioFormat.CHANNEL_IN_STEREO else AudioFormat.CHANNEL_IN_MONO
            val chunkBytes = CHUNK_FRAMES * config.channels * 2
            val minBuffer = AudioRecord.getMinBufferSize(SAMPLE_RATE, mask, AudioFormat.ENCODING_PCM_16BIT)
            val record = AudioRecord.Builder()
                .setAudioSource(sourceId(config.source))
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(SAMPLE_RATE)
                        .setChannelMask(mask)
                        .build(),
                )
                .setBufferSizeInBytes(maxOf(minBuffer, chunkBytes * 4))
                .build()
            if (record.state != AudioRecord.STATE_INITIALIZED) {
                record.release()
                throw IllegalStateException("AudioRecord не инициализирован")
            }
            config.deviceId?.let { id ->
                val manager = context.getSystemService(AudioManager::class.java)
                manager.getDevices(AudioManager.GET_DEVICES_INPUTS).firstOrNull { it.id == id }
                    ?.let { record.setPreferredDevice(it) }
            }
            record.startRecording()
            val timestamp = AudioTimestamp()
            var framesRead = 0L
            return object : Capture {
                override val info: JSONObject = JSONObject()
                    .put("sample_rate", SAMPLE_RATE)
                    .put("channels", config.channels)
                    .put("device", record.routedDevice?.id ?: JSONObject.NULL)
                    .put("api", "AudioRecord")

                override fun read(buffer: ByteBuffer): Long {
                    val n = record.read(buffer, chunkBytes, AudioRecord.READ_BLOCKING)
                    if (n < 0) throw IOException("AudioRecord.read = $n")
                    buffer.limit(n)
                    val start = framesRead
                    framesRead += n / (config.channels * 2)
                    return if (record.getTimestamp(timestamp, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
                        (timestamp.nanoTime + (start - timestamp.framePosition) * 1_000_000_000L / SAMPLE_RATE) / 1000
                    } else {
                        System.nanoTime() / 1000 - CHUNK_US
                    }
                }

                override fun close() {
                    try {
                        record.stop()
                    } catch (_: IllegalStateException) {
                    }
                    record.release()
                }
            }
        }
    }

    companion object {
        private const val TAG = "EscanorAudio"
        const val SAMPLE_RATE = 48_000
        /** 5 мс — меньше кусок, меньше задержка; накладные расходы TCP при этом ничтожны. */
        private const val CHUNK_FRAMES = SAMPLE_RATE / 200
        private const val CHUNK_US = 5_000L

        fun sourceId(name: String) = when (name) {
            "mic" -> MediaRecorder.AudioSource.MIC
            "voice_communication" -> MediaRecorder.AudioSource.VOICE_COMMUNICATION
            "voice_recognition" -> MediaRecorder.AudioSource.VOICE_RECOGNITION
            "unprocessed" -> MediaRecorder.AudioSource.UNPROCESSED
            "voice_performance" -> MediaRecorder.AudioSource.VOICE_PERFORMANCE
            else -> MediaRecorder.AudioSource.CAMCORDER
        }
    }
}
