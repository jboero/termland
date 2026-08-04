package dev.termland.android.data

import dev.termland.core.MobileCodec
import dev.termland.core.ServerProfile
import dev.termland.core.SessionParams
import kotlinx.serialization.Serializable
import java.util.UUID

/**
 * A saved server, plus the session defaults the user last chose for it.
 *
 * This is the app's own persistence model, deliberately separate from the core's
 * [ServerProfile] record: it carries UI-only fields (label, id, rememberPassword)
 * and survives contract changes on the Rust side.
 */
@Serializable
data class HostProfile(
    val id: String = UUID.randomUUID().toString(),
    val label: String = "",
    val host: String = "",
    val port: Int = DEFAULT_PORT,
    val useTls: Boolean = true,
    val acceptInvalidCerts: Boolean = false,
    val username: String = "",
    /**
     * Stored in clear in DataStore only when [rememberPassword] is set.
     *
     * TODO(M4): move this to an Android Keystore-wrapped blob, as
     * docs/mobile-clients.md calls for. Until then the honest default is
     * "don't persist it" and prompt at connect time.
     */
    val password: String = "",
    val rememberPassword: Boolean = false,
    // --- session defaults ---
    val quality: Int = 75,
    val audio: Boolean = false,
    val desktopShell: String = "",
    val appCommand: String = "",
) {
    val displayName: String get() = label.ifBlank { host.ifBlank { "New server" } }

    val subtitle: String
        get() = buildString {
            if (username.isNotBlank()) append(username).append('@')
            append(host.ifBlank { "?" }).append(':').append(port)
            append(if (useTls) "  TLS" else "  plain")
            if (useTls && acceptInvalidCerts) append(" (unverified)")
        }

    companion object {
        const val DEFAULT_PORT = 7867
    }
}

/**
 * @param passwordOverride a password typed at connect time when the profile does
 *        not persist one.
 */
fun HostProfile.toServerProfile(passwordOverride: String? = null): ServerProfile = ServerProfile(
    host = host.trim(),
    port = port.toUShort(),
    useTls = useTls,
    acceptInvalidCerts = acceptInvalidCerts,
    username = username.trim().ifBlank { null },
    password = (passwordOverride ?: password).ifBlank { null },
)

/**
 * Build [SessionParams] for this profile at a concrete surface size.
 *
 * [supportedCodecs] must be the *probed* decoder set — see
 * [dev.termland.android.net.CodecSupport]. Advertising codecs the device cannot
 * decode is the one way to break the server-side negotiation.
 */
fun HostProfile.toSessionParams(
    width: Int,
    height: Int,
    supportedCodecs: List<MobileCodec>,
): SessionParams = SessionParams(
    width = width.toUInt(),
    height = height.toUInt(),
    quality = quality.coerceIn(1, 100).toUByte(),
    audio = audio,
    desktopShell = desktopShell.trim().ifBlank { null },
    appCommand = appCommand.trim().ifBlank { null },
    supportedCodecs = supportedCodecs,
)
