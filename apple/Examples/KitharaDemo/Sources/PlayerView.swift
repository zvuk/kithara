import Kithara
import SwiftUI

// MARK: - Kithara color palette (matches kithara-ui theme)

extension Color {
    /// Deep dark blue background (#1a1a2e).
    static let kitharaBg = Color(red: 0.102, green: 0.102, blue: 0.180)
    /// Slightly lighter panel (#222244).
    static let kitharaPanel = Color(red: 0.133, green: 0.133, blue: 0.267)
    /// Gold accent from logo (#bb9442).
    static let kitharaGold = Color(red: 0.733, green: 0.580, blue: 0.259)
    /// Light text (#e6e6e6).
    static let kitharaLight = Color(red: 0.900, green: 0.900, blue: 0.900)
    /// Muted gray (#888888).
    static let kitharaMuted = Color(red: 0.533, green: 0.533, blue: 0.533)
    /// Success green (#66cc66).
    static let kitharaSuccess = Color(red: 0.400, green: 0.800, blue: 0.400)
    /// Danger red (#e64c4c).
    static let kitharaDanger = Color(red: 0.900, green: 0.300, blue: 0.300)
    /// Warning amber (#e6b333) — matches kithara-ui theme.warning.
    static let kitharaWarning = Color(red: 0.902, green: 0.702, blue: 0.200)
}

// MARK: - Player View

struct PlayerView: View {
    @StateObject private var viewModel = PlayerViewModel()

    var body: some View {
        ScrollView {
            VStack(spacing: 0) {
                headerSection
                Spacer().frame(height: 20)
                urlSection
                Spacer().frame(height: 16)
                nowPlayingSection
                Spacer().frame(height: 12)
                seekSection
                Spacer().frame(height: 16)
                transportSection
                Spacer().frame(height: 12)
                rateSection
                Spacer().frame(height: 16)
                volumeSection
                Spacer().frame(height: 16)
                tabbedSection
                Spacer().frame(height: 16)
                errorSection
            }
            .padding(18)
            .frame(maxWidth: .infinity)
        }
        .scrollIndicators(.hidden)
        .background(Color.kitharaBg.ignoresSafeArea())
    }

    // MARK: - Tabs

    enum Tab: String, CaseIterable, Identifiable {
        case playlist = "Playlist"
        case eq = "EQ"
        case settings = "Settings"

        var id: String { rawValue }
    }

    @State private var selectedTab: Tab = .playlist

    private var tabbedSection: some View {
        VStack(spacing: 10) {
            HStack(spacing: 6) {
                ForEach(Tab.allCases) { tab in
                    Button {
                        selectedTab = tab
                    } label: {
                        Text(tab.rawValue)
                            .font(.system(size: 12, weight: selectedTab == tab ? .bold : .regular))
                            .foregroundStyle(selectedTab == tab ? Color.kitharaBg : Color.kitharaMuted)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 7)
                            .frame(maxWidth: .infinity)
                            .background(selectedTab == tab ? Color.kitharaGold : Color.kitharaPanel)
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                    .buttonStyle(.plain)
                }
            }

            Group {
                switch selectedTab {
                case .playlist: playlistSection
                case .eq: eqSection
                case .settings: settingsSection
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .frame(height: 380)
    }

    // MARK: - Header

    private var headerSection: some View {
        HStack(spacing: 8) {
            Text("Kithara")
                .font(.system(size: 28, weight: .bold))
                .foregroundStyle(viewModel.isPlaying ? Color.kitharaGold : .kitharaLight)
            Text("Demo")
                .font(.system(size: 12))
                .foregroundStyle(Color.kitharaMuted)
                .offset(y: 6)
            Spacer()
            statusBadge
        }
    }

    private var statusBadge: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(statusColor)
                .frame(width: 8, height: 8)
            Text(statusText)
                .font(.system(size: 11))
                .foregroundStyle(Color.kitharaMuted)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(Color.kitharaPanel)
        .clipShape(Capsule())
    }

    // MARK: - URL Input

    private var urlSection: some View {
        HStack(spacing: 10) {
            TextField("Audio URL", text: $viewModel.urlText)
                .textFieldStyle(.plain)
                .font(.system(size: 14))
                .foregroundStyle(Color.kitharaLight)
                .padding(10)
                .background(Color.kitharaPanel)
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .autocorrectionDisabled()
                #if os(iOS)
                .textInputAutocapitalization(.never)
                .keyboardType(.URL)
                #endif

            Button {
                viewModel.addTrack()
            } label: {
                Text("Add")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Color.kitharaBg)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .background(Color.kitharaGold)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            .buttonStyle(.plain)
            .disabled(viewModel.urlText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    // MARK: - Now Playing

    private var nowPlayingSection: some View {
        HStack(spacing: 8) {
            Image(systemName: "music.note")
                .font(.system(size: 16))
                .foregroundStyle(Color.kitharaGold)
            VStack(alignment: .leading, spacing: 2) {
                Text(viewModel.trackName)
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(Color.kitharaLight)
                    .lineLimit(1)
                if let variant = viewModel.currentVariantLabel {
                    Text(variant)
                        .font(.system(size: 11))
                        .foregroundStyle(Color.kitharaMuted)
                }
            }
            Spacer()
        }
        .padding(12)
        .background(Color.kitharaPanel)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    // MARK: - Seek

    private var seekSection: some View {
        VStack(spacing: 4) {
            if let duration = viewModel.duration, duration > 0 {
                Slider(
                    value: $viewModel.currentTime,
                    in: 0...duration
                ) { editing in
                    if editing {
                        viewModel.onSeekStarted()
                    } else {
                        viewModel.onSeekEnded(viewModel.currentTime)
                    }
                }
                .tint(.kitharaGold)
            } else {
                Slider(value: .constant(0), in: 0...1)
                    .tint(.kitharaGold)
                    .disabled(true)
            }

            HStack {
                Text(viewModel.formattedCurrentTime)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(Color.kitharaMuted)
                Spacer()
                Text(viewModel.formattedDuration)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(Color.kitharaMuted)
            }
        }
    }

    // MARK: - Transport Controls

    private var transportSection: some View {
        HStack(spacing: 32) {
            Button {
                viewModel.playPrev()
            } label: {
                Image(systemName: "backward.fill")
                    .font(.system(size: 22))
                    .foregroundStyle(Color.kitharaLight)
            }
            .buttonStyle(.plain)

            Button {
                viewModel.togglePlayPause()
            } label: {
                Image(systemName: viewModel.isPlaying ? "pause.fill" : "play.fill")
                    .font(.system(size: 28))
                    .foregroundStyle(Color.kitharaBg)
                    .frame(width: 64, height: 64)
                    .background(Color.kitharaGold)
                    .clipShape(Circle())
            }
            .buttonStyle(.plain)

            Button {
                viewModel.playNext()
            } label: {
                Image(systemName: "forward.fill")
                    .font(.system(size: 22))
                    .foregroundStyle(Color.kitharaLight)
            }
            .buttonStyle(.plain)
        }
        .frame(maxWidth: .infinity)
    }

    // MARK: - Rate

    private var rateSection: some View {
        HStack(spacing: 6) {
            ForEach(PlayerViewModel.availableRates, id: \.self) { rate in
                Button {
                    viewModel.setRate(rate)
                } label: {
                    Text(rateLabel(rate))
                        .font(.system(size: 12, weight: viewModel.selectedRate == rate ? .bold : .regular))
                        .foregroundStyle(viewModel.selectedRate == rate ? Color.kitharaBg : .kitharaMuted)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .background(viewModel.selectedRate == rate ? Color.kitharaGold : Color.kitharaPanel)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: - Volume

    private var volumeSection: some View {
        HStack(spacing: 10) {
            Button {
                viewModel.toggleMute()
            } label: {
                Image(systemName: viewModel.volumeIcon)
                    .font(.system(size: 16))
                    .foregroundStyle(viewModel.isMuted ? Color.kitharaMuted : Color.kitharaGold)
                    .frame(width: 24)
            }
            .buttonStyle(.plain)

            Slider(value: $viewModel.volume, in: 0...1, step: 0.01) { editing in
                if !editing {
                    viewModel.commitVolume()
                }
            }
            .tint(.kitharaGold)

            Text("\(Int(viewModel.volume * 100))%")
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(Color.kitharaMuted)
                .frame(width: 36, alignment: .trailing)
        }
        .padding(12)
        .background(Color.kitharaPanel)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    // MARK: - EQ

    private var eqSection: some View {
        VStack(spacing: 8) {
            HStack {
                Text("EQ")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Color.kitharaLight)
                Spacer()
                Button {
                    viewModel.resetEq()
                } label: {
                    Text("Reset")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.kitharaMuted)
                }
                .buttonStyle(.plain)
            }

            HStack(spacing: 4) {
                ForEach(0..<viewModel.eqGains.count, id: \.self) { band in
                    VStack(spacing: 6) {
                        Slider(
                            value: Binding(
                                get: { viewModel.eqGains[band] },
                                set: { viewModel.setEqGain(band: band, db: $0) }
                            ),
                            in: -24...6,
                            step: 0.5
                        )
                        .frame(width: 140, height: 28)
                        .rotationEffect(.degrees(-90))
                        .frame(width: 28, height: 140)

                        Text(eqBandLabel(band))
                            .font(.system(size: 9))
                            .foregroundStyle(Color.kitharaMuted)
                    }
                }
            }
            .frame(maxWidth: .infinity)
        }
        .padding(12)
        .background(Color.kitharaPanel)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private func eqBandLabel(_ band: Int) -> String {
        let count = viewModel.eqGains.count
        guard count > 0 else { return "" }
        let minFreq: Double = 30
        let maxFreq: Double = 18000
        let freq = minFreq * pow(maxFreq / minFreq, Double(band) / Double(max(count - 1, 1)))
        if freq >= 1000 {
            return String(format: "%.0fk", freq / 1000)
        }
        return String(format: "%.0f", freq)
    }

    // MARK: - Quality

    private var qualitySection: some View {
        HStack(spacing: 6) {
            Button {
                viewModel.setAbrMode(variantIndex: nil)
            } label: {
                Text("Auto")
                    .font(.system(size: 12, weight: viewModel.abrIsAuto ? .bold : .regular))
                    .foregroundStyle(viewModel.abrIsAuto ? Color.kitharaBg : .kitharaMuted)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 5)
                    .background(viewModel.abrIsAuto ? Color.kitharaGold : Color.kitharaPanel)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            }
            .buttonStyle(.plain)

            ForEach(viewModel.discoveredVariants, id: \.index) { variant in
                let isSelected = !viewModel.abrIsAuto && viewModel.selectedVariantIndex == variant.index
                Button {
                    viewModel.setAbrMode(variantIndex: variant.index)
                } label: {
                    Text(variant.label)
                        .font(.system(size: 12, weight: isSelected ? .bold : .regular))
                        .foregroundStyle(isSelected ? Color.kitharaBg : .kitharaMuted)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .background(isSelected ? Color.kitharaGold : Color.kitharaPanel)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: - Playlist

    @ViewBuilder
    private var playlistSection: some View {
        if viewModel.playlist.isEmpty {
            Text("No tracks in playlist")
                .font(.system(size: 13))
                .foregroundStyle(Color.kitharaMuted)
                .frame(maxWidth: .infinity)
                .frame(height: 60)
                .background(Color.kitharaPanel)
                .clipShape(RoundedRectangle(cornerRadius: 10))
        } else {
            List {
                ForEach(Array(viewModel.playlist.enumerated()), id: \.element.id) { index, entry in
                    playlistRow(entry: entry, index: index)
                        .listRowInsets(EdgeInsets(top: 3, leading: 10, bottom: 3, trailing: 10))
                        .listRowBackground(Color.kitharaPanel)
                        .listRowSeparator(.hidden)
                }
            }
            .listStyle(.plain)
            .scrollIndicators(.hidden)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(Color.kitharaPanel)
            .clipShape(RoundedRectangle(cornerRadius: 10))
        }
    }

    // MARK: - Error

    @ViewBuilder
    private var errorSection: some View {
        if let error = viewModel.errorMessage {
            Text(error)
                .font(.system(size: 12))
                .foregroundStyle(Color.kitharaDanger)
                .multilineTextAlignment(.center)
                .padding(10)
                .frame(maxWidth: .infinity)
                .background(Color.kitharaDanger.opacity(0.1))
                .clipShape(RoundedRectangle(cornerRadius: 8))
        }
    }

    // MARK: - Helpers

    private var statusColor: Color {
        if viewModel.status == .failed { return .kitharaDanger }
        if viewModel.playlist.isEmpty { return .kitharaMuted }
        if viewModel.isPlaying { return .kitharaSuccess }
        return .kitharaGold
    }

    private func rateLabel(_ rate: Float) -> String {
        rate == Float(Int(rate)) ? "\(Int(rate))x" : String(format: "%.2gx", rate)
    }

    private func playlistRow(entry: PlaylistEntry, index: Int) -> some View {
        let isCurrent = entry.id == viewModel.currentTrackId
        let statusColor = trackStatusColor(entry.trackStatus, isCurrent: isCurrent)

        return Button {
            viewModel.selectTrack(entry.id)
        } label: {
            HStack(spacing: 10) {
                Text(String(format: "%02d", index + 1))
                    .font(.system(size: 11, weight: .semibold, design: .monospaced))
                    .foregroundStyle(statusColor ?? (isCurrent ? Color.kitharaGold : .kitharaMuted))

                Text(entry.name)
                    .font(.system(size: 13))
                    .foregroundStyle(statusColor ?? (isCurrent ? Color.kitharaLight : .kitharaMuted))
                    .lineLimit(1)

                Spacer()

                Text(entry.duration.map(formatDuration) ?? "--:--")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Color.kitharaMuted)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 12)
            .frame(maxWidth: .infinity)
            .background(isCurrent ? Color.kitharaGold.opacity(0.16) : Color.kitharaBg)
            .clipShape(RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .swipeActions(edge: .trailing, allowsFullSwipe: true) {
            Button(role: .destructive) {
                viewModel.removeTrack(entry.id)
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
    }

    private func trackStatusColor(_ status: TrackStatus?, isCurrent: Bool) -> Color? {
        switch status {
        case .slow: .kitharaWarning
        case .failed: .kitharaDanger
        default: nil
        }
    }

    // MARK: - Settings

    private var settingsSection: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Quality")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Color.kitharaLight)
                qualitySection
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Crossfade")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(Color.kitharaLight)
                HStack(spacing: 10) {
                    Slider(
                        value: Binding(
                            get: { viewModel.crossfadeDuration },
                            set: { viewModel.setCrossfadeDuration($0) }
                        ),
                        in: PlayerViewModel.crossfadeRange,
                        step: 0.1
                    )
                    .tint(.kitharaGold)

                    Text(String(format: "%.1fs", viewModel.crossfadeDuration))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.kitharaMuted)
                        .frame(width: 40, alignment: .trailing)
                }
            }

            Spacer()
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(Color.kitharaPanel)
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private func formatDuration(_ seconds: TimeInterval) -> String {
        guard seconds.isFinite, seconds > 0 else { return "--:--" }
        let mins = Int(seconds) / 60
        let secs = Int(seconds) % 60
        return String(format: "%d:%02d", mins, secs)
    }

    private var statusText: String {
        if viewModel.status == .failed { return "Failed" }
        if viewModel.playlist.isEmpty { return "Not Ready" }
        return viewModel.isPlaying ? "Playing" : "Idle"
    }
}
