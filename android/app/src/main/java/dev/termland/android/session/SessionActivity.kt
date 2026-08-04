package dev.termland.android.session

import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.viewModels
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.lifecycle.lifecycleScope
import dev.termland.android.data.HostProfile
import dev.termland.android.input.RemoteInputConnection
import dev.termland.android.ui.theme.TermlandTheme
import kotlinx.coroutines.launch
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

/**
 * The viewer. One SurfaceView of remote pixels, one Compose overlay, and every
 * input event we can get our hands on forwarded to the remote.
 */
class SessionActivity : ComponentActivity() {

    private val vm: SessionViewModel by viewModels()
    private lateinit var surfaceView: RemoteSurfaceView

    /** Set once the Surface exists; connecting before that has nowhere to decode to. */
    private var surfaceReady = false
    private var connectedOnce = false

    private var overlayVisible by mutableStateOf(true)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val profileJson = intent.getStringExtra(EXTRA_PROFILE)
        val profile = profileJson?.let {
            runCatching { Json { ignoreUnknownKeys = true }.decodeFromString<HostProfile>(it) }.getOrNull()
        }
        if (profile == null) {
            finish()
            return
        }
        vm.bind(
            profile = profile,
            sessionId = intent.getStringExtra(EXTRA_SESSION_ID),
            password = intent.getStringExtra(EXTRA_PASSWORD),
        )

        // A remote desktop must not have the system dim or lock the screen mid-use.
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        goFullscreen()

        surfaceView = RemoteSurfaceView(this, vm.router)
        surfaceView.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) {
                surfaceReady = true
                vm.attachDecoder(surfaceView)
                if (!connectedOnce) {
                    connectedOnce = true
                    connect(resume = intent.getStringExtra(EXTRA_SESSION_ID) != null)
                }
            }

            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
                vm.router.viewWidth = width
                vm.router.viewHeight = height
                // Rotation / split-screen resize: tell the remote to match, so the
                // session is rendered at native resolution instead of scaled.
                vm.resize(width, height)
            }

            override fun surfaceDestroyed(holder: SurfaceHolder) {
                surfaceReady = false
                vm.releaseDecoder()
            }
        })

        setContent {
            TermlandTheme {
                SessionScreen(
                    vm = vm,
                    surfaceView = surfaceView,
                    overlayVisible = overlayVisible,
                    onToggleOverlay = { overlayVisible = !overlayVisible },
                    onResume = { connect(resume = true) },
                    onDetachAndExit = {
                        vm.detach("left the session")
                        finish()
                    },
                    onToggleKeyboard = {
                        if (surfaceView.hasWindowFocus()) RemoteInputConnection.show(surfaceView)
                    },
                    onTogglePointerCapture = { surfaceView.togglePointerCapture() },
                )
            }
        }
    }

    private fun connect(resume: Boolean) {
        if (!surfaceReady) return
        val w = surfaceView.width.takeIf { it > 0 } ?: window.decorView.width
        val h = surfaceView.height.takeIf { it > 0 } ?: window.decorView.height
        vm.connect(w, h, resume)
    }

    // -----------------------------------------------------------------------
    // Keyboard: the priority path
    // -----------------------------------------------------------------------

    /**
     * `dispatchKeyEvent` is the earliest app-level hook, which is the only place
     * combinations like Alt+Tab, Ctrl+Alt+F2 or Super can be claimed before the
     * framework or the focused Compose node interprets them. Consuming the event
     * here is what stops Android from eating them.
     *
     * We claim keys only from real keyboards. Anything else (nav bar Back, volume,
     * remote-control keys) falls through so the user can always leave.
     */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (::surfaceView.isInitialized && event.isFromRealKeyboard() &&
            vm.router.onKeyEvent(event)
        ) {
            return true
        }
        return super.dispatchKeyEvent(event)
    }

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        // Mouse motion/scroll must reach the surface even when a Compose node has
        // focus; hand it to the surface first and only fall through if unhandled.
        if (::surfaceView.isInitialized && surfaceView.onGenericMotionEvent(event)) return true
        return super.dispatchGenericMotionEvent(event)
    }

    private fun KeyEvent.isFromRealKeyboard(): Boolean {
        val dev = device ?: return false
        return dev.keyboardType == android.view.InputDevice.KEYBOARD_TYPE_ALPHABETIC && !dev.isVirtual
    }

    // -----------------------------------------------------------------------
    // Lifecycle: detach on background, offer resume on return
    // -----------------------------------------------------------------------

    override fun onStart() {
        super.onStart()
        // If we came back to a detached session, the overlay's Resume button is
        // already showing (state == Detached). Auto-resuming without asking would
        // surprise anyone who backgrounded the app deliberately.
        goFullscreen()
    }

    override fun onStop() {
        super.onStop()
        if (!::surfaceView.isInitialized) return
        // Backgrounded: DETACH. The remote session keeps running and the server
        // stops encoding to a client that cannot see it.
        surfaceView.releaseHeldButtons()
        if (vm.state.value is SessionState.Live) vm.detach("backgrounded")
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (!::surfaceView.isInitialized) return
        if (hasFocus) {
            goFullscreen()
            surfaceView.requestFocus()
            lifecycleScope.launch { vm.sendLocalClipboard() }
        } else {
            // Losing focus with modifiers held would leave Ctrl stuck on the remote.
            vm.router.releaseAll()
            surfaceView.releaseHeldButtons()
        }
    }

    private fun goFullscreen() {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowCompat.getInsetsController(window, window.decorView).apply {
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            hide(WindowInsetsCompat.Type.systemBars())
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            window.attributes = window.attributes.apply {
                layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        }
    }

    companion object {
        private const val EXTRA_PROFILE = "profile"
        private const val EXTRA_SESSION_ID = "session_id"
        private const val EXTRA_PASSWORD = "password"

        /** @param sessionId non-null to resume/attach, null to create a new session. */
        fun intent(
            context: Context,
            profile: HostProfile,
            sessionId: String?,
            password: String?,
        ): Intent = Intent(context, SessionActivity::class.java).apply {
            putExtra(EXTRA_PROFILE, Json.encodeToString(profile))
            sessionId?.let { putExtra(EXTRA_SESSION_ID, it) }
            password?.let { putExtra(EXTRA_PASSWORD, it) }
        }
    }
}
