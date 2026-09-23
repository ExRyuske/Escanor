package dev.escanor

import java.io.IOException
import java.io.OutputStream
import java.nio.ByteBuffer

/** Константы протокола. Описание формата — docs/protocol.md. */
object Protocol {
    const val PORT = 27183
    const val VERSION = 6

    const val CHANNEL_CONTROL = 1
    const val CHANNEL_VIDEO = 2
    const val CHANNEL_AUDIO = 3

    const val FLAG_CONFIG = 1
    const val FLAG_KEYFRAME = 2
}

/**
 * Пишет медиапакеты: [u32 size][u8 flags][u64 pts_us][payload], big-endian.
 * Заголовок и данные уходят одним write(), чтобы не плодить мелкие TCP-сегменты.
 */
class PacketWriter(private val out: OutputStream) {
    private var buf = ByteArray(256 * 1024)

    @Synchronized
    @Throws(IOException::class)
    fun write(flags: Int, ptsUs: Long, data: ByteBuffer) {
        val size = data.remaining()
        val total = HEADER_SIZE + size
        if (buf.size < total) buf = ByteArray(total + total / 2)
        ByteBuffer.wrap(buf).apply {
            putInt(size)
            put(flags.toByte())
            putLong(ptsUs)
        }
        data.get(buf, HEADER_SIZE, size)
        out.write(buf, 0, total)
        out.flush()
    }

    fun close() {
        try {
            out.close()
        } catch (_: IOException) {
        }
    }

    companion object {
        const val HEADER_SIZE = 13
    }
}
