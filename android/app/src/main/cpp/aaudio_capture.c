// Захват звука через AAudio в режиме минимальной задержки (на Pixel — MMAP, эксклюзивный доступ).
// Kotlin читает данные блокирующим read() в свой поток, поэтому колбэки AAudio не нужны.

#include <aaudio/AAudio.h>
#include <jni.h>
#include <stdint.h>
#include <time.h>

static AAudioStream *stream_of(jlong handle) { return (AAudioStream *)(intptr_t)handle; }

static aaudio_result_t open_stream(AAudioStreamBuilder *b, aaudio_sharing_mode_t sharing, AAudioStream **out) {
    AAudioStreamBuilder_setSharingMode(b, sharing);
    return AAudioStreamBuilder_openStream(b, out);
}

JNIEXPORT jlong JNICALL Java_dev_escanor_NativeAudio_open(JNIEnv *env, jclass cls, jint device_id, jint preset,
                                                        jint channels, jint sample_rate) {
    AAudioStreamBuilder *b = NULL;
    if (AAudio_createStreamBuilder(&b) != AAUDIO_OK) return 0;
    AAudioStreamBuilder_setDirection(b, AAUDIO_DIRECTION_INPUT);
    if (device_id > 0) AAudioStreamBuilder_setDeviceId(b, device_id);
    AAudioStreamBuilder_setInputPreset(b, preset);
    AAudioStreamBuilder_setChannelCount(b, channels);
    AAudioStreamBuilder_setSampleRate(b, sample_rate);
    AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);

    AAudioStream *s = NULL;
    // Эксклюзивный режим даёт MMAP без микшера; если микрофон занят — общий.
    if (open_stream(b, AAUDIO_SHARING_MODE_EXCLUSIVE, &s) != AAUDIO_OK) {
        s = NULL;
        if (open_stream(b, AAUDIO_SHARING_MODE_SHARED, &s) != AAUDIO_OK) s = NULL;
    }
    AAudioStreamBuilder_delete(b);
    if (s == NULL) return 0;
    if (AAudioStream_requestStart(s) != AAUDIO_OK) {
        AAudioStream_close(s);
        return 0;
    }
    return (jlong)(intptr_t)s;
}

// Читает `frames` кадров в direct ByteBuffer. Возвращает число кадров или отрицательный код ошибки.
JNIEXPORT jint JNICALL Java_dev_escanor_NativeAudio_read(JNIEnv *env, jclass cls, jlong handle, jobject buffer,
                                                       jint frames, jlong timeout_nanos) {
    void *data = (*env)->GetDirectBufferAddress(env, buffer);
    if (data == NULL) return AAUDIO_ERROR_ILLEGAL_ARGUMENT;
    return AAudioStream_read(stream_of(handle), data, frames, timeout_nanos);
}

// Время (CLOCK_MONOTONIC, нс), когда был записан кадр с номером `frame`; -1, если неизвестно.
JNIEXPORT jlong JNICALL Java_dev_escanor_NativeAudio_frameTime(JNIEnv *env, jclass cls, jlong handle, jlong frame) {
    AAudioStream *s = stream_of(handle);
    int64_t position = 0, time = 0;
    if (AAudioStream_getTimestamp(s, CLOCK_MONOTONIC, &position, &time) != AAUDIO_OK) return -1;
    return time + (frame - position) * 1000000000LL / AAudioStream_getSampleRate(s);
}

JNIEXPORT jint JNICALL Java_dev_escanor_NativeAudio_sampleRate(JNIEnv *env, jclass cls, jlong handle) {
    return AAudioStream_getSampleRate(stream_of(handle));
}

JNIEXPORT jint JNICALL Java_dev_escanor_NativeAudio_channelCount(JNIEnv *env, jclass cls, jlong handle) {
    return AAudioStream_getChannelCount(stream_of(handle));
}

JNIEXPORT jboolean JNICALL Java_dev_escanor_NativeAudio_isExclusive(JNIEnv *env, jclass cls, jlong handle) {
    return AAudioStream_getSharingMode(stream_of(handle)) == AAUDIO_SHARING_MODE_EXCLUSIVE;
}

JNIEXPORT jint JNICALL Java_dev_escanor_NativeAudio_deviceId(JNIEnv *env, jclass cls, jlong handle) {
    return AAudioStream_getDeviceId(stream_of(handle));
}

JNIEXPORT void JNICALL Java_dev_escanor_NativeAudio_close(JNIEnv *env, jclass cls, jlong handle) {
    AAudioStream *s = stream_of(handle);
    AAudioStream_requestStop(s);
    AAudioStream_close(s);
}
