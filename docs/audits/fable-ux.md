# UX audit, what a person meets

Date: 11 September 2026
Commit: 3cd5451
Scope: both apps as a person meets them. First run and its prompts, pairing by scan and by code, presence and the phone's notification, transfers and retry, the Finder mount, the Files app, Settings, forgetting a device, the access log, and every row in design/errors.json.
Method: read only, on the main checkout. The docs were read first as the promises. Then the Mac app under macos/Ferry and the phone app under android/app/src/main/kotlin/app/ferry were read against them. Nothing was run. Each row says confirmed when a complete path was read, or plausible with the step that was not read. Rows are ranked by how many people would hit them. High is rows 1 to 2, medium is rows 3 to 6, low is the rest.

| No. | Platform | Screen and state | Where | What happens | Fix |
|---|---|---|---|---|---|
| 1 | Phone | Devices, first tap on Pair | FerryApp.kt:175-179, MainActivity.kt:170-176, strings.xml | The tap opens the system location prompt. No screen says why Ferry wants location. First run said "Ferry needs two things." This is a third thing with no words. The IA first run lists seven steps and this prompt is not one of them. Confirmed. | Add one line before the prompt, on the Pairing choosing screen: "Location, so Ferry can read the Wi-Fi name." Change the first run title to name three things, or state on it that location is asked at pairing. |
| 2 | Phone | Notification, advertising off | ReachableService.kt:49-52, 142-152, strings.xml | Stopping advertising stops the service. The notification vanishes. The IA promises "Not advertising. Pixel 3 XL cannot be found on Wi-Fi." with one action, "Start advertising". No string for that state exists. A person who tapped Stop from the shade has no way back without opening the app. Confirmed. | Keep a notification in the off state with the promised words and a "Start advertising" action. Add the string to strings.xml. |
| 3 | Phone | Settings, Paired | SettingsScreen.kt:177-182, strings.xml action_forget_this_device | The control reads "Forget this device". The IA and voice rule 4 say "Forget this Mac". The Mac says "Forget this phone". Same event, different words. Confirmed. | Rename the string to "Forget this Mac". |
| 4 | Mac | Settings, Shared folders, one root left | SettingsView.swift:27, 158-162 | "Stop sharing" is disabled when one folder remains. Nothing says why. The person sees a grey control and no reason. Confirmed. | Add a caption under the row: "At least one folder is shared." Or hide the control when it cannot act. |
| 5 | Phone | Pairing, by code, before advertising is on | PairingScreen.kt:104-108, Mapping.kt:275-279, FerryApp.kt:175-177 | Pair turns advertising on, then the screen shows "Pairing on. Open Ferry on the Mac." before the engine has started pairing. No countdown shows. If the service fails to start, the words stay and nothing happens. Plausible. The service failure path was not traced. | While reachable is false, show "Starting." instead of "Pairing on." If reachable stays false, show the engine error. |
| 6 | Both | Any error the table does not know | Strings.swift common.unknownError*, strings.xml error_unknown_* | The Mac says "The action stopped. Ferry has no words for the code X. Report this code." The phone says "Ferry has a fault. It reported the code X, which this version does not know. Report this. Nothing you did caused it." Same event, two sets of words. Voice says one event, one wording. Confirmed. | Pick one set and use it in both files. |
| 7 | Mac | Start, stored key wrong size | Strings.swift keyStore.wrongSizeToDo, errors.json NoiseError::BadKeyLength | The same stop line has two instructions. Keychain words say "Delete the Ferry device key in Keychain Access." The table says "Forget every device and pair again." A person cannot know which applies. Confirmed by reading both files. | Use one instruction in both places. |
| 8 | Both | Any fault error | errors.json ChunkSizeError::*, NoiseError::BadPattern, strings.xml error_unknown_todo | The "todo" says "Report this." No screen offers a way to report. Plausible. No report control was found in either app. | Say where to report, or say "Restart Ferry." |
| 9 | Both | Connection refused, not Ferry | errors.json VersionError::NotFerry | The "todo" says "Check the address." No screen shows or accepts an address. Plausible. | Say "Check that Ferry is open on the other device." |
| 10 | Mac | Files, file not found | errors.json OpError::NotFound, DeviceDetail.swift:175-178 | The "todo" says "Refresh and try again." The Files section has no control named Refresh. The error offers Retry. Confirmed. | Change the todo to "Try again." |
| 11 | Phone | Notification, advertising, quiet on this network | ReachableService.kt:153-155 | The notification adds "Ferry is quiet on this network." The IA says the notification shows only the first line, and the quiet lines show only in Settings, Networks. Confirmed. | Drop the second line from the notification, or change the IA. |
| 12 | Phone | Pairing, choosing | strings.xml pairing_choose_body, PairingScreen.kt:301-305 | The body says "Scanning is quicker when the Mac is in front of you." Voice rule 5 bans an adjective standing in for a number. The IA lists two controls and no body. Confirmed. | Remove the body, or state a count: "Scanning is three steps. A code is five." |
| 13 | Phone | Files app, file opened in an unsupported mode | strings.xml documents_open_mode_unsupported, FerryDocumentsProvider.kt:159 | The error has two parts and no "what to do". "Open mode" is not defined. Confirmed. | Add a third part: "Open the file read only, or copy it first." |
| 14 | Phone | Notification, advertising | ReachableService.kt:142 | The title uses Build.MODEL as the phone's name. Settings lets the person edit the name. After an edit the notification still shows the model. Plausible. The name setter was not traced. | Read the name the engine holds. |

## Found right

- The errors table has a row for every listed error and no empty cell.
- The phone reads the camera refusal and the four scan errors from the table and composes no words of its own. PairingScreen.kt:167-171.
- The phone Devices screen shows the all files access error with an "Open settings" action that opens the system screen. FerryApp.kt:136-138.
- The phone pairing Failed state offers the other method as a second control. PairingScreen.kt:257-267.
- The phone Scanned state offers only Cancel, as the IA says. PairingScreen.kt:205-207.
- The Mac Settings, Networks, offers "Open Location Settings" when the name cannot be read. SettingsView.swift:88-90.
- The Mac presence control states the consequence when advertising is off. PresenceControl.swift:43-44.
- The Mac Access section is absent when the mount is not ready. AccessSection.swift:19.
- "Go up" is disabled at the roots, as the IA says. DeviceDetail.swift:134.
- Access log verbs and the retention line match on both platforms. Strings.swift accessLog, strings.xml access_verb_*.
- The phone first run has Skip, and Skip lands on Devices. FirstRunScreen.kt:92-100, MainActivity.kt:83.
