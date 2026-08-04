package dev.termland.android.session

import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowDownward
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Keyboard
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Mouse
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.termland.android.input.KeyMap
import java.util.Locale

/**
 * Video plus a translucent overlay.
 *
 * The overlay lives in the window's own surface, which SurfaceFlinger composites
 * *above* the SurfaceView's dedicated layer — so these Compose controls cost
 * nothing per video frame. (This only works because the SurfaceView keeps its
 * default "below the window" z-order; setZOrderOnTop would put the video on top
 * and hide all of this.)
 */
@Composable
fun SessionScreen(
    vm: SessionViewModel,
    surfaceView: RemoteSurfaceView,
    overlayVisible: Boolean,
    onToggleOverlay: () -> Unit,
    onResume: () -> Unit,
    onDetachAndExit: () -> Unit,
    onToggleKeyboard: () -> Unit,
    onTogglePointerCapture: () -> Boolean,
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val dataRate by vm.dataRate.collectAsStateWithLifecycle()

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        AndroidView(factory = { surfaceView }, modifier = Modifier.fillMaxSize())

        when (val s = state) {
            is SessionState.Connecting -> CenteredStatus("Connecting…")
            is SessionState.Detached -> DetachedCard(s.reason, onResume, onDetachAndExit)
            is SessionState.Failed -> FailedCard(s.message, onResume, onDetachAndExit)
            else -> Unit
        }

        val live = state as? SessionState.Live

        if (overlayVisible && live != null) {
            ModifierBar(
                vm = vm,
                onToggleKeyboard = onToggleKeyboard,
                onTogglePointerCapture = onTogglePointerCapture,
                onHide = onToggleOverlay,
                modifier = Modifier.align(Alignment.BottomCenter).systemBarsPadding(),
            )

            Text(
                text = "${live.width}×${live.height}  ${live.codec}  ${formatRate(dataRate)}",
                color = Color.White.copy(alpha = 0.7f),
                style = MaterialTheme.typography.labelSmall,
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .systemBarsPadding()
                    .padding(8.dp)
                    .background(Color.Black.copy(alpha = 0.4f))
                    .padding(horizontal = 6.dp, vertical = 2.dp),
            )
        } else {
            // Always-available handle to bring the bar back, mirroring the desktop
            // client's translucent keyboard button.
            IconButton(
                onClick = onToggleOverlay,
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .systemBarsPadding()
                    .padding(8.dp)
                    .size(40.dp)
                    .background(Color.Black.copy(alpha = 0.45f), MaterialTheme.shapes.small),
            ) {
                Icon(Icons.Filled.Tune, contentDescription = "Show controls", tint = Color.White)
            }
        }
    }
}

/**
 * Sticky modifier bar. Phones and most tablet keyboard cases are missing at least
 * one of Ctrl/Alt/Super/Esc, and a remote Linux desktop is unusable without them
 * (docs/mobile-clients.md, "on-screen modifier bar").
 *
 * A latched toggle sends a *real* key press to the remote and holds it, rather
 * than only decorating the modifier bitfield, so remote-side shortcut detection
 * behaves exactly as it would for a physically held key.
 */
@Composable
private fun ModifierBar(
    vm: SessionViewModel,
    onToggleKeyboard: () -> Unit,
    onTogglePointerCapture: () -> Boolean,
    onHide: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // The router is the source of truth but is mutated from the input thread, so
    // the bar keeps its own Compose-visible mirror of the latched set.
    var latched by remember { mutableIntStateOf(0) }
    var captured by remember { mutableStateOf(false) }

    fun toggle(scancode: Int) {
        val bit = KeyMap.modifierBitForScancode(scancode)
        val wantLatched = latched and bit == 0
        vm.router.setStickyModifier(scancode, wantLatched)
        latched = if (wantLatched) latched or bit else latched and bit.inv()
    }

    /** Any real key consumes the latched modifiers (InputRouter releases them). */
    fun tap(scancode: Int, keysym: Int) {
        vm.router.tapKey(scancode, keysym)
        latched = 0
    }

    Surface(
        color = Color.Black.copy(alpha = 0.55f),
        contentColor = Color.White,
        modifier = modifier.fillMaxWidth(),
    ) {
        Row(
            Modifier
                .horizontalScroll(rememberScrollState())
                .padding(horizontal = 6.dp, vertical = 4.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            StickyChip("Ctrl", latched and KeyMap.MOD_CTRL != 0) { toggle(KeyMap.KEY_LEFTCTRL) }
            StickyChip("Alt", latched and KeyMap.MOD_ALT != 0) { toggle(KeyMap.KEY_LEFTALT) }
            StickyChip("Shift", latched and KeyMap.MOD_SHIFT != 0) { toggle(KeyMap.KEY_LEFTSHIFT) }
            StickyChip("Super", latched and KeyMap.MOD_SUPER != 0) { toggle(KeyMap.KEY_LEFTMETA) }

            TapChip("Esc") { tap(KeyMap.KEY_ESC, 0xFF1B) }
            TapChip("Tab") { tap(KeyMap.KEY_TAB, 0xFF09) }

            IconButton(onClick = { tap(KeyMap.KEY_LEFT, 0xFF51) }) {
                Icon(Icons.AutoMirrored.Filled.KeyboardArrowLeft, "Left arrow")
            }
            IconButton(onClick = { tap(KeyMap.KEY_DOWN, 0xFF54) }) {
                Icon(Icons.Filled.ArrowDownward, "Down arrow")
            }
            IconButton(onClick = { tap(KeyMap.KEY_UP, 0xFF52) }) {
                Icon(Icons.Filled.ArrowUpward, "Up arrow")
            }
            IconButton(onClick = { tap(KeyMap.KEY_RIGHT, 0xFF53) }) {
                Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, "Right arrow")
            }

            IconButton(onClick = onToggleKeyboard) {
                Icon(Icons.Filled.Keyboard, "Soft keyboard")
            }
            IconButton(onClick = { captured = onTogglePointerCapture() }) {
                Icon(
                    Icons.Filled.Mouse,
                    contentDescription = "Toggle trackpad (relative pointer) mode",
                    tint = if (captured) MaterialTheme.colorScheme.primary else Color.White,
                )
            }
            IconButton(onClick = onHide) {
                Icon(Icons.Filled.Close, "Hide controls")
            }
        }
    }
}

@Composable
private fun StickyChip(label: String, selected: Boolean, onClick: () -> Unit) {
    FilterChip(selected = selected, onClick = onClick, label = { Text(label) })
}

@Composable
private fun TapChip(label: String, onTap: () -> Unit) {
    OutlinedButton(
        onClick = onTap,
        contentPadding = PaddingValues(horizontal = 10.dp, vertical = 4.dp),
    ) { Text(label) }
}

@Composable
private fun CenteredStatus(text: String) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            CircularProgressIndicator(color = Color.White)
            Text(text, color = Color.White, modifier = Modifier.padding(top = 12.dp))
        }
    }
}

/**
 * The persistence payoff: a dropped link or a backgrounded app leaves the remote
 * session running, so the only thing the user has to do is tap Resume.
 */
@Composable
private fun DetachedCard(reason: String, onResume: () -> Unit, onExit: () -> Unit) {
    Scrim {
        Text("Detached", color = Color.White, style = MaterialTheme.typography.headlineSmall)
        Text(
            "The remote session is still running.\n$reason",
            color = Color.White.copy(alpha = 0.8f),
            textAlign = TextAlign.Center,
        )
        Button(onClick = onResume) { Text("Resume session") }
        OutlinedButton(onClick = onExit) { Text("Leave") }
    }
}

@Composable
private fun FailedCard(message: String, onRetry: () -> Unit, onExit: () -> Unit) {
    Scrim {
        Text("Connection failed", color = Color.White, style = MaterialTheme.typography.headlineSmall)
        Text(message, color = Color.White.copy(alpha = 0.8f), textAlign = TextAlign.Center)
        Button(onClick = onRetry) { Text("Retry") }
        OutlinedButton(onClick = onExit) { Text("Back") }
    }
}

@Composable
private fun Scrim(content: @Composable () -> Unit) {
    Box(
        Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.78f)),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
            modifier = Modifier.padding(24.dp),
        ) { content() }
    }
}

private fun formatRate(bytesPerSec: Long): String = when {
    bytesPerSec <= 0 -> "—"
    bytesPerSec < 1024 -> "$bytesPerSec B/s"
    bytesPerSec < 1024 * 1024 -> "${bytesPerSec / 1024} KB/s"
    else -> String.format(Locale.US, "%.1f MB/s", bytesPerSec / 1048576.0)
}
