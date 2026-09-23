package dev.escanor

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.TypedValue
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.widget.Button
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import kotlin.random.Random

/**
 * Экран приложения: состояние трансляции, а если ПК прислал раскладку — макропад.
 * ПК запускает его командой `am start -n dev.escanor/.MainActivity --ez autostart true`.
 */
class MainActivity : Activity() {

    private lateinit var root: FrameLayout
    private lateinit var statusScreen: View
    private lateinit var statusView: TextView
    private lateinit var toggle: Button
    private var pendingStart = false

    private val statusListener: (String) -> Unit = { text ->
        statusView.text = text
        toggle.text = if (StreamService.running) "Остановить" else "Запустить"
    }
    private val padListener: (Macropad.Layout?) -> Unit = { showLayout(it) }
    private val stateListener: (Int, Int) -> Unit = { page, index ->
        if (page == Macropad.page) cells.getOrNull(index)?.let(::render)
    }

    /** Кнопки открытой страницы: при смене состояния обновляется только одна, без перестройки экрана. */
    private var cells: List<Cell?> = emptyList()

    /** Кнопки с клавишами, которые сейчас держит палец (номера на открытой странице). */
    private val held = mutableSetOf<Int>()

    private class Cell(val button: Macropad.Button, val view: FrameLayout, val image: ImageView, val label: TextView)

    private val density get() = resources.displayMetrics.density

    // --- AMOLED ---

    private val handler = Handler(Looper.getMainLooper())
    private var grid: View? = null
    private var dimAfterMs = 0L
    private var dimBrightness = 0.15f
    private var lastTouch = 0L
    private var dimmed = false

    /** Раз в минуту сетка сдвигается на пару пикселей, чтобы рамки и подписи не выжигались. */
    private val pixelShift = object : Runnable {
        override fun run() {
            val range = 3 * density
            grid?.translationX = Random.nextFloat() * 2 * range - range
            grid?.translationY = Random.nextFloat() * 2 * range - range
            handler.postDelayed(this, PIXEL_SHIFT_MS)
        }
    }

    /** Проверка бездействия: приглушает экран, если давно не касались. */
    private val idleCheck = object : Runnable {
        override fun run() {
            if (dimAfterMs > 0 && !dimmed && SystemClock.uptimeMillis() - lastTouch >= dimAfterMs) {
                setDimmed(true)
            }
            handler.postDelayed(this, 1000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        root = FrameLayout(this).apply { setBackgroundColor(BACKGROUND) }
        statusScreen = buildStatusScreen()
        setContentView(root)
        if (intent.getBooleanExtra("autostart", false)) startWithPermissions()
    }

    private fun buildStatusScreen(): View {
        statusView = TextView(this).apply {
            setTextColor(TEXT)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 18f)
            gravity = Gravity.CENTER
        }
        toggle = Button(this).apply {
            setOnClickListener {
                if (StreamService.running) StreamService.stop(this@MainActivity) else startWithPermissions()
            }
        }
        val title = TextView(this).apply {
            text = "Escanor"
            setTextColor(ACCENT)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 32f)
            gravity = Gravity.CENTER
        }
        val hint = TextView(this).apply {
            text = "Подключите телефон к ПК по USB и откройте Escanor на компьютере.\n" +
                "Экран телефона можно выключить — трансляция продолжится.\n" +
                "Макропад настраивается на ПК."
            setTextColor(MUTED)
            gravity = Gravity.CENTER
        }
        val pad = (24 * density).toInt()
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setPadding(pad, pad, pad, pad)
            addView(title)
            addView(statusView, LinearLayout.LayoutParams(-1, -2).apply { topMargin = pad })
            addView(toggle, LinearLayout.LayoutParams(-2, -2).apply { topMargin = pad })
            addView(hint, LinearLayout.LayoutParams(-1, -2).apply { topMargin = pad })
        }
    }

    private fun showLayout(layout: Macropad.Layout?) {
        releaseHeld()
        root.removeAllViews()
        cells = emptyList()
        grid = null
        val page = layout?.pages?.getOrNull(Macropad.page)
        dimAfterMs = (layout?.dimAfterSecs ?: 0) * 1000L
        dimBrightness = layout?.dimBrightness ?: dimBrightness
        setDimmed(false)
        lastTouch = SystemClock.uptimeMillis()
        handler.removeCallbacks(pixelShift)
        handler.removeCallbacks(idleCheck)

        if (layout == null || page == null || page.buttons.isEmpty()) {
            requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            window.insetsController?.show(WindowInsets.Type.systemBars())
            root.setBackgroundColor(BACKGROUND)
            root.addView(statusScreen)
            return
        }
        // Макропад на весь экран, экран не гаснет, ориентация — как задано на ПК.
        // Он всегда в AMOLED-режиме: чёрный фон не изнашивает пиксели и бережёт батарею.
        root.setBackgroundColor(Color.BLACK)
        requestedOrientation = layout.orientation
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.insetsController?.apply {
            hide(WindowInsets.Type.systemBars())
            systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        val gap = (8 * density).toInt()
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(gap, gap, gap, gap)
        }
        val built = MutableList<Cell?>(page.buttons.size) { null }
        for (row in 0 until layout.rows) {
            val line = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
            for (col in 0 until layout.columns) {
                val index = row * layout.columns + col
                val view = page.buttons.getOrNull(index)?.let { button ->
                    val cell = padButton(Macropad.page, index, button)
                    built[index] = cell
                    cell.view
                } ?: View(this)
                line.addView(view, LinearLayout.LayoutParams(0, -1, 1f).apply { setMargins(gap, gap, gap, gap) })
            }
            column.addView(line, LinearLayout.LayoutParams(-1, 0, 1f))
        }
        cells = built
        grid = column
        root.addView(column)
        handler.postDelayed(pixelShift, PIXEL_SHIFT_MS)
        handler.postDelayed(idleCheck, 1000)
    }

    private fun padButton(page: Int, index: Int, button: Macropad.Button): Cell {
        val cell = FrameLayout(this).apply {
            background = GradientDrawable().apply {
                cornerRadius = 18 * density
                setColor(AMOLED_FILL)
                setStroke(density.toInt().coerceAtLeast(1), AMOLED_STROKE)
            }
            clipToOutline = true
            isHapticFeedbackEnabled = true
        }
        val image = ImageView(this).apply { scaleType = ImageView.ScaleType.FIT_CENTER }
        val label = TextView(this).apply {
            setTextColor(AMOLED_TEXT)
            gravity = Gravity.CENTER
            maxLines = 2
        }
        cell.addView(image)
        cell.addView(label)
        val views = Cell(button, cell, image, label)
        render(views)
        cell.setOnTouchListener { v, event ->
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    v.performHapticFeedback(HapticFeedbackConstants.VIRTUAL_KEY)
                    v.animate().scaleX(0.94f).scaleY(0.94f).setDuration(60).start()
                    v.background.alpha = 160
                    when (button.kind) {
                        // Папки и «Назад» открываются сразу, без обращения к ПК.
                        "folder" -> Macropad.open(button.target)
                        "back" -> Macropad.layout?.pages?.getOrNull(page)?.parent?.let { Macropad.open(it) }
                        else -> {
                            held += index
                            Macropad.press(page, index, true)
                        }
                    }
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                    v.animate().scaleX(1f).scaleY(1f).setDuration(90).start()
                    v.background.alpha = 255
                    if (held.remove(index)) Macropad.press(page, index, false)
                }
            }
            true
        }
        return views
    }

    /** Перед сменой страницы отпускаем удерживаемые кнопки: их «отпускание» иначе потерялось бы. */
    private fun releaseHeld() {
        for (index in held) Macropad.press(Macropad.page, index, false)
        held.clear()
    }

    /** Показывает текущее состояние кнопки: картинку и подпись, у папки — значок. */
    private fun render(cell: Cell) {
        val state = cell.button.current
        val bitmap = state?.image
        val text = when {
            cell.button.kind == "back" -> "←  " + (state?.label?.removePrefix("← ") ?: "Назад")
            cell.button.kind == "folder" && bitmap == null && state?.label.isNullOrEmpty() -> "▤  Папка"
            cell.button.kind == "folder" && bitmap == null -> "▤  " + state?.label
            else -> state?.label.orEmpty()
        }
        val inset = (12 * density).toInt()
        cell.image.setImageBitmap(bitmap)
        cell.image.visibility = if (bitmap != null) View.VISIBLE else View.GONE
        cell.image.layoutParams = FrameLayout.LayoutParams(-1, -1).apply {
            setMargins(inset, inset, inset, if (text.isEmpty()) inset else inset * 3)
        }
        cell.label.text = text
        cell.label.visibility = if (text.isNotEmpty()) View.VISIBLE else View.GONE
        cell.label.setTextSize(TypedValue.COMPLEX_UNIT_SP, if (bitmap == null) 20f else 13f)
        cell.label.layoutParams = FrameLayout.LayoutParams(-1, if (bitmap == null) -1 else -2).apply {
            gravity = Gravity.BOTTOM
            bottomMargin = inset / 2
        }
    }

    /**
     * Затемнение для AMOLED: минимальная яркость и приглушённая сетка. Касание возвращает
     * яркость и сразу срабатывает как обычное нажатие.
     */
    private fun setDimmed(value: Boolean) {
        if (dimmed == value) return
        dimmed = value
        window.attributes = window.attributes.apply {
            screenBrightness = if (value) dimBrightness else WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE
        }
        grid?.alpha = if (value) DIM_ALPHA else 1f
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        lastTouch = SystemClock.uptimeMillis()
        setDimmed(false)
        return super.dispatchTouchEvent(event)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (intent.getBooleanExtra("autostart", false)) startWithPermissions()
    }

    override fun onStart() {
        super.onStart()
        Status.listen(statusListener)
        Macropad.listen(padListener, stateListener)
    }

    override fun onStop() {
        releaseHeld()
        handler.removeCallbacks(pixelShift)
        handler.removeCallbacks(idleCheck)
        Macropad.unlisten(padListener, stateListener)
        Status.unlisten(statusListener)
        super.onStop()
    }

    private fun startWithPermissions() {
        val missing = PERMISSIONS.filter { checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
        if (missing.isEmpty()) {
            StreamService.start(this)
        } else {
            pendingStart = true
            requestPermissions(missing.toTypedArray(), 1)
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (!pendingStart) return
        pendingStart = false
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            StreamService.start(this)
        } else {
            Status.set("Без доступа к камере работа невозможна")
        }
    }

    companion object {
        private val PERMISSIONS = listOf(
            Manifest.permission.CAMERA,
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.POST_NOTIFICATIONS,
        )
        private const val PIXEL_SHIFT_MS = 60_000L

        // Сетка в затемнении приглушается, но остаётся читаемой; яркость экрана задаётся на ПК.
        private const val DIM_ALPHA = 0.6f

        // Палитра YeruVerse и YeruNeko.
        private val BACKGROUND = Color.rgb(0x0F, 0x11, 0x15)
        private val ACCENT = Color.rgb(0x8B, 0x5C, 0xF6)
        private val TEXT = Color.rgb(0xE8, 0xEA, 0xED)
        private val MUTED = Color.rgb(0x8B, 0x93, 0xA1)

        // AMOLED: чёрные пиксели не светятся — не изнашиваются и не тратят батарею.
        private val AMOLED_FILL = Color.BLACK
        private val AMOLED_STROKE = Color.rgb(0x1C, 0x1C, 0x22)
        private val AMOLED_TEXT = Color.rgb(0xB8, 0xBC, 0xC4)
    }
}
