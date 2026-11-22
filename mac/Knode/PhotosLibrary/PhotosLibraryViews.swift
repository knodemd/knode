import SwiftUI

extension EnvironmentValues {
    /// Where the Photos library source keeps its options: Knode Service.
    @Entry var photosLibraryClient: any PhotosLibraryClient = ServicePhotosLibraryClient()
}

/// Whether originals kept only in iCloud are downloaded, for one space: `photos_library.options`
/// and `photos_library.configure`. Shown in the Photos library's card in Settings > Sources.
struct PhotosLibraryOptionsRows: View {
    let space: URL
    @Environment(\.photosLibraryClient) private var client
    @State private var icloud = false
    @State private var loaded = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: KSpace.x2) {
            KSettingToggle("Download from iCloud",
                           detail: "Off: a photo or video kept only in iCloud waits until this Mac has its original. On: Knode downloads it.",
                           isOn: Binding(get: { icloud }, set: { on in
                               let before = icloud
                               icloud = on
                               Task {
                                   do {
                                       icloud = try await client.setICloud(on, in: space)
                                       error = nil
                                   } catch {
                                       icloud = before
                                       self.error = error.localizedDescription
                                   }
                               }
                           }))
            if let error { KErrorText(error) }
        }
        .disabled(!loaded)
        .task(id: space) {
            loaded = false
            do {
                icloud = try await client.icloud(in: space)
                error = nil
            } catch {
                self.error = error.localizedDescription
            }
            loaded = true
        }
    }
}
