import SwiftUI

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @Bindable var store: ClipStore
    @State private var urlText: String = ""
    @State private var validationError: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("https://tidepool.example.ts.net", text: $urlText)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .textContentType(.URL)
                } header: {
                    Text("Server URL")
                } footer: {
                    if let validationError {
                        Text(validationError).foregroundStyle(.red)
                    } else {
                        Text("Tidepool server URL on your tailnet.")
                    }
                }

                Section {
                    Button("Reset to default") {
                        urlText = Settings.defaultServerURL.absoluteString
                        validationError = nil
                    }
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { save() }
                }
            }
            .onAppear {
                urlText = store.serverURL.absoluteString
            }
        }
    }

    private func save() {
        let trimmed = urlText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed),
              let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              let host = url.host, !host.isEmpty
        else {
            validationError = "Enter a valid URL with an http(s) scheme and host."
            return
        }
        store.updateServerURL(url)
        dismiss()
    }
}
