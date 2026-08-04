package dev.termland.android.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.text.KeyboardOptions
import dev.termland.android.data.HostProfile

/** Add/edit a saved server, including the session defaults used for New session. */
@Composable
fun ProfileEditorDialog(
    initial: HostProfile,
    onDismiss: () -> Unit,
    onSave: (HostProfile) -> Unit,
) {
    var p by remember { mutableStateOf(initial) }
    var portText by remember { mutableStateOf(initial.port.toString()) }
    var qualityText by remember { mutableStateOf(initial.quality.toString()) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (initial.host.isBlank()) "Add server" else "Edit server") },
        confirmButton = {
            TextButton(
                enabled = p.host.isNotBlank(),
                onClick = {
                    onSave(
                        p.copy(
                            port = portText.toIntOrNull()?.coerceIn(1, 65535) ?: HostProfile.DEFAULT_PORT,
                            quality = qualityText.toIntOrNull()?.coerceIn(1, 100) ?: 75,
                        ),
                    )
                },
            ) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        text = {
            Column(
                Modifier.heightIn(max = 460.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Field("Label", p.label) { p = p.copy(label = it) }
                Field("Host", p.host) { p = p.copy(host = it) }
                Field("Port", portText, KeyboardType.Number) { portText = it.filter(Char::isDigit) }
                Field("Username", p.username) { p = p.copy(username = it) }
                OutlinedTextField(
                    value = p.password,
                    onValueChange = { p = p.copy(password = it) },
                    label = { Text("Password") },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                    modifier = Modifier.fillMaxWidth(),
                )

                Toggle("Use TLS", p.useTls) { p = p.copy(useTls = it) }
                if (p.useTls) {
                    Toggle("Accept invalid certificate", p.acceptInvalidCerts) {
                        p = p.copy(acceptInvalidCerts = it)
                    }
                    if (p.acceptInvalidCerts) {
                        Text(
                            "Certificate verification off — only for self-signed servers you trust.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                }
                Toggle("Remember password on this device", p.rememberPassword) {
                    p = p.copy(rememberPassword = it)
                }
                if (p.rememberPassword) {
                    Text(
                        "Stored unencrypted in app storage; Keystore-backed storage is planned.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }

                Text("Session defaults", style = MaterialTheme.typography.titleSmall)
                Field("Quality (1-100)", qualityText, KeyboardType.Number) {
                    qualityText = it.filter(Char::isDigit)
                }
                Toggle("Audio", p.audio) { p = p.copy(audio = it) }
                Field("Desktop shell (blank = server default)", p.desktopShell) {
                    p = p.copy(desktopShell = it)
                }
                Field("App command (blank = full desktop)", p.appCommand) {
                    p = p.copy(appCommand = it)
                }
            }
        },
    )
}

@Composable
private fun Field(
    label: String,
    value: String,
    keyboardType: KeyboardType = KeyboardType.Text,
    onChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = keyboardType),
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun Toggle(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
        Switch(checked = checked, onCheckedChange = onChange)
    }
}
