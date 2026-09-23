package dev.escanor

import android.content.Context
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import org.json.JSONArray
import org.json.JSONObject
import kotlin.math.roundToInt
import kotlin.math.sqrt

/** Перечисление камер и микрофонов телефона для ответа на get_devices. */
object DeviceInfo {

    private const val MAX_PIXELS = 3840 * 2160
    private const val MIN_WIDTH = 640

    fun phone(): JSONObject = JSONObject()
        .put("manufacturer", Build.MANUFACTURER)
        .put("model", Build.MODEL)

    fun devices(context: Context): JSONObject = JSONObject()
        .put("cameras", cameras(context))
        .put("microphones", microphones(context))
        .put("audio_sources", audioSources(context))

    // --- Камеры ---

    fun cameras(context: Context): JSONArray {
        val manager = context.getSystemService(CameraManager::class.java)
        val encoder = hardwareEncoder(MediaFormat.MIMETYPE_VIDEO_AVC)
        val result = JSONArray()
        for (id in manager.cameraIdList) {
            val chars = manager.getCameraCharacteristics(id)
            result.put(describe(id, null, chars, chars, encoder))
            // Логическая мультикамера: отдельные объективы доступны как физические камеры.
            val caps = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES) ?: intArrayOf()
            if (caps.contains(CameraMetadata.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA)) {
                for (physicalId in chars.physicalCameraIds.sorted()) {
                    val physical = manager.getCameraCharacteristics(physicalId)
                    result.put(describe(id, physicalId, chars, physical, encoder))
                }
            }
        }
        return result
    }

    private fun describe(
        id: String,
        physicalId: String?,
        logical: CameraCharacteristics,
        chars: CameraCharacteristics,
        encoder: MediaCodecInfo.VideoCapabilities?,
    ): JSONObject {
        val facing = when (chars.get(CameraCharacteristics.LENS_FACING)) {
            CameraMetadata.LENS_FACING_FRONT -> "front"
            CameraMetadata.LENS_FACING_BACK -> "back"
            else -> "external"
        }
        val focal = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS)?.firstOrNull()
        val sensor = chars.get(CameraCharacteristics.SENSOR_INFO_PHYSICAL_SIZE)
        val equivalent = if (focal != null && sensor != null) {
            val diagonal = sqrt(sensor.width * sensor.width + sensor.height * sensor.height)
            (focal * 43.27f / diagonal).roundToInt()
        } else null

        val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
        val sizes = JSONArray()
        map?.getOutputSizes(MediaCodec::class.java)
            ?.filter { it.width >= MIN_WIDTH && it.width * it.height <= MAX_PIXELS }
            ?.filter { encoder == null || encoder.isSizeSupported(it.width, it.height) }
            ?.sortedByDescending { it.width * it.height }
            ?.forEach { size ->
                val minFrameNs = map.getOutputMinFrameDuration(MediaCodec::class.java, size)
                val maxFps = if (minFrameNs > 0) (1_000_000_000.0 / minFrameNs).roundToInt() else 30
                sizes.put(JSONArray().put(size.width).put(size.height).put(maxFps))
            }

        // Диапазоны FPS и управление задаются запросом к логической камере.
        val fps = JSONArray()
        logical.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)
            ?.map { it.upper }?.distinct()?.sorted()
            ?.forEach { fps.put(it) }

        val zoom = logical.get(CameraCharacteristics.CONTROL_ZOOM_RATIO_RANGE)
        val ev = logical.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_RANGE)
        val evStep = logical.get(CameraCharacteristics.CONTROL_AE_COMPENSATION_STEP)
        val afModes = chars.get(CameraCharacteristics.CONTROL_AF_AVAILABLE_MODES) ?: intArrayOf()
        val eis = logical.get(CameraCharacteristics.CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES) ?: intArrayOf()

        return JSONObject()
            .put("id", id)
            .put("physical_id", physicalId ?: JSONObject.NULL)
            .put("name", cameraName(facing, physicalId, equivalent))
            .put("facing", facing)
            .put("sizes", sizes)
            .put("fps", fps)
            .put("zoom", JSONArray().put(zoom?.lower?.toDouble() ?: 1.0).put(zoom?.upper?.toDouble() ?: 1.0))
            .put("ev", JSONArray().put(ev?.lower ?: 0).put(ev?.upper ?: 0))
            .put("ev_step", evStep?.toDouble() ?: 0.0)
            .put("manual_focus", afModes.contains(CameraMetadata.CONTROL_AF_MODE_OFF))
            .put("min_focus_distance", chars.get(CameraCharacteristics.LENS_INFO_MINIMUM_FOCUS_DISTANCE)?.toDouble() ?: 0.0)
            .put("flash", chars.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) == true)
            .put("eis", eis.contains(CameraMetadata.CONTROL_VIDEO_STABILIZATION_MODE_ON))
    }

    private fun cameraName(facing: String, physicalId: String?, equivalent: Int?): String {
        val side = when (facing) {
            "front" -> "Фронтальная"
            "back" -> "Задняя"
            else -> "Внешняя"
        }
        if (physicalId == null) return "$side камера"
        val kind = when {
            equivalent == null -> "объектив $physicalId"
            equivalent < 20 -> "сверхширокая"
            equivalent <= 40 -> "основная"
            else -> "телефото"
        }
        val mm = equivalent?.let { " · $it мм" } ?: ""
        return "$side $kind$mm"
    }

    /** Возможности аппаратного кодировщика (первого не программного) для mime. */
    fun hardwareEncoder(mime: String): MediaCodecInfo.VideoCapabilities? =
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
            .filter { it.isEncoder && it.isHardwareAccelerated && it.supportedTypes.any { t -> t.equals(mime, true) } }
            .firstOrNull()
            ?.getCapabilitiesForType(mime)
            ?.videoCapabilities

    // --- Микрофоны ---

    private val skippedInputTypes = setOf(
        AudioDeviceInfo.TYPE_TELEPHONY,
        AudioDeviceInfo.TYPE_REMOTE_SUBMIX,
        AudioDeviceInfo.TYPE_FM_TUNER,
        AudioDeviceInfo.TYPE_TV_TUNER,
        AudioDeviceInfo.TYPE_BUS,
    )

    fun microphones(context: Context): JSONArray {
        val manager = context.getSystemService(AudioManager::class.java)
        val result = JSONArray()
        for (device in manager.getDevices(AudioManager.GET_DEVICES_INPUTS)) {
            if (device.type in skippedInputTypes) continue
            result.put(JSONObject().put("id", device.id).put("name", micName(device)))
        }
        return result
    }

    private fun micName(device: AudioDeviceInfo): String {
        val base = when (device.type) {
            AudioDeviceInfo.TYPE_BUILTIN_MIC -> "Встроенный микрофон"
            AudioDeviceInfo.TYPE_WIRED_HEADSET -> "Проводная гарнитура"
            AudioDeviceInfo.TYPE_USB_DEVICE, AudioDeviceInfo.TYPE_USB_HEADSET, AudioDeviceInfo.TYPE_USB_ACCESSORY ->
                "USB: ${device.productName}"
            AudioDeviceInfo.TYPE_BLUETOOTH_SCO, AudioDeviceInfo.TYPE_BLE_HEADSET ->
                "Bluetooth: ${device.productName}"
            else -> device.productName.toString()
        }
        val position = when (device.address) {
            "bottom" -> " (нижний)"
            "back" -> " (задний)"
            "top" -> " (верхний)"
            "" -> ""
            else -> if (device.type == AudioDeviceInfo.TYPE_BUILTIN_MIC) " (${device.address})" else ""
        }
        return base + position
    }

    /** Источники звука влияют на обработку: шумоподавление, эхоподавление, стерео. */
    fun audioSources(context: Context): JSONArray {
        val manager = context.getSystemService(AudioManager::class.java)
        val result = JSONArray()
        fun add(id: String, name: String) = result.put(JSONObject().put("id", id).put("name", name))
        add("voice_performance", "Живой звук (минимальная задержка)")
        add("camcorder", "Видеосъёмка (стерео, мягкая обработка)")
        add("mic", "Обычный микрофон")
        add("voice_communication", "Звонки (эхо- и шумоподавление)")
        add("voice_recognition", "Без АРУ (для записи голоса)")
        if (manager.getProperty(AudioManager.PROPERTY_SUPPORT_AUDIO_SOURCE_UNPROCESSED) == "true") {
            add("unprocessed", "Без обработки")
        }
        return result
    }
}
