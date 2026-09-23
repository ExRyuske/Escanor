package dev.escanor

import java.nio.ByteBuffer

/** Обёртка над AAudio (cpp/aaudio_capture.c). Дескриптор потока — непрозрачное число. */
object NativeAudio {
    init {
        System.loadLibrary("escanor_audio")
    }

    // Значения AAUDIO_INPUT_PRESET_*.
    private const val PRESET_GENERIC = 1
    private const val PRESET_CAMCORDER = 5
    private const val PRESET_VOICE_RECOGNITION = 6
    private const val PRESET_VOICE_COMMUNICATION = 7
    private const val PRESET_UNPROCESSED = 9
    private const val PRESET_VOICE_PERFORMANCE = 10

    fun preset(source: String) = when (source) {
        "mic" -> PRESET_GENERIC
        "voice_communication" -> PRESET_VOICE_COMMUNICATION
        "voice_recognition" -> PRESET_VOICE_RECOGNITION
        "unprocessed" -> PRESET_UNPROCESSED
        "voice_performance" -> PRESET_VOICE_PERFORMANCE
        else -> PRESET_CAMCORDER
    }

    @JvmStatic external fun open(deviceId: Int, preset: Int, channels: Int, sampleRate: Int): Long
    @JvmStatic external fun read(handle: Long, buffer: ByteBuffer, frames: Int, timeoutNanos: Long): Int
    @JvmStatic external fun frameTime(handle: Long, frame: Long): Long
    @JvmStatic external fun sampleRate(handle: Long): Int
    @JvmStatic external fun channelCount(handle: Long): Int
    @JvmStatic external fun isExclusive(handle: Long): Boolean
    @JvmStatic external fun deviceId(handle: Long): Int
    @JvmStatic external fun close(handle: Long)
}
