package dev.termland.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import dev.termland.android.session.SessionActivity
import dev.termland.android.ui.HomeScreen
import dev.termland.android.ui.HomeViewModel
import dev.termland.android.ui.theme.TermlandTheme

/** Server profiles and resumable sessions. The viewer lives in [SessionActivity]. */
class MainActivity : ComponentActivity() {

    private val vm: HomeViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            TermlandTheme {
                HomeScreen(
                    vm = vm,
                    onOpenSession = { profile, sessionId, password ->
                        startActivity(SessionActivity.intent(this, profile, sessionId, password))
                    },
                )
            }
        }
    }
}
