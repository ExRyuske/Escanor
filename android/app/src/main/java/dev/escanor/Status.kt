package dev.escanor

import android.os.Handler
import android.os.Looper

/** Строка состояния для экрана приложения; слушатели вызываются на главном потоке. */
object Status {
    private val main = Handler(Looper.getMainLooper())
    private val listeners = mutableSetOf<(String) -> Unit>()

    @Volatile
    var text = "Остановлено"
        private set

    fun set(value: String) {
        text = value
        main.post { listeners.toList().forEach { it(value) } }
    }

    fun listen(listener: (String) -> Unit) {
        listeners += listener
        listener(text)
    }

    fun unlisten(listener: (String) -> Unit) {
        listeners -= listener
    }
}
