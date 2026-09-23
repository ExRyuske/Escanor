package dev.escanor

import android.annotation.SuppressLint
import android.content.Context
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.MediaCodec
import android.media.MediaCodecInfo.CodecProfileLevel
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.util.Range
import android.view.Surface
import org.json.JSONObject
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit

data class VideoConfig(
    val cameraId: String,
    val physicalId: String?,
    val width: Int,
    val height: Int,
    val fps: Int,
    val bitrate: Int,
) {
    companion object {
        fun fromJson(json: JSONObject) = VideoConfig(
            cameraId = json.getString("camera"),
            physicalId = json.optString("physical").takeIf { it.isNotEmpty() && it != "null" },
            width = json.optInt("width", 1920),
            height = json.optInt("height", 1080),
            fps = json.optInt("fps", 30),
            bitrate = json.optInt("bitrate", 20_000_000),
        )
    }
}

/**
 * Camera2 → Surface → аппаратный MediaCodec → PacketWriter.
 *
 * Всё состояние камеры живёт на одном HandlerThread, поэтому блокировки не нужны.
 * Поток выгрузки кодировщика отдельный и работает только со своими локальными ссылками.
 */
class VideoStreamer(context: Context) {

    interface Listener {
        fun onVideoStarted(info: JSONObject)
        fun onVideoError(message: String)
    }

    private val cameraManager = context.getSystemService(CameraManager::class.java)
    private val thread = HandlerThread("escanor-camera").apply { start() }
    private val handler = Handler(thread.looper)
    private val executor = Executor { handler.post(it) }

    private val controls = CameraControls()

    // Поля ниже трогаются только на потоке handler.
    private var generation = 0
    private var listener: Listener? = null
    private var codec: MediaCodec? = null
    private var surface: Surface? = null
    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var request: CaptureRequest.Builder? = null
    private var characteristics: CameraCharacteristics? = null
    private var drain: DrainThread? = null

    fun start(config: VideoConfig, out: PacketWriter, listener: Listener) = handler.post {
        stopInternal()
        startInternal(config, out, listener)
    }

    fun stop() = handler.post { stopInternal() }

    fun requestKeyframe() = handler.post {
        codec?.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
    }

    fun setControls(json: JSONObject) = handler.post {
        controls.update(json)
        applyRequest()
    }

    /** Синхронно останавливает всё; вызывается при уничтожении сервиса. */
    fun release() {
        val done = CountDownLatch(1)
        handler.post {
            stopInternal()
            done.countDown()
        }
        done.await(3, TimeUnit.SECONDS)
        thread.quitSafely()
    }

    private fun startInternal(config: VideoConfig, out: PacketWriter, listener: Listener) {
        val gen = ++generation
        this.listener = listener
        try {
            val chars = cameraManager.getCameraCharacteristics(config.cameraId)
            characteristics = chars
            val encoder = createEncoder(config)
            val input = encoder.createInputSurface()
            encoder.start()
            codec = encoder
            surface = input
            drain = DrainThread(gen, encoder, out).also { it.start() }
            openCamera(gen, config, chars, input)
        } catch (e: Exception) {
            Log.e(TAG, "start failed", e)
            fail(gen, "Не удалось запустить видео: ${e.message}")
        }
    }

    @SuppressLint("MissingPermission")
    private fun openCamera(gen: Int, config: VideoConfig, chars: CameraCharacteristics, input: Surface) {
        cameraManager.openCamera(config.cameraId, executor, object : CameraDevice.StateCallback() {
            override fun onOpened(camera: CameraDevice) {
                if (gen != generation) {
                    camera.close()
                    return
                }
                device = camera
                createSession(gen, config, chars, camera, input)
            }

            override fun onDisconnected(camera: CameraDevice) {
                camera.close()
                fail(gen, "Камера отключена системой (её заняло другое приложение?)")
            }

            override fun onError(camera: CameraDevice, error: Int) {
                camera.close()
                fail(gen, "Ошибка камеры: код $error")
            }
        })
    }

    private fun createSession(
        gen: Int,
        config: VideoConfig,
        chars: CameraCharacteristics,
        camera: CameraDevice,
        input: Surface,
    ) {
        val fpsRange = chooseFpsRange(chars, config.fps)
        val builder = camera.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
            addTarget(input)
            set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, fpsRange)
        }
        controls.apply(builder, chars)
        request = builder

        val output = OutputConfiguration(input).apply {
            config.physicalId?.let { setPhysicalCameraId(it) }
        }
        val sessionConfig = SessionConfiguration(
            SessionConfiguration.SESSION_REGULAR,
            listOf(output),
            executor,
            object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) {
                    if (gen != generation) {
                        s.close()
                        return
                    }
                    session = s
                    try {
                        s.setRepeatingRequest(builder.build(), null, handler)
                    } catch (e: Exception) {
                        fail(gen, "Камера отклонила запрос: ${e.message}")
                        return
                    }
                    listener?.onVideoStarted(startedInfo(config, fpsRange))
                }

                override fun onConfigureFailed(s: CameraCaptureSession) {
                    fail(gen, "Камера не поддерживает ${config.width}×${config.height} с этим объективом")
                }
            },
        )
        sessionConfig.sessionParameters = builder.build()
        camera.createCaptureSession(sessionConfig)
    }

    private fun startedInfo(config: VideoConfig, fpsRange: Range<Int>): JSONObject {
        val physicalChars = config.physicalId?.let { cameraManager.getCameraCharacteristics(it) }
        val source = (physicalChars ?: characteristics)?.get(CameraCharacteristics.SENSOR_INFO_TIMESTAMP_SOURCE)
        return JSONObject()
            .put("camera", config.cameraId)
            .put("physical", config.physicalId ?: JSONObject.NULL)
            .put("width", config.width)
            .put("height", config.height)
            .put("fps", fpsRange.upper)
            .put("encoder", codec?.name ?: "")
            .put(
                "timestamp_source",
                if (source == CameraMetadata.SENSOR_INFO_TIMESTAMP_SOURCE_REALTIME) "realtime" else "monotonic",
            )
    }

    private fun applyRequest() {
        val builder = request ?: return
        val s = session ?: return
        val chars = characteristics ?: return
        controls.apply(builder, chars)
        try {
            s.setRepeatingRequest(builder.build(), null, handler)
        } catch (e: Exception) {
            Log.w(TAG, "setRepeatingRequest failed", e)
        }
    }

    private fun fail(gen: Int, message: String) {
        if (gen != generation) return
        val l = listener
        stopInternal()
        l?.onVideoError(message)
    }

    private fun stopInternal() {
        generation++
        try {
            session?.close()
        } catch (_: Exception) {
        }
        device?.close()
        drain?.let {
            it.running = false
            it.join(500)
        }
        codec?.let {
            try {
                it.stop()
            } catch (_: Exception) {
            }
            it.release()
        }
        surface?.release()
        session = null
        device = null
        request = null
        drain = null
        codec = null
        surface = null
        listener = null
    }

    private fun createEncoder(config: VideoConfig): MediaCodec {
        val mime = MediaFormat.MIMETYPE_VIDEO_AVC
        val encoder = MediaCodec.createEncoderByType(mime)
        val format = MediaFormat.createVideoFormat(mime, config.width, config.height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, android.media.MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, config.bitrate)
            setInteger(MediaFormat.KEY_BITRATE_MODE, android.media.MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_VBR)
            setInteger(MediaFormat.KEY_FRAME_RATE, config.fps)
            // Ключевые кадры редко: по TCP ничего не теряется, а I-кадры — это всплески битрейта.
            // ПК запрашивает ключевой кадр сам, когда он нужен.
            setFloat(MediaFormat.KEY_I_FRAME_INTERVAL, 10f)
            // Реальное время: кодировщик не копит кадры и работает на максимальной частоте.
            setInteger(MediaFormat.KEY_PRIORITY, 0)
            setInteger(MediaFormat.KEY_OPERATING_RATE, Short.MAX_VALUE.toInt())
            setInteger(MediaFormat.KEY_LATENCY, 1)
            setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1)
            // Фиксируем цветовое пространство: ПК и виртуальная камера считают, что это BT.709 limited.
            setInteger(MediaFormat.KEY_COLOR_STANDARD, MediaFormat.COLOR_STANDARD_BT709)
            setInteger(MediaFormat.KEY_COLOR_RANGE, MediaFormat.COLOR_RANGE_LIMITED)
            setInteger(MediaFormat.KEY_COLOR_TRANSFER, MediaFormat.COLOR_TRANSFER_SDR_VIDEO)
            // Расширение Qualcomm; остальные кодировщики неизвестный ключ игнорируют.
            setInteger("vendor.qti-ext-enc-low-latency.enable", 1)
            // Baseline: без B-кадров и CABAC — минимальная задержка и совместимость с декодером на ПК.
            val profiles = encoder.codecInfo.getCapabilitiesForType(mime).profileLevels.map { it.profile }
            val profile = if (CodecProfileLevel.AVCProfileConstrainedBaseline in profiles) {
                CodecProfileLevel.AVCProfileConstrainedBaseline
            } else {
                CodecProfileLevel.AVCProfileBaseline
            }
            setInteger(MediaFormat.KEY_PROFILE, profile)
        }
        try {
            encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (e: Exception) {
            // Некоторые кодировщики отвергают отдельные ключи — пробуем без необязательных.
            Log.w(TAG, "configure failed, retrying with basic format", e)
            encoder.reset()
            format.removeKey(MediaFormat.KEY_PROFILE)
            format.removeKey(MediaFormat.KEY_OPERATING_RATE)
            format.removeKey("vendor.qti-ext-enc-low-latency.enable")
            encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        }
        return encoder
    }

    private fun chooseFpsRange(chars: CameraCharacteristics, fps: Int): Range<Int> {
        val ranges = chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)
            ?: return Range(fps, fps)
        return ranges.firstOrNull { it.lower == fps && it.upper == fps }
            ?: ranges.filter { it.upper == fps }.maxByOrNull { it.lower }
            ?: ranges.filter { it.upper <= fps }.maxWithOrNull(compareBy({ it.upper }, { it.lower }))
            ?: ranges.first()
    }

    private inner class DrainThread(
        private val gen: Int,
        private val encoder: MediaCodec,
        private val out: PacketWriter,
    ) : Thread("escanor-video-drain") {
        @Volatile
        var running = true

        override fun run() {
            val info = MediaCodec.BufferInfo()
            while (running) {
                val index = try {
                    encoder.dequeueOutputBuffer(info, 100_000)
                } catch (e: IllegalStateException) {
                    break
                }
                if (index < 0) continue
                val buffer = encoder.getOutputBuffer(index)
                if (buffer != null && info.size > 0) {
                    buffer.position(info.offset)
                    buffer.limit(info.offset + info.size)
                    var flags = 0
                    if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) flags = flags or Protocol.FLAG_CONFIG
                    if (info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0) flags = flags or Protocol.FLAG_KEYFRAME
                    try {
                        out.write(flags, info.presentationTimeUs, buffer)
                    } catch (e: IOException) {
                        running = false
                        handler.post { fail(gen, "ПК закрыл видеоканал") }
                    }
                }
                encoder.releaseOutputBuffer(index, false)
            }
        }
    }

    companion object {
        private const val TAG = "EscanorVideo"
    }
}

/** Текущие ручные настройки камеры; применяются к каждому повторяющемуся запросу. */
class CameraControls {
    var zoom: Float? = null
    var ev = 0
    var focus = "continuous"
    var focusDistance = 0f
    var whiteBalance = "auto"
    var torch = false
    var stabilization = false
    var aeLock = false
    var awbLock = false

    fun update(json: JSONObject) {
        if (json.has("zoom")) zoom = json.getDouble("zoom").toFloat()
        if (json.has("ev")) ev = json.getInt("ev")
        if (json.has("focus")) focus = json.getString("focus")
        if (json.has("focus_distance")) focusDistance = json.getDouble("focus_distance").toFloat()
        if (json.has("white_balance")) whiteBalance = json.getString("white_balance")
        if (json.has("torch")) torch = json.getBoolean("torch")
        if (json.has("stabilization")) stabilization = json.getBoolean("stabilization")
        if (json.has("ae_lock")) aeLock = json.getBoolean("ae_lock")
        if (json.has("awb_lock")) awbLock = json.getBoolean("awb_lock")
    }

    fun apply(b: CaptureRequest.Builder, chars: CameraCharacteristics) {
        b.set(CaptureRequest.CONTROL_MODE, CameraMetadata.CONTROL_MODE_AUTO)
        // Быстрые режимы обработки: HIGH_QUALITY добавляет кадры задержки в конвейере камеры.
        b.set(CaptureRequest.NOISE_REDUCTION_MODE, CameraMetadata.NOISE_REDUCTION_MODE_FAST)
        b.set(CaptureRequest.EDGE_MODE, CameraMetadata.EDGE_MODE_FAST)
        b.set(CaptureRequest.CONTROL_AE_MODE, CameraMetadata.CONTROL_AE_MODE_ON)
        b.set(CaptureRequest.CONTROL_AE_LOCK, aeLock)
        chars.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)?.let {
            b.set(CaptureRequest.CONTROL_AE_EXPOSURE_COMPENSATION, ev.coerceIn(it.lower, it.upper))
        }
        zoom?.let { z ->
            val range = chars.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)
            b.set(CaptureRequest.CONTROL_ZOOM_RATIO, range?.clamp(z) ?: z)
        }

        val afModes = chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf()
        if (focus == "manual" && CameraMetadata.CONTROL_AF_MODE_OFF in afModes) {
            b.set(CaptureRequest.CONTROL_AF_MODE, CameraMetadata.CONTROL_AF_MODE_OFF)
            b.set(CaptureRequest.LENS_FOCUS_DISTANCE, focusDistance)
        } else if (CameraMetadata.CONTROL_AF_MODE_CONTINUOUS_VIDEO in afModes) {
            b.set(CaptureRequest.CONTROL_AF_MODE, CameraMetadata.CONTROL_AF_MODE_CONTINUOUS_VIDEO)
        }

        b.set(
            CaptureRequest.CONTROL_AWB_MODE,
            when (whiteBalance) {
                "incandescent" -> CameraMetadata.CONTROL_AWB_MODE_INCANDESCENT
                "fluorescent" -> CameraMetadata.CONTROL_AWB_MODE_FLUORESCENT
                "daylight" -> CameraMetadata.CONTROL_AWB_MODE_DAYLIGHT
                "cloudy" -> CameraMetadata.CONTROL_AWB_MODE_CLOUDY_DAYLIGHT
                else -> CameraMetadata.CONTROL_AWB_MODE_AUTO
            },
        )
        b.set(CaptureRequest.CONTROL_AWB_LOCK, awbLock)

        if (chars.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true) {
            b.set(CaptureRequest.FLASH_MODE, if (torch) CameraMetadata.FLASH_MODE_TORCH else CameraMetadata.FLASH_MODE_OFF)
        }

        // Режим PREVIEW_STABILIZATION рассчитан на малую задержку, в отличие от обычного ON.
        val eisModes = chars.get(CameraCharacteristics.CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES) ?: intArrayOf()
        val eisOn = if (Build.VERSION.SDK_INT >= 33 &&
            CameraMetadata.CONTROL_VIDEO_STABILIZATION_MODE_PREVIEW_STABILIZATION in eisModes
        ) {
            CameraMetadata.CONTROL_VIDEO_STABILIZATION_MODE_PREVIEW_STABILIZATION
        } else {
            CameraMetadata.CONTROL_VIDEO_STABILIZATION_MODE_ON
        }
        b.set(
            CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE,
            if (stabilization && eisOn in eisModes) eisOn else CameraMetadata.CONTROL_VIDEO_STABILIZATION_MODE_OFF,
        )
        val ois = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION) ?: intArrayOf()
        if (CameraMetadata.LENS_OPTICAL_STABILIZATION_MODE_ON in ois) {
            b.set(CaptureRequest.LENS_OPTICAL_STABILIZATION_MODE, CameraMetadata.LENS_OPTICAL_STABILIZATION_MODE_ON)
        }
    }
}
