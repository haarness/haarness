import Foundation
import AVFoundation
import Speech
import Darwin

// A small desktop-only adapter. The runtime never receives microphone audio.
// JSON events travel over stdout; closing stdin always releases the microphone.
func emit(_ value: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: value) else { return }
    FileHandle.standardOutput.write(data + Data([10]))
}
let args = CommandLine.arguments
func argument(_ name: String) -> String? {
    guard let index = args.firstIndex(of: name), index + 1 < args.count else { return nil }
    return args[index + 1]
}
let locale = Locale(identifier: argument("--locale") ?? Locale.current.identifier)
if args.contains("--probe") {
    if #available(macOS 26.0, *) {
        Task {
            let modern = SpeechTranscriber.isAvailable ? await SpeechTranscriber.supportedLocale(equivalentTo: locale) : nil
            let fallback = modern == nil ? await DictationTranscriber.supportedLocale(equivalentTo: locale) : nil
            emit(["supported": modern != nil || fallback != nil, "available": modern != nil || fallback != nil,
                  "onDevice": true, "engine": modern != nil ? "SpeechTranscriber" : "DictationTranscriber",
                  "microphoneAuthorization": AVCaptureDevice.authorizationStatus(for: .audio).rawValue])
            exit(0)
        }
        RunLoop.main.run()
    }
    let recognizer = SFSpeechRecognizer(locale: locale)
    emit(["supported": recognizer != nil, "available": recognizer?.isAvailable ?? false,
          "onDevice": recognizer?.supportsOnDeviceRecognition ?? false,
          "speechAuthorization": SFSpeechRecognizer.authorizationStatus().rawValue,
          "microphoneAuthorization": AVCaptureDevice.authorizationStatus(for: .audio).rawValue])
    exit(0)
}

protocol VoiceSession {
    func start()
    func stop()
    func finish()
}

final class Dictation: VoiceSession {
    let engine = AVAudioEngine()
    var recognizer: SFSpeechRecognizer?
    var request: SFSpeechAudioBufferRecognitionRequest?
    var task: SFSpeechRecognitionTask?
    var tapped = false
    var ending = false
    var finished = false

    func fail(_ message: String) {
        guard !finished else { return }
        emit(["type": "error", "message": message])
        finish()
    }
    func finish() {
        guard !finished else { return }
        finished = true
        releaseInput()
        task?.cancel()
        emit(["type": "ended"])
        exit(0)
    }
    func releaseInput() {
        engine.stop()
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        request?.endAudio()
    }
    func stop() {
        guard !ending && !finished else { return }
        ending = true
        releaseInput()
        if task == nil { finish(); return }
        // Let the recognizer deliver its final words, without keeping audio open.
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { self.finish() }
    }
    func start() {
        SFSpeechRecognizer.requestAuthorization { status in
            DispatchQueue.main.async {
                guard !self.finished && !self.ending else { return }
                guard status == .authorized else {
                    self.fail(status == .restricted
                      ? "Speech recognition is restricted on this Mac."
                      : "Allow Haarness in System Settings → Privacy & Security → Speech Recognition, then try again.")
                    return
                }
                if argument("--file") != nil { self.recognize(); return }
                AVCaptureDevice.requestAccess(for: .audio) { allowed in
                    DispatchQueue.main.async {
                        guard !self.finished && !self.ending else { return }
                        if allowed { self.recognize() }
                        else { self.fail("Allow Haarness in System Settings → Privacy & Security → Microphone, then try again.") }
                    }
                }
            }
        }
    }
    func recognize() {
        guard let recognizer = SFSpeechRecognizer(locale: locale), recognizer.isAvailable else {
            fail("Speech recognition is unavailable for this language. Check macOS Dictation settings and try again.")
            return
        }
        self.recognizer = recognizer
        let recognition: SFSpeechRecognitionRequest
        if let path = argument("--file") {
            recognition = SFSpeechURLRecognitionRequest(url: URL(fileURLWithPath: path))
        } else {
            let bufferRequest = SFSpeechAudioBufferRecognitionRequest()
            request = bufferRequest
            recognition = bufferRequest
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            guard format.channelCount > 0 && format.sampleRate > 0 else {
                fail("No microphone is available. Choose an input device in macOS Sound settings.")
                return
            }
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
                bufferRequest.append(buffer)
            }
            tapped = true
            engine.prepare()
            do { try engine.start() }
            catch { fail("Couldn’t start the microphone: \(error.localizedDescription)"); return }
        }
        recognition.shouldReportPartialResults = true
        recognition.requiresOnDeviceRecognition = recognizer.supportsOnDeviceRecognition
        task = recognizer.recognitionTask(with: recognition) { result, error in
            DispatchQueue.main.async {
                guard !self.finished else { return }
                if let result = result {
                    emit(["type": "transcript", "text": result.bestTranscription.formattedString, "isFinal": result.isFinal])
                    if result.isFinal { self.finish(); return }
                }
                if let error = error {
                    if self.ending { self.finish() }
                    else if (error as NSError).domain == "kLSRErrorDomain" && (error as NSError).code == 201 {
                        self.fail("Enable Dictation in System Settings → Keyboard → Dictation, then try again. This macOS version requires system dictation for speech recognition.")
                    } else { self.fail("Dictation stopped: \(error.localizedDescription)") }
                }
            }
        }
        emit(["type": "started"])
    }
}
// SpeechAnalyzer manages its own on-device models. Unlike SFSpeechRecognizer,
// it does not require the user to enable Siri or keyboard dictation.
@available(macOS 26.0, *)
final class AnalyzerDictation: VoiceSession {
    let engine = AVAudioEngine()
    var analyzer: SpeechAnalyzer?
    var continuation: AsyncStream<AnalyzerInput>.Continuation?
    var work: Task<Void, Never>?
    var results: Task<Void, Never>?
    var tapped = false
    var ending = false
    var finished = false
    var listening = false
    var finalized = ""
    var transcript = ""

    func finish() {
        guard !finished else { return }
        finished = true
        releaseInput()
        work?.cancel()
        results?.cancel()
        emit(["type": "ended"])
        exit(0)
    }
    func fail(_ message: String) {
        guard !finished else { return }
        emit(["type": "error", "message": message])
        finish()
    }
    func releaseInput() {
        engine.stop()
        if tapped { engine.inputNode.removeTap(onBus: 0); tapped = false }
        continuation?.finish()
    }
    func stop() {
        guard !ending && !finished else { return }
        ending = true
        releaseInput()
        if !listening { finish(); return }
        // Model finalization can take longer than the old recognizer's timeout.
        DispatchQueue.main.asyncAfter(deadline: .now() + 8) { self.finish() }
    }
    func receive(_ text: String, final: Bool) {
        guard !finished else { return }
        transcript = finalized + text
        if final { finalized = transcript }
        emit(["type": "transcript", "text": transcript, "isFinal": false])
    }
    func start() {
        work = Task { @MainActor in
            do { try await run() }
            catch {
                if !finished && !ending {
                    fail("Couldn’t prepare on-device dictation: \(error.localizedDescription)")
                } else if !finished { finish() }
            }
        }
    }
    @MainActor func run() async throws {
        let module: any SpeechModule
        if SpeechTranscriber.isAvailable,
           let language = await SpeechTranscriber.supportedLocale(equivalentTo: locale) {
            let transcriber = SpeechTranscriber(locale: language, preset: .progressiveTranscription)
            module = transcriber
            results = Task { @MainActor in
                do {
                    for try await result in transcriber.results {
                        receive(String(result.text.characters), final: result.isFinal)
                    }
                } catch { if !Task.isCancelled && !finished && !ending { fail("Dictation stopped: \(error.localizedDescription)") } }
            }
        } else if let language = await DictationTranscriber.supportedLocale(equivalentTo: locale) {
            let transcriber = DictationTranscriber(locale: language, preset: .progressiveLongDictation)
            module = transcriber
            results = Task { @MainActor in
                do {
                    for try await result in transcriber.results {
                        receive(String(result.text.characters), final: result.isFinal)
                    }
                } catch { if !Task.isCancelled && !finished && !ending { fail("Dictation stopped: \(error.localizedDescription)") } }
            }
        } else {
            fail("On-device dictation is unavailable for this language.")
            return
        }
        if await AssetInventory.status(forModules: [module]) != .installed,
           let installation = try await AssetInventory.assetInstallationRequest(supporting: [module]) {
            emit(["type": "preparing", "message": "Downloading speech model…"])
            try await installation.downloadAndInstall()
        }
        try Task.checkCancellation()
        guard !ending && !finished else { return }
        let analyzer = SpeechAnalyzer(modules: [module])
        self.analyzer = analyzer
        let last: CMTime?
        if let path = argument("--file") {
            let file = try AVAudioFile(forReading: URL(fileURLWithPath: path))
            listening = true
            emit(["type": "started"])
            last = try await analyzer.analyzeSequence(from: file)
        } else {
            guard await AVCaptureDevice.requestAccess(for: .audio) else {
                fail("Allow Haarness in System Settings → Privacy & Security → Microphone, then try again.")
                return
            }
            guard !ending && !finished else { return }
            let input = engine.inputNode
            let source = input.outputFormat(forBus: 0)
            guard source.channelCount > 0 && source.sampleRate > 0 else {
                fail("No microphone is available. Choose an input device in macOS Sound settings.")
                return
            }
            guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [module], considering: source),
                  let converter = AVAudioConverter(from: source, to: format) else {
                fail("This microphone’s audio format is unsupported.")
                return
            }
            try await analyzer.prepareToAnalyze(in: format)
            guard !ending && !finished else { return }
            let (stream, continuation) = AsyncStream<AnalyzerInput>.makeStream()
            self.continuation = continuation
            input.installTap(onBus: 0, bufferSize: 1024, format: source) { buffer, _ in
                // Every queued input owns its PCM storage; the engine reuses tap buffers.
                let capacity = AVAudioFrameCount(ceil(Double(buffer.frameLength) * format.sampleRate / source.sampleRate)) + 16
                guard let converted = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return }
                var supplied = false
                var error: NSError?
                let status = converter.convert(to: converted, error: &error) { _, status in
                    if supplied { status.pointee = .noDataNow; return nil }
                    supplied = true
                    status.pointee = .haveData
                    return buffer
                }
                if status == .error {
                    DispatchQueue.main.async { self.fail("Couldn’t convert microphone audio.") }
                } else if converted.frameLength > 0 {
                    continuation.yield(AnalyzerInput(buffer: converted))
                }
            }
            tapped = true
            engine.prepare()
            try engine.start()
            listening = true
            emit(["type": "started"])
            last = try await analyzer.analyzeSequence(stream)
        }
        if let last { try await analyzer.finalizeAndFinish(through: last) }
        else { results?.cancel(); await analyzer.cancelAndFinishNow() }
        await results?.value
        if !finished {
            emit(["type": "transcript", "text": transcript, "isFinal": true])
            finish()
        }
    }
}

let dictation: any VoiceSession
if #available(macOS 26.0, *) { dictation = AnalyzerDictation() }
else { dictation = Dictation() }
DispatchQueue.global(qos: .userInitiated).async {
    while let line = readLine() {
        DispatchQueue.main.async {
            if line == "stop" { dictation.stop() }
            if line == "cancel" { dictation.finish() }
        }
    }
    DispatchQueue.main.async { dictation.finish() }
}
dictation.start()
RunLoop.main.run()
