import SwiftUI
import ClowderCore

/// Probe a host, show what it presented, and record the user's decision. Nothing is written until
/// they confirm — observing and trusting are deliberately separate acts.
struct PairingSheet: View {
    @ObservedObject var model: HostsViewModel
    @Binding var isPresented: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Pair \(model.selectedHost?.name ?? "host")").font(.headline)

            switch model.pairing {
            case .idle, .probing:
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Contacting \(model.selectedHost?.address ?? "…")")
                }
                .frame(maxWidth: .infinity, alignment: .leading)

            case let .failed(message):
                Label(message, systemImage: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)

            case let .observed(probe):
                observed(probe)
            }

            HStack {
                Button("Cancel") { model.cancelPairing(); isPresented = false }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Trust") {
                    Task {
                        await model.confirmTrust()
                        if model.lastError == nil { isPresented = false }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!model.canTrust || model.isBusy)
            }
        }
        .padding(20)
        .frame(width: 520)
        .task { await model.beginPairing() }
    }

    @ViewBuilder
    private func observed(_ probe: HostProbe) -> some View {
        if let fingerprint = probe.fingerprint {
            Text("This daemon presented:").font(.subheadline)
            Text(groupedFingerprint(fingerprint))
                .font(.system(.body, design: .monospaced))
                .textSelection(.enabled)

            // The load-bearing sentence: without an out-of-band comparison this is TOFU with extra
            // clicks, so name exactly where the real value comes from.
            Text("Compare this with the fingerprint printed by `clowder remote-token` **on the daemon's "
                 + "own machine**, or in that daemon's startup log. Anything else can be forged.")
                .font(.caption).foregroundStyle(.secondary)

            TextField("Paste the expected fingerprint to compare (optional)",
                      text: $model.expectedFingerprint)
                .textFieldStyle(.roundedBorder)
                .font(.system(.caption, design: .monospaced))

            if let matches = model.fingerprintComparison {
                Label(matches ? "Matches" : "Does NOT match — do not trust this host",
                      systemImage: matches ? "checkmark.circle.fill" : "xmark.octagon.fill")
                    .foregroundStyle(matches ? .green : .red)
                    .font(.callout)
            }
        } else if !probe.tls {
            Label("This daemon is not using TLS, so it presents no certificate to pin.",
                  systemImage: "lock.open")
                .foregroundStyle(.orange)
        }

        // Covers both unreachable and "reachable but the TLS handshake never produced a
        // certificate" — the case a plain reachable/tls/fingerprint if-else chain silently
        // dropped, leaving a failed handshake's real error unshown.
        if let displayError = probe.displayError {
            Label(displayError, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
        }

        // Gated on shouldReportAuthentication: `authenticated` is meaningless before the probe
        // got far enough to attempt it (unreachable, or a TLS handshake that never completed), and
        // was otherwise rendering as a spurious "Token rejected" for a connection that never
        // happened.
        if probe.shouldReportAuthentication {
            switch probe.authSummary {
            case .tokenAccepted: Label("Token accepted", systemImage: "checkmark.seal").font(.caption)
            case .tokenRejected: Label("Token rejected", systemImage: "xmark.seal")
                    .font(.caption).foregroundStyle(.red)
            case .nonePlaintext: Label("No authentication (plaintext daemon)", systemImage: "exclamationmark.triangle")
                    .font(.caption).foregroundStyle(.orange)
            }
        }
    }
}
