import AVFoundation
import SwiftUI
import UIKit
import UniformTypeIdentifiers
import VisionKit

/// The paperclip's menu, as in Apple Notes: Attach File and Record Audio,
/// then the camera and photo items. The keyboard bar and the note's bottom
/// bar both show it.
enum AttachKind: CaseIterable {
    case file, audio, photoLibrary, camera, scanDocuments, scanText

    var title: String {
        switch self {
        case .file: "Attach File"
        case .audio: "Record Audio"
        case .photoLibrary: "Choose Photo or Video"
        case .camera: "Take Photo or Video"
        case .scanDocuments: "Scan Documents"
        case .scanText: "Scan Text"
        }
    }

    var symbol: String {
        switch self {
        case .file: "doc"
        case .audio: "waveform"
        case .photoLibrary: "photo.on.rectangle"
        case .camera: "camera"
        case .scanDocuments: "doc.viewfinder"
        case .scanText: "text.viewfinder"
        }
    }

    /// Whether this device can do it (no camera in the Simulator, …).
    var available: Bool {
        switch self {
        case .camera: UIImagePickerController.isSourceTypeAvailable(.camera)
        case .scanDocuments: VNDocumentCameraViewController.isSupported
        case .scanText: UIImagePickerController.isSourceTypeAvailable(.camera)
        default: true
        }
    }

    /// The menu's two groups.
    static let groups: [[AttachKind]] = [[.file, .audio], [.photoLibrary, .camera, .scanDocuments, .scanText]]
}

enum Attaching {
    /// The menu for UIKit (the keyboard bar).
    static func menu(_ run: @escaping (AttachKind) -> Void) -> UIMenu {
        UIMenu(children: AttachKind.groups.map { group in
            UIMenu(options: .displayInline, children: group.map { kind in
                UIAction(title: kind.title, image: UIImage(systemName: kind.symbol), attributes: kind.available ? [] : [.disabled]) { _ in run(kind) }
            })
        })
    }

    /// A copy of `url` in a folder of its own in the temporary folder,
    /// called `name` (the name the attachment shows).
    static func temporaryCopy(of url: URL, named name: String) -> URL? {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let copy = dir.appendingPathComponent(name.replacingOccurrences(of: "/", with: "-"))
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try FileManager.default.copyItem(at: url, to: copy)
            return copy
        } catch {
            return nil
        }
    }

    /// "17 Nov 2026 at 20.41", for scans' and recordings' names.
    static func stamp() -> String {
        let f = DateFormatter()
        f.dateFormat = "d MMM yyyy 'at' HH.mm"
        return f.string(from: Date())
    }
}

/// The paperclip's menu for SwiftUI (the note's bottom bar).
struct AttachMenu: View {
    let run: (AttachKind) -> Void

    var body: some View {
        Menu {
            ForEach(Array(AttachKind.groups.enumerated()), id: \.offset) { _, group in
                Section {
                    ForEach(group, id: \.self) { kind in
                        Button { run(kind) } label: { Label(kind.title, systemImage: kind.symbol) }
                            .disabled(!kind.available)
                    }
                }
            }
        } label: {
            Image(systemName: "paperclip")
        }
        .menuOrder(.fixed)
        .accessibilityLabel("Attach")
    }
}

extension EditorBridge {
    func attach(_ kind: AttachKind) { coordinator?.attach(kind) }
}

extension NoteEditorView.Coordinator: UIImagePickerControllerDelegate, UINavigationControllerDelegate {
    func attach(_ kind: AttachKind) {
        if readOnly {
            blockedEdit()
            return
        }
        switch kind {
        case .file: fileTapped()
        case .photoLibrary: photoTapped()
        case .camera: takePhoto()
        case .scanDocuments: scanDocumentsTapped()
        case .scanText: scanText()
        case .audio: recordAudio()
        }
    }

    // ---- Take Photo or Video ----

    private func takePhoto() {
        guard UIImagePickerController.isSourceTypeAvailable(.camera) else { return }
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = [UTType.image.identifier, UTType.movie.identifier]
        picker.delegate = self
        topController()?.present(picker, animated: true)
    }

    func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
        picker.dismiss(animated: true)
        if let image = info[.originalImage] as? UIImage, let data = image.jpegData(compressionQuality: 0.9) {
            attachImage(data)
        } else if let url = info[.mediaURL] as? URL,
                  let copy = Attaching.temporaryCopy(of: url, named: "Video \(Attaching.stamp()).\(url.pathExtension)") {
            attachFile(copy)
            try? FileManager.default.removeItem(at: copy.deletingLastPathComponent())
        }
    }

    func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
        picker.dismiss(animated: true)
    }

    // ---- Scan Text: Live Text types what the camera reads at the caret ----

    private func scanText() {
        guard let tv = textView else { return }
        if !tv.isFirstResponder { tv.becomeFirstResponder() }
        tv.captureTextFromCamera(nil)
    }

    // ---- Record Audio ----

    private func recordAudio() {
        let recorder = UIHostingController(rootView: AudioRecorderView { [weak self] url in
            guard let self else { return }
            self.topController()?.dismiss(animated: true)
            if let url {
                self.attachFile(url)
                try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
            }
        })
        if let sheet = recorder.sheetPresentationController {
            sheet.detents = [.medium()]
            sheet.prefersGrabberVisible = true
        }
        topController()?.present(recorder, animated: true)
    }
}

/// Records a voice memo to attach: the time, a big record/stop button, and
/// Cancel. Calls `done` with the recording (nil when cancelled).
struct AudioRecorderView: View {
    let done: (URL?) -> Void

    @State private var recorder: AVAudioRecorder?
    @State private var url: URL?
    @State private var elapsed: TimeInterval = 0
    @State private var denied = false
    private let tick = Timer.publish(every: 0.25, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(spacing: 24) {
            HStack {
                Button("Cancel") { cancel() }
                Spacer()
            }
            Spacer()
            Text(denied ? "Notes can't use the microphone. Allow it in Settings → Privacy & Security → Microphone." : "New Recording")
                .font(denied ? .body : .headline)
                .multilineTextAlignment(.center)
                .foregroundStyle(denied ? .secondary : .primary)
            Text(Duration.seconds(elapsed).formatted(.time(pattern: .minuteSecond)))
                .font(.system(size: 48, weight: .light, design: .rounded).monospacedDigit())
            Button {
                recorder == nil ? start() : stop()
            } label: {
                ZStack {
                    Circle().strokeBorder(Color.primary.opacity(0.4), lineWidth: 3).frame(width: 72, height: 72)
                    if recorder == nil {
                        Circle().fill(.red).frame(width: 58, height: 58)
                    } else {
                        RoundedRectangle(cornerRadius: 6).fill(.red).frame(width: 28, height: 28)
                    }
                }
            }
            .disabled(denied)
            .accessibilityLabel(recorder == nil ? "Record" : "Stop and Attach")
            Spacer()
        }
        .padding()
        .onReceive(tick) { _ in
            if let r = recorder, r.isRecording { elapsed = r.currentTime }
        }
        .onDisappear {
            // Swiped away: nothing is attached.
            if recorder != nil { cancel() }
        }
    }

    private func start() {
        AVAudioApplication.requestRecordPermission { granted in
            DispatchQueue.main.async {
                guard granted else {
                    denied = true
                    return
                }
                let session = AVAudioSession.sharedInstance()
                let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
                let file = dir.appendingPathComponent("Recording \(Attaching.stamp()).m4a")
                let settings: [String: Any] = [
                    AVFormatIDKey: kAudioFormatMPEG4AAC,
                    AVSampleRateKey: 44100,
                    AVNumberOfChannelsKey: 1,
                    AVEncoderAudioQualityKey: AVAudioQuality.high.rawValue,
                ]
                do {
                    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                    try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker])
                    try session.setActive(true)
                    let r = try AVAudioRecorder(url: file, settings: settings)
                    r.record()
                    recorder = r
                    url = file
                } catch {
                    denied = false
                    done(nil)
                }
            }
        }
    }

    private func stop() {
        recorder?.stop()
        recorder = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        done(url)
    }

    private func cancel() {
        recorder?.stop()
        recorder = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        if let url { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        url = nil
        done(nil)
    }
}
