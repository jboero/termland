@file:OptIn(ExperimentalFoundationApi::class)

package dev.termland.android.ui

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.termland.android.data.HostProfile
import dev.termland.android.net.CodecSupport
import dev.termland.core.SessionSummary

/**
 * Server list -> session list -> viewer.
 *
 * Two screens rather than a nav graph: the flow is two levels deep and adding
 * navigation-compose for that would be more moving parts than it saves.
 */
@Composable
fun HomeScreen(
    vm: HomeViewModel,
    onOpenSession: (HostProfile, String?, String?) -> Unit,
) {
    val profiles by vm.profiles.collectAsStateWithLifecycle()
    var selected by remember { mutableStateOf<HostProfile?>(null) }
    var editing by remember { mutableStateOf<HostProfile?>(null) }

    val current = selected?.let { sel -> profiles.firstOrNull { it.id == sel.id } ?: sel }

    if (current == null) {
        ServerListScreen(
            profiles = profiles,
            onSelect = { vm.clearSessions(); selected = it; vm.refreshSessions(it) },
            onEdit = { editing = it },
            onDelete = { vm.delete(it.id) },
            onAdd = { editing = HostProfile() },
        )
    } else {
        SessionListScreen(
            vm = vm,
            profile = current,
            onBack = { selected = null; vm.clearSessions() },
            onEdit = { editing = current },
            onOpenSession = onOpenSession,
        )
    }

    editing?.let { profile ->
        ProfileEditorDialog(
            initial = profile,
            onDismiss = { editing = null },
            onSave = {
                vm.save(it)
                if (!it.rememberPassword) vm.rememberTransientPassword(it.id, it.password)
                editing = null
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ServerListScreen(
    profiles: List<HostProfile>,
    onSelect: (HostProfile) -> Unit,
    onEdit: (HostProfile) -> Unit,
    onDelete: (HostProfile) -> Unit,
    onAdd: () -> Unit,
) {
    Scaffold(
        topBar = { TopAppBar(title = { Text("Termland") }) },
        floatingActionButton = {
            FloatingActionButton(onClick = onAdd) { Icon(Icons.Filled.Add, "Add server") }
        },
    ) { padding ->
        if (profiles.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
                Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.padding(32.dp),
                ) {
                    Text("No servers yet", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Add a Termland server to see its resumable sessions.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    CodecCapabilityNote()
                }
            }
        } else {
            LazyColumn(
                contentPadding = PaddingValues(
                    top = padding.calculateTopPadding() + 8.dp,
                    bottom = padding.calculateBottomPadding() + 88.dp,
                    start = 12.dp,
                    end = 12.dp,
                ),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                items(profiles, key = { it.id }) { profile ->
                    ServerCard(profile, onSelect, onEdit, onDelete)
                }
                item { CodecCapabilityNote(Modifier.padding(top = 16.dp)) }
            }
        }
    }
}

@Composable
private fun ServerCard(
    profile: HostProfile,
    onSelect: (HostProfile) -> Unit,
    onEdit: (HostProfile) -> Unit,
    onDelete: (HostProfile) -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    Card(Modifier.fillMaxWidth()) {
        Row(
            Modifier
                .combinedClickable(
                    onClick = { onSelect(profile) },
                    onLongClick = { menuOpen = true },
                )
                .padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(profile.displayName, style = MaterialTheme.typography.titleMedium)
                Text(
                    profile.subtitle,
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                )
            }
            Box {
                IconButton(onClick = { menuOpen = true }) {
                    Icon(Icons.Filled.MoreVert, "More")
                }
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                    DropdownMenuItem(
                        text = { Text("Edit") },
                        leadingIcon = { Icon(Icons.Filled.Edit, null) },
                        onClick = { menuOpen = false; onEdit(profile) },
                    )
                    DropdownMenuItem(
                        text = { Text("Delete") },
                        onClick = { menuOpen = false; onDelete(profile) },
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SessionListScreen(
    vm: HomeViewModel,
    profile: HostProfile,
    onBack: () -> Unit,
    onEdit: () -> Unit,
    onOpenSession: (HostProfile, String?, String?) -> Unit,
) {
    val state by vm.sessions.collectAsStateWithLifecycle()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(profile.displayName) },
                navigationIcon = {
                    IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                },
                actions = {
                    IconButton(onClick = { vm.refreshSessions(profile) }) {
                        Icon(Icons.Filled.Refresh, "Refresh")
                    }
                    IconButton(onClick = onEdit) { Icon(Icons.Filled.Edit, "Edit server") }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(
                onClick = { onOpenSession(profile, null, vm.passwordFor(profile)) },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text("New session") }

            when (val s = state) {
                SessionListState.Idle -> Unit
                SessionListState.Loading -> Box(
                    Modifier.fillMaxWidth().padding(24.dp),
                    contentAlignment = Alignment.Center,
                ) { CircularProgressIndicator() }

                is SessionListState.Error -> Column(
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.padding(vertical = 12.dp),
                ) {
                    Text(
                        "Could not list sessions",
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                    Text(s.message, style = MaterialTheme.typography.bodySmall)
                    OutlinedButton(onClick = { vm.refreshSessions(profile) }) { Text("Try again") }
                }

                is SessionListState.Loaded -> if (s.sessions.isEmpty()) {
                    Text(
                        "No resumable sessions on this server.",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(vertical = 12.dp),
                    )
                } else {
                    LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        items(s.sessions, key = { it.sessionId }) { session ->
                            SessionCard(
                                session = session,
                                onResume = {
                                    onOpenSession(profile, session.sessionId, vm.passwordFor(profile))
                                },
                                onClose = { vm.closeSession(profile, session.sessionId) },
                            )
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun SessionCard(session: SessionSummary, onResume: () -> Unit, onClose: () -> Unit) {
    var menuOpen by remember { mutableStateOf(false) }
    Card(Modifier.fillMaxWidth()) {
        Row(
            Modifier
                .combinedClickable(onClick = onResume, onLongClick = { menuOpen = true })
                .padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    "${session.mode}  ${session.width}×${session.height}",
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    buildString {
                        append(formatAge(session.ageSecs.toLong()))
                        append(" · ")
                        append(if (session.attached) "attached elsewhere" else "detached")
                    },
                    style = MaterialTheme.typography.bodySmall,
                )
                Text(
                    session.sessionId,
                    style = MaterialTheme.typography.labelSmall,
                    fontFamily = FontFamily.Monospace,
                )
            }
            OutlinedButton(onClick = onResume) { Text("Resume") }
            Box {
                IconButton(onClick = { menuOpen = true }) { Icon(Icons.Filled.MoreVert, "More") }
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                    DropdownMenuItem(
                        text = { Text("Close session") },
                        onClick = { menuOpen = false; onClose() },
                    )
                }
            }
        }
    }
}

/**
 * Surfacing the probe result is not decoration: it is the single best explanation
 * for "why did I get H.264 instead of AV1", since this exact set is what the
 * server negotiates against.
 */
@Composable
private fun CodecCapabilityNote(modifier: Modifier = Modifier) {
    Column(modifier) {
        Text("This device can decode", style = MaterialTheme.typography.labelMedium)
        Text(
            CodecSupport.supportedCodecs.joinToString { codec ->
                val hw = CodecSupport.best(codec)?.hardware == true
                "${codec.name}${if (hw) " (hw)" else " (sw)"}"
            }.ifEmpty { "nothing — no usable video decoder found" },
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
        )
    }
}

private fun formatAge(secs: Long): String = when {
    secs < 60 -> "${secs}s old"
    secs < 3600 -> "${secs / 60}m old"
    secs < 86400 -> "${secs / 3600}h ${secs % 3600 / 60}m old"
    else -> "${secs / 86400}d old"
}
