package com.kithara.example.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
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
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.kithara.PlayerStatus
import com.kithara.example.PlayerUiState
import com.kithara.example.PlayerViewModel
import com.kithara.example.R
import com.kithara.example.ui.theme.AccentGold
import com.kithara.example.ui.theme.KitharaBackground
import com.kithara.example.ui.theme.KitharaDanger
import com.kithara.example.ui.theme.KitharaMuted
import com.kithara.example.ui.theme.KitharaSuccess
import com.kithara.example.ui.theme.PanelBackground
import com.kithara.example.ui.theme.PanelBorder
import com.kithara.example.ui.theme.PrimaryText
import com.kithara.example.ui.theme.SecondaryText
import com.kithara.example.ui.theme.KitharaTheme

@Composable
internal fun PlayerScreen(
    uiState: PlayerUiState,
    onUrlChanged: (String) -> Unit,
    onLoadClick: () -> Unit,
    onPickFileClick: () -> Unit,
    onPlayPauseClick: () -> Unit,
    onRateClick: (Float) -> Unit,
    onSeekStarted: () -> Unit,
    onSeekChanged: (Float) -> Unit,
    onSeekFinished: () -> Unit,
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
        verticalArrangement = Arrangement.spacedBy(18.dp),
    ) {
        HeaderSection(
            isPlaying = uiState.isPlaying,
            status = uiState.status,
        )
        UrlSection(
            url = uiState.url,
            onUrlChanged = onUrlChanged,
            onLoadClick = onLoadClick,
        )
        LocalFileAction(onPickFileClick = onPickFileClick)
        NowPlayingSection(trackTitle = uiState.trackTitle)
        SeekSection(
            currentTimeSeconds = sliderValue,
            durationSeconds = uiState.durationSeconds,
            enabled = sliderEnabled,
            isSeeking = uiState.isSeeking,
            onSeekStarted = onSeekStarted,
            onSeekChanged = onSeekChanged,
            onSeekFinished = onSeekFinished,
        )
        TransportSection(
            isPlaying = uiState.isPlaying,
            onPlayPauseClick = onPlayPauseClick,
        )
        RateSection(
            selectedRate = uiState.selectedRate,
            onRateClick = onRateClick,
        )
        uiState.errorMessage?.let { message ->
            ErrorSection(message = message)
        }
    }
}

@Composable
private fun HeaderSection(
    isPlaying: Boolean,
    status: PlayerStatus,
) {
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
        StatusBadge(status = status)
    }
}

@Composable
private fun StatusBadge(status: PlayerStatus) {
    val statusColor = when (status) {
        PlayerStatus.ReadyToPlay -> KitharaSuccess
        PlayerStatus.Failed -> KitharaDanger
        PlayerStatus.Unknown -> KitharaMuted
    }
    val statusText = when (status) {
        PlayerStatus.ReadyToPlay -> stringResource(R.string.status_ready)
        PlayerStatus.Failed -> stringResource(R.string.status_failed)
        PlayerStatus.Unknown -> stringResource(R.string.status_not_ready)
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
private fun UrlSection(
    url: String,
    onUrlChanged: (String) -> Unit,
    onLoadClick: () -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        OutlinedTextField(
            value = url,
            onValueChange = onUrlChanged,
            modifier = Modifier.weight(1f),
            placeholder = {
                Text(
                    text = stringResource(R.string.audio_url_hint),
                    color = KitharaMuted,
                )
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
            onClick = onLoadClick,
            enabled = url.isNotBlank(),
            shape = RoundedCornerShape(16.dp),
            colors = ButtonDefaults.buttonColors(
                containerColor = AccentGold,
                contentColor = KitharaBackground,
                disabledContainerColor = AccentGold.copy(alpha = 0.35f),
                disabledContentColor = KitharaBackground.copy(alpha = 0.7f),
            ),
        ) {
            Text(text = stringResource(R.string.load_action))
        }
    }
}

@Composable
private fun LocalFileAction(onPickFileClick: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.End,
    ) {
        TextButton(onClick = onPickFileClick) {
            Icon(
                imageVector = Icons.Rounded.FolderOpen,
                contentDescription = null,
                tint = AccentGold,
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
                .padding(horizontal = 16.dp, vertical = 18.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(
                imageVector = Icons.Rounded.MusicNote,
                contentDescription = null,
                tint = AccentGold,
            )
            Text(
                text = trackTitle.ifBlank { stringResource(R.string.no_track) },
                color = PrimaryText,
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
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
    onSeekStarted: () -> Unit,
    onSeekChanged: (Float) -> Unit,
    onSeekFinished: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Slider(
            value = currentTimeSeconds,
            onValueChange = { value ->
                if (!isSeeking) {
                    onSeekStarted()
                }
                onSeekChanged(value)
            },
            modifier = Modifier.fillMaxWidth(),
            valueRange = 0f..(durationSeconds ?: 1f),
            enabled = enabled,
            onValueChangeFinished = onSeekFinished,
            colors = SliderDefaults.colors(
                thumbColor = PrimaryText,
                activeTrackColor = AccentGold,
                inactiveTrackColor = PanelBackground,
                disabledActiveTrackColor = PanelBorder,
                disabledInactiveTrackColor = PanelBackground,
                disabledThumbColor = PanelBorder,
            ),
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
        ) {
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
private fun TransportSection(
    isPlaying: Boolean,
    onPlayPauseClick: () -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.Center,
    ) {
        FilledTonalButton(
            onClick = onPlayPauseClick,
            modifier = Modifier.size(96.dp),
            shape = CircleShape,
            colors = ButtonDefaults.filledTonalButtonColors(
                containerColor = AccentGold,
                contentColor = KitharaBackground,
            ),
        ) {
            Icon(
                imageVector = if (isPlaying) Icons.Rounded.Pause else Icons.Rounded.PlayArrow,
                contentDescription = if (isPlaying) {
                    stringResource(R.string.pause_action)
                } else {
                    stringResource(R.string.play_action)
                },
                modifier = Modifier.size(40.dp),
            )
        }
    }
}

@Composable
private fun RateSection(
    selectedRate: Float,
    onRateClick: (Float) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        PlayerViewModel.AvailableRates.forEach { rate ->
            val selected = selectedRate == rate
            OutlinedButton(
                onClick = { onRateClick(rate) },
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

private fun rateLabel(rate: Float): String {
    return if (rate == rate.toInt().toFloat()) {
        "${rate.toInt()}x"
    } else {
        "${rate}x"
    }
}

@Preview(showBackground = true, backgroundColor = 0xFF050507)
@Composable
private fun PlayerScreenPreview() {
    KitharaTheme {
        PlayerScreen(
            uiState = PlayerUiState(
                currentTimeSeconds = 42f,
                durationSeconds = 180f,
                selectedRate = 1f,
                status = PlayerStatus.Unknown,
                trackTitle = "No Track",
                url = "https://example.com/song.mp3",
            ),
            onUrlChanged = {},
            onLoadClick = {},
            onPickFileClick = {},
            onPlayPauseClick = {},
            onRateClick = {},
            onSeekStarted = {},
            onSeekChanged = {},
            onSeekFinished = {},
        )
    }
}
