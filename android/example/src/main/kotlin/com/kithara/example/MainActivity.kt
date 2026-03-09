package com.kithara.example

import android.content.Context
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.kithara.Kithara
import com.kithara.KitharaConfig
import com.kithara.KitharaPlayer
import com.kithara.KitharaPlayerItem
import com.kithara.KitharaStoreOptions
import java.io.File
import java.io.IOException
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val TAG = "KitharaExample"

class MainActivity : ComponentActivity() {
    private val player by lazy(LazyThreadSafetyMode.NONE) { KitharaPlayer() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Kithara.initialize(
            applicationContext,
            KitharaConfig(
                store = KitharaStoreOptions(
                    cacheDir = applicationContext.cacheDir.absolutePath,
                ),
            ),
        )
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    PlayerScreen(player)
                }
            }
        }
    }
}

@Composable
private fun PlayerScreen(player: KitharaPlayer) {
    val context = LocalContext.current
    var url by rememberSaveable { mutableStateOf("") }
    var localError by remember { mutableStateOf<String?>(null) }

    val playerState by player.state.collectAsState()
    val scope = rememberCoroutineScope()
    val openFileLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.OpenDocument(),
    ) { uri ->
        if (uri == null) {
            return@rememberLauncherForActivityResult
        }

        localError = null
        scope.launch {
            try {
                val localPath = copyDocumentToCache(context, uri)
                url = localPath
                loadAndPlay(player, localPath)
            } catch (error: Exception) {
                Log.e(TAG, "Load/play failed for selected file: $uri", error)
                localError = error.message ?: error::class.simpleName.orEmpty()
            }
        }
    }

    LaunchedEffect(playerState.error) {
        playerState.error?.let { error ->
            Log.e(TAG, "Player error: ${error.logMessage()}", error)
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        OutlinedTextField(
            value = url,
            onValueChange = {
                url = it
                localError = null
            },
            modifier = Modifier.fillMaxWidth(),
            label = { Text("Source URL or local path") },
            placeholder = { Text("https://example.com/song.mp3") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            singleLine = true,
        )

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Button(
                onClick = {
                    val trimmedUrl = url.trim()
                    if (trimmedUrl.isEmpty()) {
                        localError = "Enter a URL or pick a file"
                    } else {
                        scope.launch {
                            try {
                                loadAndPlay(player, trimmedUrl)
                            } catch (error: Exception) {
                                Log.e(TAG, "Load/play failed for source: $trimmedUrl", error)
                                localError = error.message ?: error::class.simpleName.orEmpty()
                            }
                        }
                    }
                },
            ) {
                Text("Load and Play")
            }

            Button(
                onClick = {
                    openFileLauncher.launch(arrayOf("audio/*"))
                },
            ) {
                Text("Pick File")
            }

            Button(
                onClick = {
                    if (playerState.rate > 0f) {
                        player.pause()
                    } else {
                        player.play()
                    }
                },
            ) {
                Text(if (playerState.rate > 0f) "Pause" else "Play")
            }

            Button(
                onClick = {
                    localError = null
                    player.pause()
                    player.removeAllItems()
                },
            ) {
                Text("Stop")
            }
        }

        Text(
            text = "Status: ${playerState.status}",
            style = MaterialTheme.typography.titleMedium,
        )
        Text(
            text = "${formatTime(playerState.currentTime)} / ${formatTime(playerState.duration)}",
            style = MaterialTheme.typography.bodyLarge,
        )
        Text(
            text = localError ?: playerState.error?.message ?: "No errors",
            style = MaterialTheme.typography.bodyMedium,
        )
    }
}

private fun formatTime(seconds: Double?): String {
    if (seconds == null) {
        return "--:--"
    }

    val wholeSeconds = seconds.toInt()
    val minutes = wholeSeconds / 60
    val remainingSeconds = wholeSeconds % 60
    return "%d:%02d".format(minutes, remainingSeconds)
}

private suspend fun loadAndPlay(
    player: KitharaPlayer,
    source: String,
) {
    val item = KitharaPlayerItem(source)
    item.load()
    player.removeAllItems()
    player.insert(item)
    player.play()
}

private suspend fun copyDocumentToCache(
    context: Context,
    uri: Uri,
): String = withContext(Dispatchers.IO) {
    val fileName = queryDisplayName(context, uri)
        ?.takeIf(String::isNotBlank)
        ?: uri.lastPathSegment?.substringAfterLast('/')
        ?: "selected-audio"
    val importDir = File(context.cacheDir, "imports")
    if (!importDir.exists() && !importDir.mkdirs()) {
        throw IOException("Unable to create import directory")
    }

    val outputFile = File(importDir, "${UUID.randomUUID()}-$fileName")
    context.contentResolver.openInputStream(uri)?.use { input ->
        outputFile.outputStream().use { output ->
            input.copyTo(output)
        }
    } ?: throw IOException("Unable to open selected file")

    outputFile.absolutePath
}

private fun queryDisplayName(
    context: Context,
    uri: Uri,
): String? = context.contentResolver.query(
    uri,
    arrayOf(OpenableColumns.DISPLAY_NAME),
    null,
    null,
    null,
)?.use { cursor ->
    val nameColumn = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
    if (nameColumn < 0 || !cursor.moveToFirst()) {
        return@use null
    }
    cursor.getString(nameColumn)
}

private fun Throwable.logMessage(): String = message ?: javaClass.simpleName
