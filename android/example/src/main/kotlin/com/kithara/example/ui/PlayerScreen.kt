package com.kithara.example.ui

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
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBars
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.FastForward
import androidx.compose.material.icons.rounded.FastRewind
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.automirrored.rounded.VolumeOff
import androidx.compose.material.icons.automirrored.rounded.VolumeUp
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.SliderDefaults
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.kithara.PlayerStatus
import com.kithara.TrackStatus
import com.kithara.example.PlaylistEntry
import com.kithara.example.PlayerUiState
import com.kithara.example.R
import com.kithara.example.ui.theme.AccentGold
import com.kithara.example.ui.theme.CardBackground
import com.kithara.example.ui.theme.KitharaBackground
import com.kithara.example.ui.theme.KitharaDanger
import com.kithara.example.ui.theme.KitharaMuted
import com.kithara.example.ui.theme.KitharaSuccess
import com.kithara.example.ui.theme.KitharaTheme
import com.kithara.example.ui.theme.KitharaWarning
import com.kithara.example.ui.theme.PanelBackground
import com.kithara.example.ui.theme.PanelBorder
import com.kithara.example.ui.theme.PrimaryText
import com.kithara.example.ui.theme.SecondaryText
import kotlin.math.exp
import kotlin.math.ln
import kotlin.math.roundToInt

internal sealed interface PlayerScreenEvent {
    data class UrlChanged(val url: String) : PlayerScreenEvent
    data object AddClick : PlayerScreenEvent
    data object PlayPauseClick : PlayerScreenEvent
    data object PrevClick : PlayerScreenEvent
    data object NextClick : PlayerScreenEvent
    data class TrackClick(val trackId: String) : PlayerScreenEvent
    data class RateClick(val rate: Float) : PlayerScreenEvent
    data object SeekStarted : PlayerScreenEvent
    data class SeekChanged(val value: Float) : PlayerScreenEvent
    data object SeekFinished : PlayerScreenEvent
    data class RemoveTrackClick(val trackId: String) : PlayerScreenEvent
    data class VolumeChanged(val volume: Float) : PlayerScreenEvent
    data object MuteClick : PlayerScreenEvent
    data class EqBandChanged(val bandIndex: Int, val gain: Float) : PlayerScreenEvent
    data object EqResetClick : PlayerScreenEvent
    data class CrossfadeChanged(val durationSeconds: Float) : PlayerScreenEvent
    data class AbrChanged(val variantIndex: UInt?) : PlayerScreenEvent
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

    var selectedTabIndex by remember { mutableIntStateOf(0) }
    val tabs = listOf("Playlist", "EQ", "Settings")

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
        NowPlayingSection(
            trackTitle = uiState.trackTitle,
            variantLabel = uiState.currentVariantLabel,
        )
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
        VolumeSection(
            volume = uiState.volume,
            isMuted = uiState.isMuted,
            onEvent = onEvent,
        )

        TabPills(
            tabs = tabs,
            selectedIndex = selectedTabIndex,
            onSelect = { selectedTabIndex = it },
        )

        Box(modifier = Modifier.weight(1f)) {
            when (selectedTabIndex) {
                0 -> PlaylistSection(
                    playlist = uiState.playlist,
                    currentTrackId = uiState.currentTrackId,
                    onEvent = onEvent,
                    modifier = Modifier.fillMaxSize(),
                )
                1 -> EqSection(
                    eqGains = uiState.eqGains,
                    onEvent = onEvent,
                    modifier = Modifier.fillMaxSize(),
                )
                2 -> SettingsSection(
                    crossfadeDuration = uiState.crossfadeDuration,
                    abrIsAuto = uiState.abrIsAuto,
                    selectedVariantIndex = uiState.selectedVariantIndex,
                    discoveredVariants = uiState.discoveredVariants,
                    onEvent = onEvent,
                    modifier = Modifier.fillMaxSize(),
                )
            }
        }

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
private fun NowPlayingSection(trackTitle: String, variantLabel: String?) {
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
            Column(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                Text(
                    text = trackTitle.ifBlank { stringResource(R.string.no_track) },
                    color = PrimaryText,
                    style = MaterialTheme.typography.titleMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (!variantLabel.isNullOrBlank()) {
                    Text(
                        text = variantLabel,
                        color = SecondaryText,
                        style = MaterialTheme.typography.labelSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
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
                inactiveTrackColor = PanelBorder,
                disabledActiveTrackColor = PanelBorder,
                disabledInactiveTrackColor = PanelBorder,
                disabledThumbColor = KitharaMuted,
            ),
        )
        Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                text = formatTime(currentTimeSeconds),
                color = SecondaryText,
                style = MaterialTheme.typography.labelLarge,
                fontFamily = FontFamily.Monospace,
            )
            Box(modifier = Modifier.weight(1f))
            Text(
                text = formatTime(durationSeconds),
                color = SecondaryText,
                style = MaterialTheme.typography.labelLarge,
                fontFamily = FontFamily.Monospace,
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
                imageVector = Icons.Rounded.FastRewind,
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
                imageVector = Icons.Rounded.FastForward,
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
    onEvent: (PlayerScreenEvent) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        availableRates.forEach { rate ->
            PillButton(
                text = rateLabel(rate),
                selected = selectedRate == rate,
                onClick = { onEvent(PlayerScreenEvent.RateClick(rate)) },
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun VolumeSection(volume: Float, isMuted: Boolean, onEvent: (PlayerScreenEvent) -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        IconButton(onClick = { onEvent(PlayerScreenEvent.MuteClick) }) {
            Icon(
                imageVector = if (isMuted) Icons.AutoMirrored.Rounded.VolumeOff else Icons.AutoMirrored.Rounded.VolumeUp,
                contentDescription = null,
                tint = if (isMuted) KitharaMuted else AccentGold,
            )
        }
        Slider(
            value = if (isMuted) 0f else volume,
            onValueChange = { onEvent(PlayerScreenEvent.VolumeChanged(it)) },
            modifier = Modifier.weight(1f),
            valueRange = 0f..1f,
            colors = SliderDefaults.colors(
                thumbColor = PrimaryText,
                activeTrackColor = AccentGold,
                inactiveTrackColor = PanelBorder,
            ),
        )
        Text(
            text = "%d%%".format(((if (isMuted) 0f else volume) * 100f).toInt()),
            color = SecondaryText,
            style = MaterialTheme.typography.labelLarge,
            modifier = Modifier.width(40.dp),
            textAlign = TextAlign.End,
        )
    }
}

@Composable
private fun TabPills(
    tabs: List<String>,
    selectedIndex: Int,
    onSelect: (Int) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        tabs.forEachIndexed { index, title ->
            PillButton(
                text = title,
                selected = selectedIndex == index,
                onClick = { onSelect(index) },
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun PillButton(
    text: String,
    selected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Box(
        modifier = modifier
            .clip(RoundedCornerShape(8.dp))
            .background(if (selected) AccentGold else PanelBackground)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 7.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = text,
            color = if (selected) KitharaBackground else SecondaryText,
            fontWeight = if (selected) FontWeight.Bold else FontWeight.Normal,
            style = MaterialTheme.typography.labelSmall,
            maxLines = 1,
            overflow = TextOverflow.Visible,
        )
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
                onRemove = { onEvent(PlayerScreenEvent.RemoveTrackClick(entry.id)) },
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
    onRemove: () -> Unit,
) {
    val dismissState = rememberSwipeToDismissBoxState()
    val currentOnRemove by rememberUpdatedState(onRemove)
    LaunchedEffect(dismissState.currentValue) {
        if (dismissState.currentValue == SwipeToDismissBoxValue.EndToStart) {
            currentOnRemove()
        }
    }

    SwipeToDismissBox(
        state = dismissState,
        enableDismissFromStartToEnd = false,
        enableDismissFromEndToStart = true,
        backgroundContent = {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .clip(RoundedCornerShape(8.dp))
                    .background(KitharaDanger)
                    .padding(horizontal = 16.dp),
                contentAlignment = Alignment.CenterEnd,
            ) {
                Icon(
                    imageVector = Icons.Rounded.Delete,
                    contentDescription = null,
                    tint = PrimaryText,
                    modifier = Modifier.size(20.dp),
                )
            }
        },
    ) {
        PlaylistRow(
            index = index,
            entry = entry,
            isCurrent = isCurrent,
            onClick = onClick,
        )
    }
}

@Composable
private fun PlaylistRow(
    index: Int,
    entry: PlaylistEntry,
    isCurrent: Boolean,
    onClick: () -> Unit,
) {
    val statusColor = trackStatusColor(entry.trackStatus)
    val background = if (isCurrent) AccentGold.copy(alpha = 0.18f) else PanelBackground
    val indexColor = statusColor ?: if (isCurrent) AccentGold else KitharaMuted
    val nameColor = statusColor ?: if (isCurrent) PrimaryText else SecondaryText

    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(background)
            .clickable(onClick = onClick)
            .padding(horizontal = 12.dp, vertical = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = "%02d".format(index + 1),
            color = indexColor,
            style = MaterialTheme.typography.labelMedium,
            fontFamily = FontFamily.Monospace,
        )
        Text(
            text = entry.name,
            color = nameColor,
            style = MaterialTheme.typography.bodyMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Text(
            text = formatDuration(entry.duration),
            color = KitharaMuted,
            style = MaterialTheme.typography.labelSmall,
            fontFamily = FontFamily.Monospace,
        )
    }
}

@Composable
private fun EqSection(
    eqGains: List<Float>,
    onEvent: (PlayerScreenEvent) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (eqGains.isEmpty()) {
        Box(modifier = modifier, contentAlignment = Alignment.Center) {
            Text(text = "EQ is not available", color = KitharaMuted)
        }
        return
    }

    Card(
        modifier = modifier.fillMaxWidth(),
        shape = RoundedCornerShape(10.dp),
        colors = CardDefaults.cardColors(containerColor = CardBackground),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    text = "EQ",
                    style = MaterialTheme.typography.labelLarge,
                    color = PrimaryText,
                    fontWeight = FontWeight.SemiBold,
                )
                Box(modifier = Modifier.weight(1f))
                TextButton(
                    onClick = { onEvent(PlayerScreenEvent.EqResetClick) },
                    contentPadding = PaddingValues(horizontal = 8.dp, vertical = 0.dp),
                ) {
                    Text(
                        text = "Reset",
                        color = SecondaryText,
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                eqGains.forEachIndexed { index, gain ->
                    EqBand(
                        gain = gain,
                        label = eqBandLabel(index, eqGains.size),
                        onChange = { value ->
                            onEvent(PlayerScreenEvent.EqBandChanged(index, value))
                        },
                        modifier = Modifier.weight(1f),
                    )
                }
            }
        }
    }
}

@Composable
private fun EqBand(
    gain: Float,
    label: String,
    onChange: (Float) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier,
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Box(
            modifier = Modifier.size(width = 28.dp, height = 140.dp),
            contentAlignment = Alignment.Center,
        ) {
            Slider(
                value = gain.coerceIn(EQ_GAIN_MIN, EQ_GAIN_MAX),
                onValueChange = onChange,
                valueRange = EQ_GAIN_MIN..EQ_GAIN_MAX,
                modifier = Modifier
                    .requiredSize(width = 140.dp, height = 28.dp)
                    .rotate(-90f),
                colors = SliderDefaults.colors(
                    thumbColor = PrimaryText,
                    activeTrackColor = AccentGold,
                    inactiveTrackColor = PanelBorder,
                ),
            )
        }
        Text(
            text = label,
            color = KitharaMuted,
            fontSize = 9.sp,
            maxLines = 1,
        )
    }
}

@Composable
private fun SettingsSection(
    crossfadeDuration: Float,
    abrIsAuto: Boolean,
    selectedVariantIndex: UInt?,
    discoveredVariants: List<Pair<UInt, String>>,
    onEvent: (PlayerScreenEvent) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .padding(vertical = 16.dp)
            .fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(
                text = "Quality",
                style = MaterialTheme.typography.labelLarge,
                color = PrimaryText,
            )
            QualityChips(
                abrIsAuto = abrIsAuto,
                selectedVariantIndex = selectedVariantIndex,
                discoveredVariants = discoveredVariants,
                onEvent = onEvent,
            )
        }
        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(
                text = "Crossfade",
                style = MaterialTheme.typography.labelLarge,
                color = PrimaryText,
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Slider(
                    value = crossfadeDuration.coerceIn(0f, CROSSFADE_MAX_SECONDS),
                    onValueChange = { value ->
                        onEvent(PlayerScreenEvent.CrossfadeChanged(value))
                    },
                    valueRange = 0f..CROSSFADE_MAX_SECONDS,
                    modifier = Modifier.weight(1f),
                    colors = SliderDefaults.colors(
                        thumbColor = PrimaryText,
                        activeTrackColor = AccentGold,
                        inactiveTrackColor = PanelBorder,
                    ),
                )
                Text(
                    text = "%.1fs".format(crossfadeDuration),
                    color = SecondaryText,
                    style = MaterialTheme.typography.labelLarge,
                    modifier = Modifier.width(48.dp),
                    textAlign = TextAlign.End,
                    fontFamily = FontFamily.Monospace,
                )
            }
        }
    }
}

@Composable
private fun QualityChips(
    abrIsAuto: Boolean,
    selectedVariantIndex: UInt?,
    discoveredVariants: List<Pair<UInt, String>>,
    onEvent: (PlayerScreenEvent) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        PillButton(
            text = "Auto",
            selected = abrIsAuto,
            onClick = { onEvent(PlayerScreenEvent.AbrChanged(null)) },
        )
        discoveredVariants.forEach { (index, label) ->
            val selected = !abrIsAuto && selectedVariantIndex == index
            PillButton(
                text = label,
                selected = selected,
                onClick = { onEvent(PlayerScreenEvent.AbrChanged(index)) },
            )
        }
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

private fun formatDuration(seconds: Double?): String =
    formatTime(seconds?.toFloat())

private fun rateLabel(rate: Float): String =
    if (rate == rate.toInt().toFloat()) "${rate.toInt()}x" else "${rate}x"

private fun eqBandLabel(band: Int, total: Int): String {
    val logMin = ln(30.0)
    val logMax = ln(18000.0)
    val frac = if (total > 1) band.toDouble() / (total - 1).toDouble() else 0.0
    val freq = exp(logMin + frac * (logMax - logMin))
    return if (freq >= 1000.0) "${(freq / 1000.0).roundToInt()}k" else "${freq.roundToInt()}"
}

private const val EQ_GAIN_MIN: Float = -24f
private const val EQ_GAIN_MAX: Float = 6f
private const val CROSSFADE_MAX_SECONDS: Float = 8f

@Preview(
    showBackground = true,
    backgroundColor = 0xFF1A1A2E,
    heightDp = 900,
)
@Composable
private fun PlayerScreenPreview() {
    val playlist = listOf(
        PlaylistEntry(id = "preview-1", url = "", name = "song.mp3", duration = 215.0),
        PlaylistEntry(id = "preview-2", url = "", name = "another-long-track-name-that-gets-truncated.mp3", duration = 184.0),
        PlaylistEntry(id = "preview-3", url = "", name = "third-track.mp3", duration = null),
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
                eqGains = List(10) { 0f },
                currentVariantLabel = "256 kbps",
            ),
            onEvent = {},
        )
    }
}
