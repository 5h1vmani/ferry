// Whether this Mac advertises and accepts connections, and what that costs
// when it does not (docs/components.md, PresenceControl).
//
// Four call sites read this one value: the sidebar footer, the menu bar
// item, and — on the phone — the home screen and the notification. That is
// why it is a component and not four views.
//
// It never holds its own switch state. A switch with local state and a
// remote truth drifts, and reachability is the one fact a person turns off
// in a hurry and then has to trust. The binding goes straight to the model.

import SwiftUI

struct PresenceControl: View {
    let presence: PresenceSnapshot
    let onChange: (Bool) -> Void

    /// The menu bar has room for a speed; the sidebar footer does not.
    var showsSpeed = false
    /// False before the engine's first snapshot arrives. `presence` is
    /// `.unknown` until then, which is not the same fact as "not
    /// reachable": nobody has asked the engine yet, so this draws a
    /// neutral line instead of a switch that is really off.
    /// `docs/audits/oss-looks.md`, M6.
    var isReady = true

    var body: some View {
        Group {
            if isReady {
                ready
            } else {
                starting
            }
        }
        .padding(!isReady || presence.isAdvertising ? FerrySpace.s1 : FerrySpace.s3)
        .background(background)
    }

    private var ready: some View {
        VStack(alignment: .leading, spacing: FerrySpace.s1) {
            Toggle(isOn: binding) {
                HStack(spacing: FerrySpace.s2) {
                    Image(systemName: presence.isAdvertising ? FerryIcon.wifi : FerryIcon.advertisingOff)
                        .foregroundStyle(FerryColor.text)
                    Text(label)
                        .font(FerryFont.body)
                        .foregroundStyle(FerryColor.text)
                    if showsSpeed, let speed = presence.speedBytesPerSec, speed > 0 {
                        Spacer()
                        Text(FerryFormat.speed(bytesPerSec: speed))
                            .font(FerryFont.mono)
                            .foregroundStyle(FerryColor.textSecondary)
                    }
                }
            }
            .toggleStyle(.switch)

            // Stated because the failure it causes is silent: Wi-Fi
            // transfers stop working and a person who cannot see why has
            // no way to guess. Never coloured: nothing went wrong.
            if !presence.isAdvertising {
                Text(S.presence.consequence)
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else if presence.isQuietOnThisNetwork {
                // Same reasoning, for the quiet-on-this-network state
                // (docs/engine-contract.md, item 18): the words are the
                // same ones Settings, Networks shows for this state.
                Text(presence.quietOnThisNetworkLine)
                    .font(FerryFont.caption)
                    .foregroundStyle(FerryColor.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityLabel)
        // Tells a screen reader what the switch does, as a verb, next to
        // the state `accessibilityLabel` already states. `docs/voice.md`
        // rule 9.
        .accessibilityHint(presence.isAdvertising ? S.presence.stopBeingReachable : S.presence.becomeReachable)
    }

    /// Before the first snapshot: no switch, because there is nothing yet
    /// to turn on or off, only a fact still being read.
    private var starting: some View {
        HStack(spacing: FerrySpace.s2) {
            Image(systemName: FerryIcon.wifi)
                .foregroundStyle(FerryColor.textSecondary)
            Text(S.common.starting)
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.textSecondary)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(S.common.starting)
    }

    private var binding: Binding<Bool> {
        Binding(
            get: { presence.isAdvertising },
            set: { onChange($0) }
        )
    }

    private var label: String {
        presence.isAdvertising ? S.presence.advertising : S.presence.notAdvertising
    }

    /// Off, the block lifts out of the sidebar with one step of grey and
    /// one hairline, which is how this design system distinguishes a
    /// surface (readme.md, Background and imagery).
    @ViewBuilder
    private var background: some View {
        if !isReady || presence.isAdvertising {
            Color.clear
        } else {
            RoundedRectangle(cornerRadius: FerryRadius.medium)
                .fill(FerryColor.surfaceRaised)
                .overlay(
                    RoundedRectangle(cornerRadius: FerryRadius.medium)
                        .strokeBorder(FerryColor.borderStrong, lineWidth: 1)
                )
        }
    }

    private var accessibilityLabel: String {
        guard presence.isAdvertising else {
            return S.presence.accessibilityOff(consequence: S.presence.consequence)
        }
        guard presence.isQuietOnThisNetwork else {
            return S.presence.accessibilityOn
        }
        return S.presence.accessibilityQuiet(why: presence.quietOnThisNetworkLine)
    }
}

#if DEBUG
#Preview {
    VStack(alignment: .leading, spacing: FerrySpace.s5) {
        PresenceControl(
            presence: PresenceSnapshot(
                isAdvertising: true,
                activeTransport: .usb,
                speedBytesPerSec: 38_000_000,
                networkName: "Home",
                isWifiPresenceOn: true,
                adbPresent: true
            ),
            onChange: { _ in },
            showsSpeed: true
        )
        PresenceControl(
            presence: PresenceSnapshot(
                isAdvertising: true,
                activeTransport: nil,
                speedBytesPerSec: nil,
                networkName: "Café Wifi",
                isWifiPresenceOn: false,
                adbPresent: true
            ),
            onChange: { _ in }
        )
        PresenceControl(presence: .unknown, onChange: { _ in })
        PresenceControl(presence: .unknown, onChange: { _ in }, isReady: false)
    }
    .frame(width: 232)
    .padding()
}
#endif
