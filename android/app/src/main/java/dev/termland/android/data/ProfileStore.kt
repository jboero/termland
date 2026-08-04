package dev.termland.android.data

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

private val Context.dataStore: DataStore<Preferences> by preferencesDataStore(name = "termland")

/**
 * Saved host profiles, persisted as one JSON blob in Preferences DataStore.
 *
 * A blob rather than a row per field because the whole list is always read and
 * written together, and DataStore gives us the atomic write + Flow for free.
 */
class ProfileStore(private val context: Context) {

    private val json = Json { ignoreUnknownKeys = true; encodeDefaults = true }

    val profiles: Flow<List<HostProfile>> = context.dataStore.data.map { prefs ->
        prefs[KEY_PROFILES]?.let { raw ->
            runCatching { json.decodeFromString<List<HostProfile>>(raw) }.getOrDefault(emptyList())
        } ?: emptyList()
    }

    val lastUsedProfileId: Flow<String?> = context.dataStore.data.map { it[KEY_LAST_USED] }

    suspend fun upsert(profile: HostProfile) = mutate { list ->
        // Never let a non-persisted password linger in the store.
        val sanitised = if (profile.rememberPassword) profile else profile.copy(password = "")
        val idx = list.indexOfFirst { it.id == sanitised.id }
        if (idx >= 0) list.toMutableList().also { it[idx] = sanitised } else list + sanitised
    }

    suspend fun delete(id: String) = mutate { list -> list.filterNot { it.id == id } }

    suspend fun setLastUsed(id: String) {
        context.dataStore.edit { it[KEY_LAST_USED] = id }
    }

    private suspend fun mutate(block: (List<HostProfile>) -> List<HostProfile>) {
        context.dataStore.edit { prefs ->
            val current = prefs[KEY_PROFILES]?.let {
                runCatching { json.decodeFromString<List<HostProfile>>(it) }.getOrDefault(emptyList())
            } ?: emptyList()
            prefs[KEY_PROFILES] = json.encodeToString(block(current))
        }
    }

    private companion object {
        val KEY_PROFILES = stringPreferencesKey("profiles_json")
        val KEY_LAST_USED = stringPreferencesKey("last_used_profile")
    }
}
