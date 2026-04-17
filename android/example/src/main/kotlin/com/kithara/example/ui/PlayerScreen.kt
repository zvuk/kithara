package com.kithara.example.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBars
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.SkipNext
import androidx.compose.material.icons.rounded.SkipPrevious
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.kithara.PlayerStatus
import com.kithara.example.PlaylistEntry
import com.kithara.example.PlayerUiState
import com.kithara.example.R
import com.kithara.example.ui.theme.AccentGold
import com.kithara.example.ui.theme.KitharaBackground
import com.kithara.example.ui.theme.KitharaDanger
import com.kithara.example.ui.theme.KitharaMuted
import com.kithara.example.ui.theme.KitharaWarning
import com.kithara.example.ui.theme.KitharaSuccess
import androidx.compose.ui.graphics.Color
import com.kithara.TrackStatus
import com.kithara.example.ui.theme.KitharaTheme
import com.kithara.example.ui.theme.PanelBackground
import com.kithara.example.ui.theme.PanelBorder
import com.kithara.example.ui.theme.PrimaryText
import com.kithara.example.ui.theme.SecondaryText

internal sealed interface PlayerScreenEvent {
    data class UrlChanged(val url: String) : PlayerScreenEvent
    data object AddClick : PlayerScreenEvent
    data object PickFileClick : PlayerScreenEvent
    data object PlayPauseClick : PlayerScreenEvent
    data object PrevClick : PlayerScreenEvent
    data object NextClick : PlayerScreenEvent
    data class TrackClick(val trackId: String) : PlayerScreenEvent
    data class RateClick(val rate: Float) : PlayerScreenEvent
    data object SeekStarted : PlayerScreenEvent
    data class SeekChanged(val value: Float) : PlayerScreenEvent
    data object SeekFinished : PlayerScreenEvent
}

@Composable
internal fun PlayerScreen(
    uiState: PlayerUiState,
    onEvent: (PlayerScreenEvent) -> Unit,
    modifier: Modifier = Modifier,
) {
    val duration = uiState.durationSeconds ?: 0f
    val sliderEnabled = duration > 0f
    val sliderValue = if (sliderEnabled) {
        uiState.currentTimeSeconds.coerceIn(0f, duration)
    } else {
        0f
    }

    val systemBarsPadding = WindowInsets.systemBars.asPaddingValues()
    Column(
        modifier = modifier
            .fillMaxSize()
            .background(KitharaBackground)
            .padding(
                top = systemBarsPadding.calculateTopPadding() + 20.dp,
                bottom = systemBarsPadding.calculateBottomPadding() + 20.dp,
                start = 20.dp,
                end = 20.dp,
            ),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        HeaderSection(
            isPlaying = uiState.isPlaying,
            status = uiState.status,
            hasTracks = uiState.playlist.isNotEmpty(),
        )
        UrlSection(
            url = uiState.url,
            onEvent = onEvent,
        )
        LocalFileAction(onEvent = onEvent)
        NowPlayingSection(trackTitle = uiState.trackTitle)
        SeekSection(
            currentTimeSeconds = sliderValue,
            durationSeconds = uiState.durationSeconds,
            enabled = sliderEnabled,
            isSeeking = uiState.isSeeking,
            onEvent = onEvent,
        )
        TransportSection(
            isPlaying = uiState.isPlaying,
            onEvent = onEvent,
        )
        RateSection(
            selectedRate = uiState.selectedRate,
            availableRates = uiState.availableRates,
            onEvent = onEvent,
        )
        PlaylistSection(
            playlist = uiState.playlist,
            currentTrackId = uiState.currentTrackId,
            onEvent = onEvent,
            modifier = Modifier.weight(1f),
        )
        uiState.errorMessage?.let { ErrorSection(message = it) }
    }
}

@Composable
private fun HeaderSection(isPlaying: Boolean, status: PlayerStatus, hasTracks: Boolean) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = stringResource(R.string.app_title),
            style = MaterialTheme.typography.headlineMedium,
            color = if (isPlaying) AccentGold else PrimaryText,
            fontWeight = FontWeight.Bold,
        )
        Text(
            text = stringResource(R.string.demo_label),
            style = MaterialTheme.typography.bodyMedium,
            color = SecondaryText,
            modifier = Modifier.padding(start = 8.dp, top = 10.dp),
        )
        Box(modifier = Modifier.weight(1f))
        StatusBadge(status = status, isPlaying = isPlaying, hasTracks = hasTracks)
    }
}

@Composable
private fun StatusBadge(status: PlayerStatus, isPlaying: Boolean, hasTracks: Boolean) {
    val statusColor = when {
        status == PlayerStatus.Failed -> KitharaDanger
        !hasTracks -> KitharaMuted
        isPlaying -> KitharaSuccess
        else -> AccentGold
    }
    val statusText = when {
        status == PlayerStatus.Failed -> stringResource(R.string.status_failed)
        !hasTracks -> stringResource(R.string.status_not_ready)
        isPlaying -> stringResource(R.string.status_playing)
        else -> stringResource(R.string.status_idle)
    }

    Row(
        modifier = Modifier
            .clip(CircleShape)
            .background(PanelBackground)
            .padding(horizontal = 12.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(8.dp)
                .clip(CircleShape)
                .background(statusColor),
        )
        Text(
            text = statusText,
            color = SecondaryText,
            style = MaterialTheme.typography.labelLarge,
        )
    }
}

@Composable
private fun UrlSection(url: String, onEvent: (PlayerScreenEvent) -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        OutlinedTextField(
            value = url,
            onValueChange = { onEvent(PlayerScreenEvent.UrlChanged(it)) },
            modifier = Modifier.weight(1f),
            placeholder = {
                Text(text = stringResource(R.string.audio_url_hint), color = KitharaMuted)
            },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            colors = TextFieldDefaults.colors(
                focusedContainerColor = PanelBackground,
                unfocusedContainerColor = PanelBackground,
                disabledContainerColor = PanelBackground,
                focusedIndicatorColor = PanelBorder,
                unfocusedIndicatorColor = PanelBorder,
                cursorColor = AccentGold,
                focusedTextColor = PrimaryText,
                unfocusedTextColor = PrimaryText,
            ),
            shape = RoundedCornerShape(16.dp),
        )
        Button(
            onClick = { onEvent(PlayerScreenEvent.AddClick) },
            enabled = url.isNotBlank(),
            shape = RoundedCornerShape(16.dp),
            colors = ButtonDefaults.buttonColors(
                containerColor = AccentGold,
                contentColor = KitharaBackground,
                disabledContainerColor = AccentGold.copy(alpha = 0.35f),
                disabledContentColor = KitharaBackground.copy(alpha = 0.7f),
            ),
        ) {
            Text(text = stringResource(R.string.add_action))
        }
    }
}

@Composable
private fun LocalFileAction(onEvent: (PlayerScreenEvent) -> Unit) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        TextButton(onClick = { onEvent(PlayerScreenEvent.PickFileClick) }) {
            Icon(
                imageVector = Icons.Rounded.FolderOpen,
                contentDescription = null,
                tint = AccentGold
            )
            Text(
                text = stringResource(R.string.open_local_file),
                color = AccentGold,
                modifier = Modifier.padding(start = 8.dp),
            )
        }
    }
}

@Composable
private fun NowPlayingSection(trackTitle: String) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(18.dp),
        colors = CardDefaults.cardColors(containerColor = PanelBackground),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 16.dp, vertical = 14.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Rounded.MusicNote,
                contentDescription = null,
                tint = AccentGold
            )
            Text(
                text = trackTitle.ifBlank { stringResource(R.string.no_track) },
                color = PrimaryText,
                style = MaterialTheme.typography.titleMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
private fun SeekSection(
    currentTimeSeconds: Float,
    durationSeconds: Float?,
    enabled: Boolean,
    isSeeking: Boolean,
    onEvent: (PlayerScreenEvent) -> Unit,
) {
    Column(modifier = Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Slider(
            value = currentTimeSeconds,
            onValueChange = { value ->
                if (!isSeeking) onEvent(PlayerScreenEvent.SeekStarted)
                onEvent(PlayerScreenEvent.SeekChanged(value))
            },
            modifier = Modifier.fillMaxWidth(),
            valueRange = 0f..(durationSeconds ?: 1f),
            enabled = enabled,
            onValueChangeFinished = { onEvent(PlayerScreenEvent.SeekFinished) },
            colors = SliderDefaults.colors(
                thumbColor = PrimaryText,
                activeTrackColor = AccentGold,
                inactiveTrackColor = PanelBackground,
                disabledActiveTrackColor = PanelBorder,
                disabledInactiveTrackColor = PanelBackground,
                disabledThumbColor = PanelBorder,
            ),
        )
        Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = formatTime(currentTimeSeconds),
                color = SecondaryText,
                style = MaterialTheme.typography.labelLarge,
            )
            Box(modifier = Modifier.weight(1f))
            Text(
                text = formatTime(durationSeconds),
                color = SecondaryText,
                style = MaterialTheme.typography.labelLarge,
            )
        }
    }
}

@Composable
private fun TransportSection(isPlaying: Boolean, onEvent: (PlayerScreenEvent) -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(onClick = { onEvent(PlayerScreenEvent.PrevClick) }) {
            Icon(
                imageVector = Icons.Rounded.SkipPrevious,
                contentDescription = null,
                tint = PrimaryText,
                modifier = Modifier.size(32.dp),
            )
        }
        FilledTonalButton(
            onClick = { onEvent(PlayerScreenEvent.PlayPauseClick) },
            modifier = Modifier
                .padding(horizontal = 24.dp)
                .size(64.dp),
            shape = CircleShape,
            colors = ButtonDefaults.filledTonalButtonColors(
                containerColor = AccentGold,
                contentColor = KitharaBackground,
            ),
        ) {
            Icon(
                imageVector = if (isPlaying) Icons.Rounded.Pause else Icons.Rounded.PlayArrow,
                contentDescription = null,
                modifier = Modifier.size(28.dp),
            )
        }
        IconButton(onClick = { onEvent(PlayerScreenEvent.NextClick) }) {
            Icon(
                imageVector = Icons.Rounded.SkipNext,
                contentDescription = null,
                tint = PrimaryText,
                modifier = Modifier.size(32.dp),
            )
        }
    }
}

@Composable
private fun RateSection(
    selectedRate: Float,
    availableRates: List<Float>,
    onEvent: (PlayerScreenEvent) -> Unit
) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        availableRates.forEach { rate ->
            val selected = selectedRate == rate
            OutlinedButton(
                onClick = { onEvent(PlayerScreenEvent.RateClick(rate)) },
                modifier = Modifier.weight(1f),
                shape = RoundedCornerShape(10.dp),
                border = BorderStroke(
                    width = 1.dp,
                    color = if (selected) AccentGold else PanelBorder,
                ),
                colors = ButtonDefaults.outlinedButtonColors(
                    containerColor = if (selected) AccentGold else PanelBackground,
                    contentColor = if (selected) KitharaBackground else SecondaryText,
                ),
                contentPadding = PaddingValues(horizontal = 4.dp, vertical = 8.dp),
            ) {
                Text(
                    text = rateLabel(rate),
                    style = MaterialTheme.typography.labelSmall,
                    fontWeight = if (selected) FontWeight.Bold else FontWeight.Medium,
                    textAlign = TextAlign.Center,
                    maxLines = 1,
                )
            }
        }
    }
}

@Composable
private fun PlaylistSection(
    playlist: List<PlaylistEntry>,
    currentTrackId: String?,
    onEvent: (PlayerScreenEvent) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (playlist.isEmpty()) {
        Box(modifier = modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
            Text(
                text = stringResource(R.string.playlist_empty),
                color = KitharaMuted,
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        return
    }

    LazyColumn(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        itemsIndexed(playlist, key = { _, entry -> entry.id }) { index, entry ->
            PlaylistItem(
                index = index,
                entry = entry,
                isCurrent = entry.id == currentTrackId,
                onClick = { onEvent(PlayerScreenEvent.TrackClick(entry.id)) },
            )
        }
    }
}

@Composable
private fun PlaylistItem(
    index: Int,
    entry: PlaylistEntry,
    isCurrent: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val statusColor = trackStatusColor(entry.trackStatus)
    val background = if (isCurrent) AccentGold.copy(alpha = 0.18f) else KitharaBackground
    val indexColor = statusColor ?: if (isCurrent) AccentGold else KitharaMuted
    val nameColor = statusColor ?: if (isCurrent) PrimaryText else SecondaryText

    Row(
        modifier = modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(background)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = "%02d".format(index + 1),
            color = indexColor,
            style = MaterialTheme.typography.labelMedium,
        )
        Text(
            text = entry.name,
            color = nameColor,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
    }
}

private fun trackStatusColor(status: TrackStatus?): Color? = when (status) {
    is TrackStatus.Slow -> KitharaWarning
    is TrackStatus.Failed -> KitharaDanger
    else -> null
}

@Composable
private fun ErrorSection(message: String) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        colors = CardDefaults.cardColors(containerColor = KitharaDanger.copy(alpha = 0.12f)),
    ) {
        Text(
            text = message,
            modifier = Modifier.padding(horizontal = 14.dp, vertical = 12.dp),
            color = KitharaDanger,
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
        )
    }
}

private fun formatTime(seconds: Float?): String {
    val value = seconds ?: return "--:--"
    val totalSeconds = value.toInt()
    return "%d:%02d".format(totalSeconds / 60, totalSeconds % 60)
}

private fun rateLabel(rate: Float): String =
    if (rate == rate.toInt().toFloat()) "${rate.toInt()}x" else "${rate}x"

@Preview(
    showBackground = true,
    backgroundColor = 0xFF050507,
    heightDp = 900,
)
@Composable
private fun PlayerScreenPreview() {
    val playlist = listOf(
        PlaylistEntry(id = "preview-1", url = "", name = "song.mp3"),
        PlaylistEntry(id = "preview-2", url = "", name = "another-long-track-name-that-gets-truncated.mp3"),
        PlaylistEntry(id = "preview-3", url = "", name = "third-track.mp3"),
    )

    KitharaTheme {
        PlayerScreen(
            uiState = PlayerUiState(
                currentTimeSeconds = 42f,
                durationSeconds = 180f,
                selectedRate = 1f,
                status = PlayerStatus.ReadyToPlay,
                url = "",
                playlist = playlist,
                currentTrackId = playlist.first().id,
            ),
            onEvent = {},
        )
    }
}
