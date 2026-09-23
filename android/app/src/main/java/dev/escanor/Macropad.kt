package dev.escanor

import android.content.pm.ActivityInfo
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.Handler
import android.os.Looper
import android.util.Base64
import org.json.JSONObject

/**
 * Раскладка макропада, присланная с ПК: страницы (корневая и папки) с кнопками.
 * Между страницами телефон ходит сам — папка открывается мгновенно, без обращения к ПК.
 * Экран подписывается на изменения, сессия — на нажатия. Слушатели вызываются на главном потоке.
 */
object Macropad {
    class State(val label: String, val image: Bitmap?)

    /** `kind`: "keys" — клавиши на ПК, "folder" — открыть страницу `target`, "back" — назад. */
    class Button(val kind: String, val target: Int, val states: List<State>, @Volatile var state: Int) {
        val current: State? get() = states.getOrNull(state) ?: states.firstOrNull()
    }

    class Page(val parent: Int, val buttons: List<Button>)

    /** `orientation` — значение ActivityInfo.SCREEN_ORIENTATION_*. */
    class Layout(
        val columns: Int,
        val rows: Int,
        val orientation: Int,
        val dimAfterSecs: Int,
        /** Яркость экрана в затемнении, 0..1. */
        val dimBrightness: Float,
        val pages: List<Page>,
    )

    private val main = Handler(Looper.getMainLooper())
    private val listeners = mutableSetOf<(Layout?) -> Unit>()
    private val stateListeners = mutableSetOf<(Int, Int) -> Unit>()

    @Volatile
    var layout: Layout? = null
        private set

    /** Открытая страница. */
    @Volatile
    var page = 0
        private set

    /** Отправка нажатия (true) и отпускания (false) на ПК: страница, кнопка; задаёт активная сессия. */
    @Volatile
    var onPress: ((Int, Int, Boolean) -> Unit)? = null

    fun setLayout(json: JSONObject) {
        val pages = json.getJSONArray("pages")
        val list = (0 until pages.length()).map { p ->
            val page = pages.getJSONObject(p)
            val buttons = page.getJSONArray("buttons")
            Page(
                if (page.isNull("parent")) -1 else page.getInt("parent"),
                (0 until buttons.length()).map { i ->
                    val b = buttons.getJSONObject(i)
                    val states = b.getJSONArray("states")
                    Button(
                        b.getString("kind"),
                        if (b.isNull("target")) -1 else b.optInt("target", -1),
                        (0 until states.length()).map { j ->
                            val s = states.getJSONObject(j)
                            State(s.optString("label"), decode(s.optString("image")))
                        },
                        b.optInt("state"),
                    )
                },
            )
        }
        val amoled = json.getJSONObject("amoled")
        val next = Layout(
            json.getInt("columns"),
            json.getInt("rows"),
            orientation(json.optString("orientation")),
            amoled.getInt("dim_after_secs"),
            amoled.getInt("dim_brightness").coerceIn(1, 100) / 100f,
            list,
        )
        // Открытая папка остаётся открытой, пока она существует.
        if (page >= list.size) page = 0
        layout = next
        publish()
    }

    /** Смена состояния одной кнопки — без перестройки всего экрана. */
    fun setState(page: Int, id: Int, state: Int) {
        val button = layout?.pages?.getOrNull(page)?.buttons?.getOrNull(id) ?: return
        button.state = state
        main.post { stateListeners.toList().forEach { it(page, id) } }
    }

    /** Открыть страницу (папку или родительскую). */
    fun open(page: Int) {
        if (layout?.pages?.getOrNull(page) == null) return
        this.page = page
        publish()
    }

    fun clear() {
        layout = null
        page = 0
        publish()
    }

    fun press(page: Int, index: Int, down: Boolean) {
        onPress?.invoke(page, index, down)
    }

    private fun orientation(name: String) = when (name) {
        "portrait" -> ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        "landscape" -> ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
        "reverse_portrait" -> ActivityInfo.SCREEN_ORIENTATION_REVERSE_PORTRAIT
        "reverse_landscape" -> ActivityInfo.SCREEN_ORIENTATION_REVERSE_LANDSCAPE
        else -> ActivityInfo.SCREEN_ORIENTATION_FULL_SENSOR
    }

    private fun decode(base64: String): Bitmap? {
        if (base64.isEmpty() || base64 == "null") return null
        val bytes = Base64.decode(base64, Base64.DEFAULT)
        return BitmapFactory.decodeByteArray(bytes, 0, bytes.size)
    }

    private fun publish() {
        val value = layout
        main.post { listeners.toList().forEach { it(value) } }
    }

    fun listen(listener: (Layout?) -> Unit, onState: (Int, Int) -> Unit) {
        listeners += listener
        stateListeners += onState
        listener(layout)
    }

    fun unlisten(listener: (Layout?) -> Unit, onState: (Int, Int) -> Unit) {
        listeners -= listener
        stateListeners -= onState
    }
}
