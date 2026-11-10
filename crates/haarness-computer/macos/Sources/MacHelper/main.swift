import AppKit
import MacKit

// haarness-mac speaks newline-delimited JSON: each stdin line is a request, each stdout line an answer.
//
//   {"id": 3, "op": "snapshot", "longest": 1456}  ->  {"id": 3, "ok": true, "data": {...}}
//                                                 or  {"id": 3, "ok": false, "code": "...", "message": "..."}
//
// Lines with "event" and no id report what the person did: "escape", "pause", "resume", "stop".
// An accessory-policy app, with neither Dock icon nor menu bar, so it may draw the bar and the click ring. macOS
// charges its permission use to the app that launched Haarness: Desktop or a terminal. When stdin
// closes it completes the current request and exits, so it cannot outlive its server.

if CommandLine.arguments.dropFirst().first == "--version" {
    print(#"{"protocol":\#(protocolVersion),"build":"\#(helperBuild)"}"#)
    exit(0)
}

let outputLock = NSLock()

func emit(_ value: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: value, options: []) else { return }
    outputLock.lock()
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
    outputLock.unlock()
}

let dispatcher = Dispatcher()
let worker = DispatchQueue(label: "haarness.mac.requests")

Thread.detachNewThread {
    while let line = readLine(strippingNewline: true) {
        if line.isEmpty { continue }
        guard let data = line.data(using: .utf8),
              let request = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let op = request["op"] as? String else {
            emit(["id": NSNull(), "ok": false, "code": "invalid", "message": "The request is not a JSON object with an op."])
            continue
        }
        let id = request["id"] ?? NSNull()
        // End of turn: drop the bar and the display's awake hold immediately rather than after the request in progress. A
        // pause ends with the turn and is reported as resumed, so the following turn starts unpaused.
        if op == "release" {
            let wasPaused = ControlBar.shared.isPaused
            ControlBar.shared.hide()
            KeepAwake.shared.release()
            if wasPaused { emit(["event": "resume"]) }
            emit(["id": id, "ok": true, "data": [String: Any]()])
            continue
        }
        // Resume pressed in a Haarness window rather than on the bar: answered straight away too, since the paused
        // agent waits for it before its next request.
        if op == "resume" {
            ControlBar.shared.resume()
            emit(["id": id, "ok": true, "data": [String: Any]()])
            continue
        }
        // Permission and lock state are answered straight away, even mid-action: the Settings page polls for them every
        // few seconds.
        if op == "status" {
            emit(["id": id, "ok": true, "data": dispatcher.status()])
            continue
        }
        worker.async {
            do {
                emit(["id": id, "ok": true, "data": try dispatcher.handle(op, request)])
            } catch let failure as HelperFailure {
                emit(["id": id, "ok": false, "code": failure.code, "message": failure.message])
            } catch {
                emit(["id": id, "ok": false, "code": "failed", "message": error.localizedDescription])
            }
        }
    }
    worker.sync {}
    exit(0)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
ControlBar.shared.report = { emit(["event": $0]) }
// Escape stops the agent only while it works in the app in front (see Dispatcher.escapeStops).
Presence.shared.start { if dispatcher.escapeStops { emit(["event": "escape"]) } }
app.run()
