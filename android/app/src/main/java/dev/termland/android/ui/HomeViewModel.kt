package dev.termland.android.ui

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.termland.android.data.HostProfile
import dev.termland.android.data.ProfileStore
import dev.termland.android.data.toServerProfile
import dev.termland.android.net.Core
import dev.termland.core.SessionSummary
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Resumable sessions on one server. */
sealed interface SessionListState {
    data object Idle : SessionListState
    data object Loading : SessionListState
    data class Loaded(val sessions: List<SessionSummary>) : SessionListState
    data class Error(val message: String) : SessionListState
}

class HomeViewModel(app: Application) : AndroidViewModel(app) {

    private val store = ProfileStore(app)

    val profiles: StateFlow<List<HostProfile>> = store.profiles
        .stateIn(viewModelScope, SharingStarted.Eagerly, emptyList())

    private val _sessions = MutableStateFlow<SessionListState>(SessionListState.Idle)
    val sessions: StateFlow<SessionListState> = _sessions.asStateFlow()

    /**
     * Password typed for a profile that does not persist one. Kept in memory only,
     * for the lifetime of this screen.
     */
    private val transientPasswords = mutableMapOf<String, String>()

    fun passwordFor(profile: HostProfile): String? =
        profile.password.takeIf { it.isNotBlank() }
            ?: transientPasswords[profile.id]?.takeIf { it.isNotBlank() }

    fun rememberTransientPassword(profileId: String, password: String) {
        if (password.isBlank()) transientPasswords.remove(profileId)
        else transientPasswords[profileId] = password
    }

    fun save(profile: HostProfile) = viewModelScope.launch { store.upsert(profile) }
    fun delete(id: String) = viewModelScope.launch { store.delete(id) }

    /** `list_sessions` is a blocking control-plane call — keep it off the main thread. */
    fun refreshSessions(profile: HostProfile) {
        _sessions.value = SessionListState.Loading
        viewModelScope.launch {
            _sessions.value = withContext(Dispatchers.IO) {
                try {
                    val list = Core.client.listSessions(profile.toServerProfile(passwordFor(profile)))
                    SessionListState.Loaded(list)
                } catch (e: Exception) {
                    SessionListState.Error(Core.describe(e))
                }
            }
            store.setLastUsed(profile.id)
        }
    }

    fun closeSession(profile: HostProfile, sessionId: String) {
        viewModelScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    Core.client.closeSession(profile.toServerProfile(passwordFor(profile)), sessionId)
                }
            }
            if (result.isFailure) {
                _sessions.value = SessionListState.Error(Core.describe(result.exceptionOrNull()!!))
            } else {
                refreshSessions(profile)
            }
        }
    }

    fun clearSessions() {
        _sessions.value = SessionListState.Idle
    }
}
