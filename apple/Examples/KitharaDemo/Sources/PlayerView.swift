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
}

// MARK: - Player View

struct PlayerView: View {
    @StateObject private var viewModel = PlayerViewModel()

    var body: some View {
        VStack(spacing: 0) {
            headerSection
            Spacer().frame(height: 24)
            urlSection
            Spacer().frame(height: 20)
            nowPlayingSection
            Spacer().frame(height: 16)
            seekSection
            Spacer().frame(height: 20)
            transportSection
            Spacer().frame(height: 16)
            rateSection
            Spacer().frame(height: 20)
            volumeSection
            Spacer()
            errorSection
        }
        .padding(18)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.kitharaBg)
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
                viewModel.loadAndPlay()
            } label: {
                Text("Load")
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
        VStack(spacing: 6) {
            HStack(spacing: 8) {
                Image(systemName: "music.note")
                    .font(.system(size: 16))
                    .foregroundStyle(Color.kitharaGold)
                Text(viewModel.trackName)
                    .font(.system(size: 18, weight: .medium))
                    .foregroundStyle(Color.kitharaLight)
                    .lineLimit(1)
                Spacer()
            }

            if let itemId = viewModel.currentItemId {
                HStack {
                    Text("ID: \(itemId.prefix(8))…")
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.kitharaMuted)
                    Spacer()
                }
            }
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
            Spacer()
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
            Spacer()
        }
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
        switch viewModel.status {
        case .readyToPlay: .kitharaSuccess
        case .failed: .kitharaDanger
        case .unknown: .kitharaMuted
        }
    }

    private func rateLabel(_ rate: Float) -> String {
        rate == Float(Int(rate)) ? "\(Int(rate))x" : String(format: "%.2gx", rate)
    }

    private var statusText: String {
        switch viewModel.status {
        case .readyToPlay: "Ready"
        case .failed: "Failed"
        case .unknown: "Not Ready"
        }
    }
}
