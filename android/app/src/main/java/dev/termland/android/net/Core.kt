package dev.termland.android.net

import dev.termland.core.TermlandClient

/**
 * Process-wide handle on the Rust core.
 *
 * One client for the whole app: the session-list screen and the viewer activity
 * must share it so that `disconnect()` (which DETACHES, leaving the remote
 * session alive) and a later `attach()` act on the same connection state across
 * an activity hand-off or a configuration change.
 */
object Core {
    val client: TermlandClient by lazy { TermlandClient() }

    /** Human-readable reason for a thrown core error, safe for any contract shape. */
    fun describe(t: Throwable): String =
        t.message?.takeIf { it.isNotBlank() } ?: t::class.simpleName ?: "unknown error"
}
