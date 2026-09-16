// Posts a system notification when a transfer or batch ends
// (docs/ux-fix-plan.md, item 2). `EngineModel` calls this from
// `reloadTransfers`, which already knows which group just moved to Done
// or Failed.
//
// The words are the ones `TransferRow` already shows for that state: bytes
// and duration for Done, the error's `whatStopped` for Failed. No English
// is written here, the same rule every other file in Ferry follows.

import Foundation
import UserNotifications

enum TransferNotifier {
    /// Asks for authorization to post alerts. Safe to call more than once:
    /// the system asks the person only the first time, and answers every
    /// later call from what it already knows.
    static func requestAuthorization() {
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    /// Posts one notification for a batch or single transfer that just
    /// ended. Does nothing for a group that is not Done or Failed.
    static func notify(group: TransferGroupSnapshot) {
        guard let body = body(for: group) else { return }
        let content = UNMutableNotificationContent()
        content.title = group.label
        content.body = body
        let request = UNNotificationRequest(identifier: group.id, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    /// "43 files · 4.8 GB · 3 min" for Done, or the error's first line for
    /// Failed. The same parts `TransferRow` shows, in the same order. A
    /// Failed group with no error still posts one, with the words
    /// `ThreePartError` uses for a code it does not recognize: item 2
    /// promises one notification for every ending, not only the ones with
    /// a known cause. `docs/audits/ux-gestures.md`, finding 15.
    private static func body(for group: TransferGroupSnapshot) -> String? {
        switch group.state {
        case .done:
            return group.doneSummary
        case .failed:
            return group.error?.whatStopped ?? S.common.unknownErrorStopped
        case .queued, .active, .paused:
            return nil
        }
    }
}
