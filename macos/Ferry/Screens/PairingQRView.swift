// The square code the phone scans, and the one question the Mac answers
// afterwards (docs/ia.md, Pairing, the Mac, by scan).
//
// The code is rendered by the platform, from bytes the engine serialises.
// Nothing is drawn by hand and nothing here parses the payload: the view
// draws what it is given, which is why it stays a view and not a component
// (docs/components.md, Rejected).
//
// Why no digits in this flow. Comparing six digits proves the same person
// holds both devices. Scanning the Mac's screen proves it too, and earlier:
// the phone learns the Mac's static key out of band, so there is no window
// in which a wrong confirm accepts a stranger. What remains is the Mac's
// half of the trust, and that is one named question with two answers.

import SwiftUI
import AppKit
import CoreImage
import CoreImage.CIFilterBuiltins

struct PairingQRView: View {
    let offer: PairingOfferSnapshot
    let onUseCode: () -> Void
    let onCancel: () -> Void

    var body: some View {
        VStack(spacing: FerrySpace.s5) {
            code

            VStack(spacing: FerrySpace.s1) {
                Text(S.pairing.scanThis)
                    .font(FerryFont.body)
                    .foregroundStyle(FerryColor.text)

                // The expiry is a count, not "soon".
                if let expiresIn = offer.expiresIn {
                    Text(S.pairing.expiresIn(expiresIn))
                        .font(FerryFont.caption)
                        .foregroundStyle(FerryColor.textSecondary)
                }
            }

            HStack(spacing: FerrySpace.s3) {
                Button(S.pairing.useCodeInstead, action: onUseCode)
                    .buttonStyle(.link)
                Spacer()
                Button(S.common.cancel, action: onCancel)
                    .buttonStyle(.bordered)
            }
        }
    }

    @ViewBuilder
    private var code: some View {
        if let image = PairingQRView.image(from: offer.payload) {
            Image(nsImage: image)
                .interpolation(.none)
                .resizable()
                .frame(width: 220, height: 220)
                .padding(FerrySpace.s3)
                .background(Color.white)
                .overlay(
                    RoundedRectangle(cornerRadius: FerryRadius.medium)
                        .strokeBorder(FerryColor.border, lineWidth: 1)
                )
                .accessibilityLabel(S.pairing.accessibilityCode)
        } else {
            // The payload could not be encoded, which is not a pairing
            // failure and has no cause worth guessing at. The code method
            // is the way onward.
            ErrorBlock(
                error: ThreePartError(
                    whatStopped: S.pairing.codeNotShownStopped,
                    why: S.pairing.codeNotShownWhy,
                    whatToDo: S.pairing.codeNotShownToDo,
                    canRetry: false
                )
            )
        }
    }

    /// The platform's own generator. `CIQRCodeGenerator` with high error
    /// correction, scaled with no interpolation so the modules stay square.
    private static func image(from payload: Data) -> NSImage? {
        guard !payload.isEmpty else { return nil }
        let filter = CIFilter.qrCodeGenerator()
        filter.message = payload
        filter.correctionLevel = "H"
        guard let output = filter.outputImage else { return nil }
        let scale = 10.0
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext()
        guard let cgImage = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return NSImage(cgImage: cgImage, size: NSSize(width: scaled.extent.width, height: scaled.extent.height))
    }
}

/// The one question the Mac asks after a phone scans: pair with this named
/// device, or refuse. No digits, because the scan already proved what the
/// digits prove.
struct PairingRequestView: View {
    let name: String
    let kind: DeviceKind
    let transport: Transport
    let onPair: () -> Void
    let onRefuse: () -> Void

    private var icon: String {
        switch kind {
        case .phone: return FerryIcon.devicePhone
        case .mac: return FerryIcon.deviceMac
        }
    }

    var body: some View {
        VStack(spacing: FerrySpace.s3) {
            Image(systemName: icon)
                .font(.system(size: 32))
                .foregroundStyle(FerryColor.accent)

            Text(S.pairing.wantsToPair(name: name))
                .font(FerryFont.title)
                .foregroundStyle(FerryColor.text)

            Text(S.pairing.scannedOver(transport: transport))
                .font(FerryFont.body)
                .foregroundStyle(FerryColor.textSecondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: FerrySpace.s3) {
                Button(S.pairing.refuse, action: onRefuse)
                    .buttonStyle(.bordered)
                Button(S.pairing.pair, action: onPair)
                    .buttonStyle(.borderedProminent)
            }
            .padding(.top, FerrySpace.s2)
        }
        .frame(maxWidth: 360)
        .accessibilityElement(children: .contain)
    }
}

#if DEBUG
#Preview("Offering") {
    PairingQRView(
        offer: PreviewData.offer,
        onUseCode: {},
        onCancel: {}
    )
    .padding(FerrySpace.s6)
    .frame(width: 460)
}

#Preview("Requested") {
    PairingRequestView(name: "Pixel 3 XL", kind: .phone, transport: .usb, onPair: {}, onRefuse: {})
        .padding(FerrySpace.s6)
        .frame(width: 460)
}
#endif
