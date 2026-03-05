import SwiftUI

struct PlayerView: View {
    @StateObject private var viewModel = PlayerViewModel()

    var body: some View {
        VStack(spacing: 20) {
            Text("Kithara Demo")
                .font(.largeTitle)
                .fontWeight(.bold)

            // URL input
            HStack {
                TextField("Audio URL", text: $viewModel.urlText)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    #endif

                Button("Load") {
                    viewModel.loadAndPlay()
                }
                .buttonStyle(.borderedProminent)
                .disabled(viewModel.urlText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }

            // Status
            HStack {
                Circle()
                    .fill(statusColor)
                    .frame(width: 10, height: 10)
                Text(statusText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            // Time display
            Text(viewModel.formattedTime)
                .font(.system(.title2, design: .monospaced))

            // Progress slider — seeks only on drag end to avoid flooding
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
            }

            // Play/Pause button
            Button {
                viewModel.togglePlayPause()
            } label: {
                Image(systemName: viewModel.isPlaying ? "pause.circle.fill" : "play.circle.fill")
                    .font(.system(size: 64))
            }

            // Error display
            if let error = viewModel.errorMessage {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
            }

            Spacer()
        }
        .padding()
    }

    private var statusColor: Color {
        switch viewModel.status {
        case .readyToPlay: .green
        case .failed: .red
        case .unknown: .gray
        }
    }

    private var statusText: String {
        switch viewModel.status {
        case .readyToPlay: "Ready"
        case .failed: "Failed"
        case .unknown: "Not Ready"
        }
    }
}
