// Says how a device is reachable right now (docs/components.md,
// TransportBadge). Appears on every DeviceRow and on every active
// transfer's ProgressLine.

import SwiftUI

enum TransportBadgeState: Equatable {
    case usbIdle
    case usbMoving(speedBytesPerSec: Int)
    case wifiIdle
    case wifiMoving(speedBytesPerSec: Int)
    case connecting
    case notReachable
}

struct TransportBadge: View {
    let state: TransportBadgeState

    var body: some View {
        HStack(spacing: FerrySpace.s1) {
            Image(systemName: icon)
            Text(label)
                .font(isMoving ? FerryFont.mono : FerryFont.label)
        }
        .foregroundStyle(state == .notReachable ? FerryColor.textSecondary : FerryColor.text)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(TransportBadge.accessibilityText(for: state))
    }

    private var isMoving: Bool {
        switch state {
        case .usbMoving, .wifiMoving: return true
        default: return false
        }
    }

    private var icon: String {
        switch state {
        case .usbIdle, .usbMoving: return FerryIcon.usb
        case .wifiIdle, .wifiMoving: return FerryIcon.wifi
        case .connecting: return FerryIcon.wifi
        case .notReachable: return FerryIcon.notReachable
        }
    }

    private var label: String {
        switch state {
        case .usbIdle:
            return S.transportBadge.usb
        case .usbMoving(let speed):
            return S.transportBadge.usbMoving(speed: FerryFormat.speed(bytesPerSec: speed))
        case .wifiIdle:
            return S.transportBadge.wifi
        case .wifiMoving(let speed):
            return S.transportBadge.wifiMoving(speed: FerryFormat.speed(bytesPerSec: speed))
        case .connecting:
            return S.common.connecting
        case .notReachable:
            return S.common.notReachable
        }
    }

    /// The exact words docs/components.md says a screen reader speaks for
    /// each state. DeviceRow reuses this to fold the badge into its own
    /// combined accessibility label, instead of nesting two elements.
    static func accessibilityText(for state: TransportBadgeState) -> String {
        switch state {
        case .usbIdle:
            return S.transportBadge.accessibilityUSB
        case .usbMoving(let speed):
            return S.transportBadge.accessibilityUSBSpeed(FerryFormat.speedSpoken(bytesPerSec: speed))
        case .wifiIdle:
            return S.transportBadge.accessibilityWifi
        case .wifiMoving(let speed):
            return S.transportBadge.accessibilityWifiSpeed(FerryFormat.speedSpoken(bytesPerSec: speed))
        case .connecting:
            return S.common.connecting
        case .notReachable:
            return S.common.notReachable
        }
    }
}

extension TransportBadgeState {
    /// Builds the badge state a DeviceInfo shows in the Devices list.
    init(device: DeviceInfo) {
        switch device.reachableVia {
        case .usb:
            if let speed = device.speedBytesPerSec {
                self = .usbMoving(speedBytesPerSec: speed)
            } else {
                self = .usbIdle
            }
        case .wifi:
            if let speed = device.speedBytesPerSec {
                self = .wifiMoving(speedBytesPerSec: speed)
            } else {
                self = .wifiIdle
            }
        case nil:
            self = .notReachable
        }
    }

    /// Builds the badge state an active transfer shows next to its
    /// ProgressLine.
    init(transfer: TransferInfo) {
        switch transfer.transport {
        case .usb:
            if let speed = transfer.speedBytesPerSec {
                self = .usbMoving(speedBytesPerSec: speed)
            } else {
                self = .usbIdle
            }
        case .wifi:
            if let speed = transfer.speedBytesPerSec {
                self = .wifiMoving(speedBytesPerSec: speed)
            } else {
                self = .wifiIdle
            }
        }
    }
}

#Preview {
    VStack(alignment: .leading, spacing: FerrySpace.s3) {
        TransportBadge(state: .usbIdle)
        TransportBadge(state: .usbMoving(speedBytesPerSec: 38_000_000))
        TransportBadge(state: .wifiIdle)
        TransportBadge(state: .wifiMoving(speedBytesPerSec: 24_000_000))
        TransportBadge(state: .connecting)
        TransportBadge(state: .notReachable)
    }
    .padding()
}
